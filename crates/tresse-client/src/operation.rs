//! Блокировка репозитория и запись операции на диске.
//!
//! Формат записи и решение, что доводить после обрыва, принадлежат домену —
//! [`tresse_lib::operation`]. Здесь то, что знает про хранилище и процессы:
//! блокировка ОС на `.tresse/lock.os`, отметка «жив» для фронтендов без неё и
//! атомарная запись файла операции.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tresse_lib::layout::{RESTORE_TEMP_DIR, TRESSE_DIR};
use tresse_lib::operation::{
    HEARTBEAT_INTERVAL_MS, LOCK_PATH, LockRecord, Operation, OperationKind, PATH, Resume, Step,
    StepKind, resume_unreadable,
};

use crate::Repo;
use crate::config::TresseConfig;
use crate::vault::{Vault, VaultLock};
use crate::worktree::RestoreOutcome;

type Failure = Box<dyn std::error::Error>;

const APP: &str = "tresse-gui";

/// A local edit of the worktree needs the shared lock and recovery, but no
/// commit journal: recovery must never turn an ordinary save into a version.
pub(crate) fn edit_worktree(
    repo: &Repo,
    write: impl FnOnce() -> Result<(), Failure>,
) -> Result<(), Failure> {
    let vault = repo.vault();
    match vault.metadata(TRESSE_DIR)? {
        Some(meta) if meta.is_dir => {}
        _ => return Err("not a tresse repository; run `tresse init` first".into()),
    }
    let _lock = RepoLock::acquire(repo)?;
    settle(repo)?;
    write()
}

/// Блокировка репозитория на время одной операции.
///
/// Блокировку ОС отпускает сама система, когда процесс умирает, — зависших
/// блокировок от CLI не бывает. Отметка времени в соседнем файле нужна тем, кто
/// блокировку ОС взять не может: по ней плагин узнаёт, что клиент ещё работает.
pub struct RepoLock {
    vault: Arc<dyn Vault>,
    _os_lock: Box<dyn VaultLock>,
    stop: Option<Sender<()>>,
    heartbeat: Option<JoinHandle<()>>,
}

impl RepoLock {
    pub fn acquire(repo: &Repo) -> Result<Self, Failure> {
        let vault = repo.vault().clone();
        let os_lock = match vault.lock() {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(busy(read_record(vault.as_ref())).into());
            }
            Err(error) => return Err(error.into()),
        };
        // Блокировка ОС наша, но плагин её не берёт: его слышно только по отметке.
        if let Some(owner) = read_record(vault.as_ref())
            && !owner.os_lock
            && owner.is_fresh(now_ms())
        {
            return Err(busy(Some(owner)).into());
        }

        write_record(vault.as_ref())?;
        let beating = vault.clone();
        let (stop, stopped) = mpsc::channel::<()>();
        let heartbeat = std::thread::spawn(move || {
            let interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);
            while stopped.recv_timeout(interval) == Err(RecvTimeoutError::Timeout) {
                let _ = write_record(beating.as_ref());
            }
        });
        Ok(Self {
            vault,
            _os_lock: os_lock,
            stop: Some(stop),
            heartbeat: Some(heartbeat),
        })
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        // Пустой файл — «свободно»: плагину не придётся ждать, пока истечёт аренда.
        let _ = self.vault.write(LOCK_PATH, b"");
    }
}

fn read_record(vault: &dyn Vault) -> Option<LockRecord> {
    LockRecord::from_json(&vault.read(LOCK_PATH).ok()?)
}

fn write_record(vault: &dyn Vault) -> std::io::Result<()> {
    let bytes = LockRecord {
        app: APP.to_string(),
        pid: Some(std::process::id()),
        os_lock: true,
        heartbeat_ms: now_ms(),
        nonce: None,
    }
    .to_json();
    vault.write(LOCK_PATH, &bytes)?;
    Ok(())
}

fn busy(owner: Option<LockRecord>) -> String {
    match owner {
        Some(LockRecord {
            app,
            pid: Some(pid),
            ..
        }) => format!("another tresse operation is running ({app}, pid {pid})"),
        Some(LockRecord { app, .. }) => format!("another tresse operation is running ({app})"),
        None => "another tresse operation is running".to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Идущая операция: блокировка плюс её запись на диске.
///
/// Запись остаётся лежать, если операция не дошла до [`RunningOperation::complete`], —
/// упала, была убита или вернула ошибку — либо дошла, но рабочую директорию
/// оставила позади проекции. Разницы нет: следующая операция, работающая с
/// файлами, сначала перельёт то, что за записью осталось.
pub struct RunningOperation {
    repo: Repo,
    operation: Operation,
    /// Что перелито в рабочую директорию за прежней записью, если было что.
    pub settled: Option<RestoreOutcome>,
    _lock: RepoLock,
}

/// Взять блокировку, перелить оставшееся за прежней записью и начать свою.
///
/// Для операций, которые читают или пишут рабочую директорию: commit, restore.
/// Коммит, начатый поверх отставшей директории, принял бы недоехавшие файлы за
/// удалённые.
pub fn begin(
    repo: &Repo,
    kind: OperationKind,
    steps: impl IntoIterator<Item = Step>,
) -> Result<RunningOperation, Failure> {
    let lock = RepoLock::acquire(repo)?;
    let settled = settle(repo)?;
    start(repo, lock, Operation::new(kind, steps), settled)
}

/// Взять блокировку и начать свою операцию, забрав долг прежней записи себе.
///
/// Для операций, которые рабочую директорию не трогают: sync. Она делает ровно
/// то, что названо, и файлов не переливает — но и долг не теряет.
pub fn begin_carrying(
    repo: &Repo,
    kind: OperationKind,
    steps: impl IntoIterator<Item = Step>,
) -> Result<RunningOperation, Failure> {
    let lock = RepoLock::acquire(repo)?;
    let mut operation = Operation::new(kind, steps);
    if let Some((_, left)) = read_left(repo)? {
        operation.carry(left);
    }
    start(repo, lock, operation, None)
}

fn start(
    repo: &Repo,
    lock: RepoLock,
    operation: Operation,
    settled: Option<RestoreOutcome>,
) -> Result<RunningOperation, Failure> {
    // Обломки оборванного restore: под блокировкой они ничьи.
    let _ = repo
        .vault()
        .remove_dir_all(&format!("{TRESSE_DIR}/{RESTORE_TEMP_DIR}"));
    let running = RunningOperation {
        repo: repo.clone(),
        operation,
        settled,
        _lock: lock,
    };
    running.save()?;
    Ok(running)
}

impl RunningOperation {
    pub fn start(&mut self, step: StepKind) -> Result<(), Failure> {
        self.operation.start(step);
        self.save()?;
        Ok(())
    }

    pub fn finish(&mut self, step: StepKind) -> Result<(), Failure> {
        self.operation.finish(step);
        self.save()
    }

    pub fn finish_with_restore_paths(
        &mut self,
        step: StepKind,
        paths: Vec<String>,
    ) -> Result<(), Failure> {
        self.operation.finish_with_restore_paths(step, paths);
        self.save()
    }

    /// Операция дошла до конца: запись больше не нужна.
    pub fn complete(self) -> Result<(), Failure> {
        remove_record(&self.repo)
    }

    /// Операция дошла до конца, но могла оставить рабочую директорию позади:
    /// запись убирается, только если за ней ничего не числится.
    pub fn complete_or_leave(self) -> Result<(), Failure> {
        match self.operation.resume() {
            Resume::Nothing => remove_record(&self.repo),
            _ => Ok(()),
        }
    }

    /// Ждёт ли рабочая директория переливки за этой записью.
    pub fn leaves_restore(&self) -> bool {
        self.operation.resume() != Resume::Nothing
    }

    fn save(&self) -> Result<(), Failure> {
        let bytes = self.operation.to_json()?;
        self.repo.vault().write(PATH, &bytes)?;
        Ok(())
    }
}

fn remove_record(repo: &Repo) -> Result<(), Failure> {
    repo.vault().remove_file(PATH)?;
    Ok(())
}

/// Что осталось за прежней записью и как звалась её операция.
fn read_left(repo: &Repo) -> Result<Option<(&'static str, Resume)>, Failure> {
    let bytes = match repo.vault().read(PATH) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(match Operation::from_json(&bytes) {
        Ok(operation) => (operation.kind.name(), operation.resume()),
        Err(_) => ("operation", resume_unreadable()),
    }))
}

/// Перелить в рабочую директорию то, что осталось за прежней записью.
///
/// Вызывается под блокировкой: раз она наша, прежняя операция уже не идёт, и
/// её запись — всё, что от неё осталось. Оборвали её или она сама оставила
/// переливку следующему (sync), здесь неважно.
fn settle(repo: &Repo) -> Result<Option<RestoreOutcome>, Failure> {
    let Some((name, left)) = read_left(repo)? else {
        return Ok(None);
    };
    let settled = match left {
        Resume::Nothing => Ok(None),
        Resume::RestoreLatest { paths } => {
            eprintln!("restoring what {name} left for the working tree");
            settle_latest(repo, paths).map(Some)
        }
        Resume::RestoreVersion { version, then } => {
            eprintln!("finishing interrupted {name}: restoring version {version}");
            settle_version(repo, &version).and_then(|restored| match *then {
                // Поверх оборванного restore прошёл sync: что он скачал, версия
                // не знает, и это переливается следом.
                Resume::RestoreLatest { paths } => settle_latest(repo, paths).map(Some),
                _ => Ok(restored),
            })
        }
    };
    let settled = settled.map_err(|error| {
        format!(
            "cannot restore what {name} left: {error}\n\
             (if this can never succeed, `tresse abort` shows what would be given up)"
        )
    })?;
    remove_record(repo)?;
    Ok(settled)
}

fn settle_latest(repo: &Repo, paths: Option<Vec<String>>) -> Result<RestoreOutcome, Failure> {
    let config = TresseConfig::load(repo.vault().as_ref())?;
    let mut local = crate::cas::open_local_store(repo)?;
    let paths = paths.map(|paths| paths.into_iter().collect());
    let outcome = crate::worktree::restore_latest(repo, &config, &mut local, paths.as_ref())?;
    outcome.report_skipped();
    Ok(outcome)
}

/// Довести `restore <версия>`, оборванный до нас.
///
/// С охраной, хотя сам restore к версии её не знает: между обрывом и доводкой
/// могло пройти сколько угодно времени, и всё, что пользователь успел за него
/// изменить, просьбой перезаписать уже не покрыто.
fn settle_version(repo: &Repo, version: &str) -> Result<Option<RestoreOutcome>, Failure> {
    let config = TresseConfig::load(repo.vault().as_ref())?;
    let mut local = crate::cas::open_local_store(repo)?;
    let meta = crate::cas::load_merged_meta(&local)?;
    if crate::cas::resolve_version(&meta, version).is_err() {
        // Версии нет — значит, restore к ней ничего и не успел.
        return Ok(None);
    }
    let outcome = crate::worktree::restore_version(repo, &config, &mut local, version, true)?;
    outcome.report_skipped();
    Ok(Some(outcome))
}
