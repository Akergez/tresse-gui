//! Running a folder's sync in the background, and what the screens say while
//! it runs.
//!
//! The client's calls block — they read the folder, talk to the storage and
//! write files back — so each step runs on a background executor and the
//! screen is told which step is running. There are no callbacks inside a
//! step; the phase moves between steps.
//!
//! A folder is either idle, running one of the three steps, done, or failed.
//! A sync of a folder already running is refused, not queued: the person sees
//! it is busy.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::{App, AppContext as _, AsyncApp, Context, Entity, Global, SharedString};
use tresse_client::Repo;

use crate::settings::{RepoRecord, Settings};

/// Which step of a sync a folder is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// Reading the folder into a version.
    Committing,
    /// Exchanging objects with the storage.
    Exchanging,
    /// Writing what arrived back into the folder.
    Applying,
    Done,
    Failed,
}

impl Phase {
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Phase::Committing | Phase::Exchanging | Phase::Applying
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Idle => "Not synced yet",
            Phase::Committing => "Reading the folder…",
            Phase::Exchanging => "Exchanging with the storage…",
            Phase::Applying => "Applying changes…",
            Phase::Done => "Synced",
            Phase::Failed => "Sync failed",
        }
    }
}

/// What a folder's last sync did, or what it is doing now.
#[derive(Clone, Default)]
pub struct Status {
    pub phase: Phase,
    pub pulled: usize,
    pub pushed: usize,
    pub error: Option<SharedString>,
    /// When the last attempt finished, either way (unix milliseconds).
    pub finished_at: Option<u64>,
}

impl Status {
    pub fn summary(&self) -> Option<SharedString> {
        match self.phase {
            Phase::Done => Some(format!("{} received, {} sent", self.pulled, self.pushed).into()),
            _ => None,
        }
    }
}

/// The states of every folder in this session.
pub struct SyncEngine {
    states: HashMap<String, Status>,
}

impl SyncEngine {
    pub fn install(cx: &mut App) -> Entity<Self> {
        let engine = cx.new(|_| SyncEngine {
            states: HashMap::new(),
        });
        cx.set_global(Engine(engine.clone()));
        engine
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<Engine>().0.clone()
    }

    pub fn status(&self, path: &str) -> Status {
        self.states.get(path).cloned().unwrap_or_default()
    }

    fn begin(&mut self, path: &str, phase: Phase) {
        self.states.insert(
            path.to_string(),
            Status {
                phase,
                ..Default::default()
            },
        );
    }

    fn phase(&mut self, path: &str, phase: Phase) {
        self.states.entry(path.to_string()).or_default().phase = phase;
    }

    fn done(&mut self, path: &str, pulled: usize, pushed: usize, at: u64) {
        let status = self.states.entry(path.to_string()).or_default();
        status.phase = Phase::Done;
        status.pulled = pulled;
        status.pushed = pushed;
        status.error = None;
        status.finished_at = Some(at);
    }

    fn failed(&mut self, path: &str, error: String, at: u64) {
        let status = self.states.entry(path.to_string()).or_default();
        status.phase = Phase::Failed;
        status.error = Some(error.into());
        status.finished_at = Some(at);
    }
}

impl Global for Engine {}

struct Engine(Entity<SyncEngine>);

/// Sync the folder `record` names: read it, exchange with the storage, apply
/// what arrived. Does nothing while a sync of the same folder runs.
pub fn sync<T: 'static>(engine: &Entity<SyncEngine>, record: RepoRecord, cx: &mut Context<T>) {
    start(engine, record, Step::Sync, cx);
}

/// Apply what earlier syncs downloaded, without sending anything new.
pub fn apply<T: 'static>(engine: &Entity<SyncEngine>, record: RepoRecord, cx: &mut Context<T>) {
    start(engine, record, Step::Apply, cx);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Sync,
    Apply,
}

fn start<T: 'static>(
    engine: &Entity<SyncEngine>,
    record: RepoRecord,
    step: Step,
    cx: &mut Context<T>,
) {
    let key = record.path.clone();
    if engine.read(cx).status(&key).phase.is_running() {
        return;
    }
    engine.update(cx, |engine, cx| {
        engine.begin(&key, Phase::Committing);
        cx.notify();
    });
    let engine = engine.clone();
    cx.spawn(async move |_, cx| match step {
        Step::Sync => run_sync(engine, record, cx).await,
        Step::Apply => run_apply(engine, record, cx).await,
    })
    .detach();
}

async fn open(record: RepoRecord, cx: &mut AsyncApp) -> Result<Repo, String> {
    cx.background_spawn({
        let record = record.clone();
        async move { crate::folders::open(&record) }
    })
    .await
}

async fn run_sync(engine: Entity<SyncEngine>, record: RepoRecord, cx: &mut AsyncApp) {
    let key = record.path.clone();
    let repo = match open(record, cx).await {
        Ok(repo) => repo,
        Err(error) => return fail(&engine, &key, error, cx),
    };

    tracing::info!(folder = %key, "reading the folder");
    let committed = cx
        .background_spawn({
            let repo = repo.clone();
            async move { tresse_client::commit(&repo) }
        })
        .await;
    if let Err(error) = committed {
        return fail(&engine, &key, error, cx);
    }
    tracing::info!(folder = %key, "the folder is read");

    engine.update(cx, |engine, cx| {
        engine.phase(&key, Phase::Exchanging);
        cx.notify();
    });
    tracing::info!(folder = %key, "exchanging with the storage");
    let exchanged = cx
        .background_spawn({
            let repo = repo.clone();
            async move { tresse_client::exchange(&repo) }
        })
        .await;
    let (pulled, pushed) = match exchanged {
        Ok(counts) => counts,
        Err(error) => return fail(&engine, &key, error, cx),
    };
    tracing::info!(folder = %key, pulled, pushed, "exchanged");

    engine.update(cx, |engine, cx| {
        engine.phase(&key, Phase::Applying);
        cx.notify();
    });
    tracing::info!(folder = %key, "applying what arrived");
    let settled = cx
        .background_spawn({
            let repo = repo.clone();
            async move { tresse_client::settle(&repo) }
        })
        .await;
    match settled {
        Ok(()) => finish(&engine, &key, pulled, pushed, cx),
        Err(error) => fail(&engine, &key, error, cx),
    }
}

async fn run_apply(engine: Entity<SyncEngine>, record: RepoRecord, cx: &mut AsyncApp) {
    let key = record.path.clone();
    let repo = match open(record, cx).await {
        Ok(repo) => repo,
        Err(error) => return fail(&engine, &key, error, cx),
    };
    engine.update(cx, |engine, cx| {
        engine.phase(&key, Phase::Applying);
        cx.notify();
    });
    let settled = cx
        .background_spawn({
            let repo = repo.clone();
            async move { tresse_client::settle(&repo) }
        })
        .await;
    match settled {
        Ok(()) => finish(&engine, &key, 0, 0, cx),
        Err(error) => fail(&engine, &key, error, cx),
    }
}

fn finish(engine: &Entity<SyncEngine>, key: &str, pulled: usize, pushed: usize, cx: &mut AsyncApp) {
    let at = now_ms();
    let key = key.to_string();
    engine.update(cx, |engine, cx| {
        engine.done(&key, pulled, pushed, at);
        Settings::update(cx, |settings| settings.note_synced(&key, at));
        cx.notify();
    });
    tracing::info!(folder = %key, pulled, pushed, "synced");
}

fn fail(engine: &Entity<SyncEngine>, key: &str, error: String, cx: &mut AsyncApp) {
    let at = now_ms();
    tracing::warn!(folder = %key, %error, "sync failed");
    let key = key.to_string();
    engine.update(cx, |engine, cx| {
        engine.failed(&key, error, at);
        cx.notify();
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The clock a person reads, for "last synced at".
pub fn clock(at: u64) -> String {
    chrono::DateTime::from_timestamp_millis(at as i64)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}
