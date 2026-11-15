//! The folders this application manages: the list, and adding one to it.
//!
//! A folder is a Tresse repository when it has `.tresse/` in it. Adding an
//! existing one only writes a line to `settings.json`; adding a folder that
//! has no history yet asks first, then starts one in place. Either way the
//! folder's own files — its `tresse.toml`, its remote — stay the only place
//! that describes it.

use std::rc::Rc;

use gpui_adaptive::{
    ActiveColors as _, Collection, Command, Confirm, Empty, ItemActions, ListRow, Notice, Signal,
    Tile, Tone,
};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, WeakEntity, Window, div};

use crate::folders;
use crate::settings::{RepoRecord, Settings};
use crate::sync_engine::{self, SyncEngine};
use crate::ui::Lucide;
use tresse_client::{RemoteConfig, StorageType};

/// What the list asks the window to do.
pub enum Event {
    /// Open one folder's page.
    Open(RepoRecord),
}

/// One folder as the list shows it.
struct Row {
    record: RepoRecord,
    remote: Option<RemoteConfig>,
    /// What is wrong with the folder itself: it moved, the grant is gone, or
    /// it is not (or no longer) a Tresse repository.
    problem: Option<String>,
}

impl Row {
    /// Reads what the folder says about itself. Never writes: a folder that
    /// moved is reported, not recreated.
    fn read(record: &RepoRecord) -> Self {
        let mut row = Row {
            record: record.clone(),
            remote: None,
            problem: None,
        };
        if !record.saf && !std::path::Path::new(&record.path).is_dir() {
            row.problem = Some("The folder is not there.".to_string());
            return row;
        }
        let repo = match folders::open(record) {
            Ok(repo) => repo,
            Err(error) => {
                row.problem = Some(error);
                return row;
            }
        };
        match repo.vault().metadata(".tresse") {
            Ok(Some(meta)) if meta.is_dir => {}
            Ok(_) => {
                row.problem = Some("Not a Tresse repository here.".to_string());
                return row;
            }
            Err(error) => {
                row.problem = Some(error.to_string());
                return row;
            }
        }
        match tresse_client::config::read_remote(repo.vault().as_ref()) {
            Ok(remote) => row.remote = remote,
            Err(error) => row.problem = Some(error),
        }
        row
    }
}

/// The list itself, and what can be done from it.
pub struct Repositories {
    rows: Vec<Row>,
    engine: Entity<SyncEngine>,
}

impl EventEmitter<Event> for Repositories {}

impl Repositories {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let engine = SyncEngine::global(cx);
        cx.observe(&engine, |_, _, cx| cx.notify()).detach();
        let mut repositories = Self {
            rows: Vec::new(),
            engine,
        };
        repositories.reload(cx);
        repositories
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Reads every folder again: its remote, and what the latest sync said.
    pub(crate) fn reload(&mut self, cx: &App) {
        self.rows = Settings::global(cx)
            .repositories()
            .iter()
            .map(Row::read)
            .collect();
    }

    fn contains(&self, path: &str) -> bool {
        self.rows.iter().any(|row| row.record.path == path)
    }

    /// Ask the system for a folder, then decide what kind of folder it is.
    pub fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A scripted run has no hand to answer a file dialog with: the folder
        // it is about is named when the application starts. Debug builds
        // only, so nothing ships with a way around the picker.
        #[cfg(all(not(target_os = "android"), debug_assertions))]
        if let Some(path) = std::env::var_os("TRESSE_GUI_FOLDER") {
            let path = std::path::PathBuf::from(path);
            let record = RepoRecord {
                name: folders::folder_name(&path),
                path: path.display().to_string(),
                last_sync_at: None,
                saf: false,
            };
            self.choose(record, window, cx);
            return;
        }
        #[cfg(not(target_os = "android"))]
        {
            let prompt = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("Add a folder".into()),
            });
            cx.spawn_in(window, async move |this, cx| {
                let result = async {
                    let paths = prompt
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(paths.and_then(|paths| paths.into_iter().next()))
                }
                .await;
                this.update_in(cx, |this, window, cx| match result {
                    Ok(Some(path)) => {
                        let record = RepoRecord {
                            name: folders::folder_name(&path),
                            path: path.display().to_string(),
                            last_sync_at: None,
                            saf: false,
                        };
                        this.choose(record, window, cx);
                    }
                    Ok(None) => {}
                    Err(error) => Notice::error(error).push(window, cx),
                })
                .ok();
            })
            .detach();
        }
        #[cfg(target_os = "android")]
        {
            cx.spawn_in(window, async move |this, cx| {
                let picked = cx
                    .background_spawn(async { crate::saf::pick_folder() })
                    .await;
                this.update_in(cx, |this, window, cx| match picked {
                    Ok(Some((uri, name))) => {
                        let record = RepoRecord {
                            name,
                            path: uri,
                            last_sync_at: None,
                            saf: true,
                        };
                        this.choose(record, window, cx);
                    }
                    Ok(None) => {}
                    Err(error) => Notice::error(error).push(window, cx),
                })
                .ok();
            })
            .detach();
        }
    }

    /// An existing repository is added at once; anything else is asked about
    /// before a history is started in it.
    fn choose(&mut self, record: RepoRecord, window: &mut Window, cx: &mut Context<Self>) {
        if self.contains(&record.path) {
            Notice::new("This folder is already in the list.").push(window, cx);
            return;
        }
        let existing = match folders::open(&record) {
            Ok(repo) => repo
                .vault()
                .metadata(".tresse")
                .ok()
                .flatten()
                .is_some_and(|meta| meta.is_dir),
            Err(error) => {
                Notice::error(error).push(window, cx);
                return;
            }
        };
        if existing {
            let name = record.name.clone();
            self.register(record, cx);
            Notice::new(format!("Added “{name}”."))
                .tone(Tone::Success)
                .push(window, cx);
            return;
        }
        let this = cx.weak_entity();
        let target = record.clone();
        let name = record.name.clone();
        let make =
            Command::new("make-repository", "Make repository").on_trigger(move |window, cx| {
                this.update(cx, |this, cx| {
                    this.make_repository(target.clone(), window, cx)
                })
                .ok();
            });
        Confirm::new(
            "make-repository",
            format!("Make “{name}” a Tresse repository?"),
        )
        .body(
            "The folder has no Tresse history yet. One is started in it and its files are \
                 kept as they are; the first sync versions them.",
        )
        .confirm(make)
        .open(window, cx);
    }

    fn make_repository(&mut self, record: RepoRecord, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let target = record.clone();
        cx.spawn_in(window, async move |_, cx| {
            let result = cx
                .background_spawn({
                    let record = target.clone();
                    async move {
                        let repo = folders::open(&record)?;
                        tresse_client::init(&repo)?;
                        Ok::<_, String>(())
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let name = record.name.clone();
                match result {
                    Ok(()) => {
                        this.register(record, cx);
                        Notice::new(format!("Made “{name}” a Tresse repository."))
                            .tone(Tone::Success)
                            .push(window, cx);
                    }
                    Err(error) => {
                        Notice::error(format!("Couldn’t make it a repository. ({error})"))
                            .push(window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn register(&mut self, record: RepoRecord, cx: &mut Context<Self>) {
        Settings::update(cx, |settings| settings.add_repository(record));
        self.reload(cx);
        cx.notify();
    }

    fn remove(&mut self, path: &str, cx: &mut Context<Self>) {
        Settings::update(cx, |settings| settings.remove_repository(path));
        self.reload(cx);
        cx.notify();
    }

    fn row(&self, row: &Row, cx: &mut Context<Self>) -> ListRow {
        let key = row.record.path.clone();
        let status = self.engine.read(cx).status(&key);
        let id = SharedString::from(format!("folder-{key}"));
        let mut list_row = ListRow::new(id, row.record.name.clone())
            .leading(Tile::new(Lucide::Folder, cx.colors().accent))
            .subtitle(key.clone());
        if status.phase.is_running() {
            list_row = list_row
                .trailing(
                    div()
                        .text_color(cx.colors().muted)
                        .child(status.phase.label()),
                )
                .signal(Some(Signal::Busy));
        } else if let Some(error) = &status.error {
            list_row = list_row
                .supporting(error.clone())
                .supporting_tone(Tone::Danger);
        } else {
            match (&row.problem, &row.remote) {
                (Some(problem), _) => {
                    list_row = list_row
                        .supporting(problem.clone())
                        .supporting_tone(Tone::Danger);
                }
                (None, Some(remote)) => {
                    let mut label = storage_label(remote.storage_type).to_string();
                    if let Some(at) = status.finished_at.or(row.record.last_sync_at) {
                        label.push_str(&format!(" · {}", sync_engine::clock(at)));
                    }
                    list_row = list_row.trailing(div().text_color(cx.colors().muted).child(label));
                }
                (None, None) => {
                    list_row = list_row
                        .trailing(div().text_color(cx.colors().muted).child("Not connected"));
                }
            }
        }
        let open = {
            let this = cx.weak_entity();
            let record = row.record.clone();
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |_, cx| cx.emit(Event::Open(record.clone())))
                    .ok();
            }
        };
        let actions = self.row_actions(row, cx);
        list_row.on_activate(open).actions(actions)
    }

    fn row_actions(&self, row: &Row, cx: &mut Context<Self>) -> ItemActions {
        let this: WeakEntity<Self> = cx.weak_entity();
        let path = row.record.path.clone();
        let name = row.record.name.clone();
        let record = row.record.clone();
        let remove_id = SharedString::from(format!("remove-folder-{path}"));
        let configure_id = SharedString::from(format!("configure-folder-{path}"));
        let sync_id = SharedString::from(format!("sync-folder-{path}"));
        let engine = self.engine.clone();
        let sync_engine = engine.clone();
        let sync_record = record.clone();
        let configure_record = record.clone();
        let changed_this = this.clone();
        let changed: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
            changed_this
                .update(cx, |this, cx| {
                    this.reload(cx);
                    cx.notify();
                })
                .ok();
        });
        let sync_this = this.clone();
        ItemActions::new()
            .title(name.clone())
            .command(
                Command::new(sync_id, "Sync now").on_trigger(move |_, cx| {
                    sync_this
                        .update(cx, |_, cx| {
                            sync_engine::sync(&sync_engine, sync_record.clone(), cx)
                        })
                        .ok();
                }),
            )
            .command(
                Command::new(configure_id, "Configure connection…").on_trigger(move |window, cx| {
                    crate::connect::open(configure_record.clone(), changed.clone(), window, cx);
                }),
            )
            .command(
                Command::new(remove_id, "Remove from list")
                    .destructive()
                    .on_trigger(move |window, cx| {
                        let path = path.clone();
                        this.update(cx, |this, cx| this.remove(&path, cx)).ok();
                        Notice::new(format!(
                            "Removed “{name}” from the list. The folder and its history stay where they are."
                        ))
                        .push(window, cx);
                    }),
            )
    }
}

impl Render for Repositories {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return Empty::new(Lucide::FolderOpen, "No folders yet")
                .body(
                    "Add a folder to keep its history in Tresse and sync it with a Tresse \
                     server or your own bucket.",
                )
                .action(add_command(cx.weak_entity()))
                .into_any_element();
        }
        let rows: Vec<ListRow> = self.rows.iter().map(|row| self.row(row, cx)).collect();
        Collection::new("folders").children(rows).into_any_element()
    }
}

/// The page's one primary command, also the empty state's way in.
pub fn add_command(this: WeakEntity<Repositories>) -> Command {
    Command::new("add-folder", "Add folder…")
        .icon(Lucide::Plus)
        .primary()
        .on_trigger(move |window, cx| {
            this.update(cx, |this, cx| this.add(window, cx)).ok();
        })
}

fn storage_label(storage: StorageType) -> &'static str {
    match storage {
        StorageType::Http => "Tresse server",
        StorageType::S3 => "Own bucket",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_engine::Phase;

    fn record(path: &std::path::Path) -> RepoRecord {
        RepoRecord {
            path: path.display().to_string(),
            name: folders::folder_name(path),
            last_sync_at: None,
            saf: false,
        }
    }

    #[test]
    fn a_repository_is_recognised_by_its_tresse_directory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".tresse")).unwrap();
        let row = Row::read(&record(directory.path()));
        assert!(row.problem.is_none());
        assert!(row.remote.is_none());
    }

    #[test]
    fn a_folder_without_tresse_is_reported_not_made_one() {
        let directory = tempfile::tempdir().unwrap();
        let row = Row::read(&record(directory.path()));
        assert_eq!(
            row.problem.as_deref(),
            Some("Not a Tresse repository here.")
        );
        assert!(!directory.path().join(".tresse").exists());
    }

    #[test]
    fn a_moved_folder_is_reported_and_not_recreated() {
        let directory = tempfile::tempdir().unwrap();
        let gone = directory.path().join("gone");
        let row = Row::read(&record(&gone));
        assert_eq!(row.problem.as_deref(), Some("The folder is not there."));
        assert!(!gone.exists());
    }

    #[test]
    fn the_remote_is_read_from_the_folder_itself() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".tresse")).unwrap();
        std::fs::write(
            directory.path().join(".tresse/remotes.toml"),
            "[[remotes.origin]]\nurl = \"https://tresse.example.com\"\ntoken = \"t\"\nrepo_id = \"notes\"\n\
             encryption_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n",
        )
        .unwrap();
        let row = Row::read(&record(directory.path()));
        assert!(row.problem.is_none());
        assert_eq!(
            row.remote.as_ref().map(|remote| remote.storage_type),
            Some(StorageType::Http)
        );
    }

    #[test]
    fn idle_folders_report_no_phase_and_a_finished_one_summarises() {
        let idle = sync_engine::Status::default();
        assert_eq!(idle.phase, Phase::Idle);
        assert!(idle.summary().is_none());
        let done = sync_engine::Status {
            phase: Phase::Done,
            pulled: 2,
            pushed: 1,
            ..Default::default()
        };
        assert_eq!(done.summary().as_deref(), Some("2 received, 1 sent"));
    }
}
