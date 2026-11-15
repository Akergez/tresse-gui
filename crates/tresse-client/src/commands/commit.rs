use tresse_lib::VersionLabel;
use tresse_lib::commit::{CommitOptions, CommitOutcome, NoJournal, commit};
use tresse_lib::operation::{OperationKind, Step, StepKind};
use tresse_lib::partial_sync::pull_remote_content;

use crate::Repo;
use crate::cas::{
    ConfiguredRemote, DEFAULT_CHUNK_RAW_SIZE, DEFAULT_MAX_OBJECT_SIZE, EmptyRemote,
    load_merged_meta, open_local_store,
};
use crate::config::TresseConfig;
use crate::fs_writer::{VaultSource, build_shape};

pub fn run(repo: &Repo) -> Result<(), Box<dyn std::error::Error>> {
    let vault = repo.vault();
    match vault.metadata(".tresse")? {
        Some(meta) if meta.is_dir => {}
        _ => return Err("not a tresse repository; run `tresse init` first".into()),
    }

    // До чтения директории: оборванная до нас операция могла оставить рабочую
    // директорию позади проекции, и коммит принял бы недоехавшие файлы за
    // удалённые.
    let mut operation =
        crate::operation::begin(repo, OperationKind::Commit, [Step::new(StepKind::Commit)])?;
    operation.start(StepKind::Commit)?;
    let config = TresseConfig::load(vault.as_ref())?;
    let mut local = open_local_store(repo)?;
    let meta = load_merged_meta(&local)?;

    // The same rule the terminal's commit uses: with versions committed, the
    // file index says which paths are unchanged, and only the rest is read.
    // A path the index has never seen — the first commit, or a new file — is
    // read to learn whether it is text; its nature is then remembered.
    let ignore = config.ignore_rules()?;
    let previous_index = if meta.versions.is_empty() {
        None
    } else {
        crate::file_index::load(vault.as_ref())
    };
    let (skel, current_index) = build_shape(vault.as_ref(), &ignore, previous_index.as_ref())?;
    let disk_source = VaultSource::new(vault.clone());

    let missing_base_content =
        tresse_lib::partial_sync::content_fetch_plan_paths(&meta, &skel.changed_paths(), &local)?;
    if !missing_base_content.is_empty() {
        let remote_config = config
            .origin()
            .ok_or("local content is incomplete and no remote is configured")?;
        let repo_key = tresse_lib::crypto::decode_key_b64(&remote_config.encryption_key)?;
        let remote = ConfiguredRemote::open(remote_config)?;
        pull_remote_content(&mut local, &remote, &repo_key, &missing_base_content)?;
    }

    let outcome = commit(
        &meta,
        &skel,
        &disk_source,
        &mut local,
        &EmptyRemote,
        CommitOptions {
            chunk_raw_size: DEFAULT_CHUNK_RAW_SIZE,
            max_object_size: DEFAULT_MAX_OBJECT_SIZE,
        },
        &mut NoJournal,
    )?;
    match outcome {
        CommitOutcome::NoChange => println!("no changes detected"),
        CommitOutcome::Committed {
            version,
            root_object,
            ..
        } => {
            println!(
                "committed version {} ({})",
                VersionLabel::from_source(version.source).format(),
                &root_object[..12]
            );
        }
    }
    crate::file_index::save(vault.as_ref(), &current_index)?;
    operation.finish(StepKind::Commit)?;
    operation.complete()?;
    Ok(())
}
