//! Приведение рабочей директории к Tresse.
//!
//! Два направления, и различаются они тем, кто попросил. К последней проекции
//! рабочую директорию приводит сама операция (sync, доводка оборванной) — и
//! потому не трогает то, что пользователь изменил. К заказанной версии её
//! приводит пользователь, и там перезапись — сама просьба.

use std::collections::HashSet;

use tresse_lib::commit::merge_local_meta_as_of;
use tresse_lib::partial_sync::{content_fetch_plan, content_fetch_plan_paths, pull_remote_content};
use tresse_lib::restore::{restore, restore_paths};
use tresse_lib::{FileIndex, IgnoreRules};

use crate::Repo;
use crate::cas::{
    ConfiguredRemote, EmptyRemote, VaultObjectStore, load_merged_meta, load_meta_entries,
    resolve_version,
};
use crate::config::TresseConfig;
use crate::restore_sink::VaultRestoreSink;
use crate::vault::{Vault, join};

type Failure = Box<dyn std::error::Error>;

pub struct RestoreOutcome {
    pub restored: usize,
    pub removed: usize,
    /// Пути, которые пользователь изменил и которые поэтому остались как есть.
    pub skipped: Vec<String>,
}

impl RestoreOutcome {
    pub fn report_skipped(&self) {
        for path in &self.skipped {
            eprintln!("  kept local changes: {path}");
        }
        if !self.skipped.is_empty() {
            eprintln!(
                "{} locally modified path(s) were left untouched: the incoming change was not \
                 applied to them, and the next commit records the local content over it",
                self.skipped.len()
            );
        }
    }
}

/// Перелить последнюю проекцию: все пути или только перечисленные.
pub fn restore_latest(
    repo: &Repo,
    config: &TresseConfig,
    local: &mut VaultObjectStore,
    paths: Option<&HashSet<String>>,
) -> Result<RestoreOutcome, Failure> {
    let merged = load_merged_meta(local)?;
    let missing = match paths {
        Some(paths) => content_fetch_plan_paths(&merged, paths, local)?,
        None => content_fetch_plan(&merged, local)?,
    };
    pull_missing_content(config, local, &missing)?;

    let vault = repo.vault();
    let index = crate::file_index::load(vault.as_ref()).unwrap_or_default();
    let mut sink = VaultRestoreSink::guarded(vault.clone(), config.ignore_rules()?, index);
    match paths {
        Some(paths) => restore_paths(&merged, paths, local, &EmptyRemote, &mut sink)?,
        None => restore(&merged, local, &EmptyRemote, &mut sink)?,
    };
    let changes = sink.into_changes();
    crate::file_index::apply_restore(vault.as_ref(), &changes.written, &changes.removed)?;

    Ok(RestoreOutcome {
        restored: changes.written.len(),
        removed: changes.removed.len(),
        skipped: changes.skipped,
    })
}

/// Привести рабочую директорию к версии: записать её файлы и убрать остальное.
///
/// Сначала запись, потом уборка. Оборванный на полпути restore оставляет
/// директорию между двумя состояниями, но не пустой — и доводится повтором.
///
/// `guarded` — для доводки: не трогать ни при записи, ни при уборке то, что
/// изменили после прошлого коммита или restore.
pub fn restore_version(
    repo: &Repo,
    config: &TresseConfig,
    local: &mut VaultObjectStore,
    version_spec: &str,
    guarded: bool,
) -> Result<RestoreOutcome, Failure> {
    let latest = load_merged_meta(local)?;
    let target = resolve_version(&latest, version_spec)?;
    let meta = merge_local_meta_as_of(load_meta_entries(local)?, target.source)?;

    let missing = content_fetch_plan(&meta, local)?;
    pull_missing_content(config, local, &missing)?;

    let keep = config.ignore_rules()?;
    let vault = repo.vault();
    let index = crate::file_index::load(vault.as_ref()).unwrap_or_default();
    let mut sink = if guarded {
        VaultRestoreSink::guarded(vault.clone(), keep.clone(), index.clone())
    } else {
        VaultRestoreSink::forced(vault.clone(), keep.clone())
    };
    restore(&meta, local, &EmptyRemote, &mut sink)?;
    let mut changes = sink.into_changes();

    let wanted: HashSet<&str> = changes
        .written
        .iter()
        .map(|file| file.path.as_str())
        .chain(changes.skipped.iter().map(String::as_str))
        .collect();
    let mut sweep = Sweep {
        vault: vault.as_ref(),
        ignore: &keep,
        wanted: &wanted,
        guard: guarded.then_some(&index),
        removed: Vec::new(),
        kept: Vec::new(),
    };
    sweep.directory("")?;
    let Sweep { removed, kept, .. } = sweep;
    changes.removed.extend(removed);
    changes.skipped.extend(kept);
    crate::file_index::apply_restore(vault.as_ref(), &changes.written, &changes.removed)?;

    Ok(RestoreOutcome {
        restored: changes.written.len(),
        removed: changes.removed.len(),
        skipped: changes.skipped,
    })
}

fn pull_missing_content(
    config: &TresseConfig,
    local: &mut VaultObjectStore,
    missing: &[String],
) -> Result<(), Failure> {
    if missing.is_empty() {
        return Ok(());
    }
    let remote_config = config
        .origin()
        .ok_or("restore requires missing content but no remote is configured")?;
    let repo_key = tresse_lib::crypto::decode_key_b64(&remote_config.encryption_key)?;
    let remote = ConfiguredRemote::open(remote_config)?;
    pull_remote_content(local, &remote, &repo_key, missing)?;
    Ok(())
}

/// Уборка после restore к версии: всё, чего в версии нет, из директории уходит.
struct Sweep<'a> {
    vault: &'a dyn Vault,
    ignore: &'a IgnoreRules,
    wanted: &'a HashSet<&'a str>,
    /// С охраной уходит только то, что индекс признаёт нетронутым.
    guard: Option<&'a FileIndex>,
    removed: Vec<String>,
    kept: Vec<String>,
}

impl Sweep<'_> {
    /// Убрать лишнее из каталога. Возвращает, пуст ли он теперь.
    fn directory(&mut self, dir: &str) -> std::io::Result<bool> {
        let mut empty = true;
        for entry in self.vault.list(dir)? {
            let child = join(dir, &entry.name);
            if self.ignore.is_ignored(&child, entry.is_dir) {
                empty = false;
            } else if entry.is_dir {
                if self.directory(&child)? {
                    self.vault.remove_dir(&child)?;
                } else {
                    empty = false;
                }
            } else if self.wanted.contains(child.as_str()) {
                empty = false;
            } else if self.is_foreign(&child) {
                self.kept.push(child);
                empty = false;
            } else {
                self.vault.remove_file(&child)?;
                self.removed.push(child);
            }
        }
        Ok(empty)
    }

    fn is_foreign(&self, path: &str) -> bool {
        let Some(index) = self.guard else {
            return false;
        };
        match self.vault.metadata(path) {
            Ok(Some(meta)) => !index.holds(path, meta.mtime_ms, meta.len),
            _ => true,
        }
    }
}
