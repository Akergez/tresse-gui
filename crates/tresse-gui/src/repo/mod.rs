//! One folder's page: what its sync did, where it syncs, and what can be done
//! about either.

use std::rc::Rc;

use gpui_adaptive::{Command, Fields, Note, Screen, Setting, Tone};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, Window};

use crate::connect;
use crate::folders;
use crate::settings::RepoRecord;
use crate::sync_engine::{self, Phase, SyncEngine};
use crate::ui::Lucide;
use tresse_client::RemoteConfig;

/// What the page asks the window to do.
pub enum Event {
    /// Back to the list of folders.
    Back,
    /// The connection changed; the list reads the folder again.
    Changed,
}

pub struct RepoView {
    record: RepoRecord,
    engine: Entity<SyncEngine>,
    remote: Option<RemoteConfig>,
    problem: Option<String>,
}

impl EventEmitter<Event> for RepoView {}

impl RepoView {
    pub fn new(record: RepoRecord, engine: Entity<SyncEngine>, cx: &mut Context<Self>) -> Self {
        cx.observe(&engine, |_, _, cx| cx.notify()).detach();
        let mut view = RepoView {
            record,
            engine,
            remote: None,
            problem: None,
        };
        view.reload(cx);
        view
    }

    /// Reads the folder's remote again, after a connection was saved or the
    /// folder changed on disk.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.problem = None;
        self.remote = None;
        if !self.record.saf && !std::path::Path::new(&self.record.path).is_dir() {
            self.problem = Some("The folder is not there.".to_string());
        } else {
            match folders::open(&self.record)
                .and_then(|repo| tresse_client::config::read_remote(repo.vault().as_ref()))
            {
                Ok(remote) => self.remote = remote,
                Err(error) => self.problem = Some(error),
            }
        }
        cx.notify();
    }

    fn status_note(&self, cx: &Context<Self>) -> Note {
        let status = self.engine.read(cx).status(&self.record.path);
        match status.phase {
            Phase::Idle => Note::new("Not synced yet."),
            Phase::Failed => Note::new(
                status
                    .error
                    .clone()
                    .unwrap_or_else(|| "Sync failed.".into()),
            )
            .tone(Tone::Danger),
            Phase::Done => Note::new(format!(
                "Synced: {}.",
                status.summary().unwrap_or_else(|| "no changes".into())
            ))
            .tone(Tone::Success),
            running => Note::new(running.label()).tone(Tone::Info),
        }
    }

    fn render_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.record.path.clone();
        let status = self.engine.read(cx).status(&key);
        let connection = Setting::new("Connection").detail(match &self.remote {
            Some(remote) => connect::describe(remote),
            None => "Not connected".to_string(),
        });
        let folder = Setting::new("Folder").detail(key.clone());
        let last = status.finished_at.map(|at| {
            Setting::new("Last sync").detail(format!(
                "{} · {}",
                sync_engine::clock(at),
                status.summary().unwrap_or_else(|| "no changes".into())
            ))
        });
        Fields::new()
            .child(self.status_note(cx))
            .children(
                self.problem
                    .clone()
                    .map(|problem| Note::new(problem).tone(Tone::Danger)),
            )
            .child(connection)
            .child(folder)
            .children(last)
    }
}

impl Render for RepoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.record.path.clone();
        let running = self.engine.read(cx).status(&key).phase.is_running();
        let this = cx.entity().downgrade();

        let back: Rc<dyn Fn(&mut Window, &mut App)> = {
            let this = this.clone();
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(Event::Back)).ok();
            })
        };
        let sync = Command::new("sync-now", "Sync now")
            .icon(Lucide::RefreshCw)
            .primary()
            .disabled(running)
            .on_trigger({
                let this = this.clone();
                move |_, cx| {
                    this.update(cx, |view, cx| {
                        sync_engine::sync(&view.engine, view.record.clone(), cx)
                    })
                    .ok();
                }
            });
        let apply = Command::new("apply-remote", "Apply remote changes")
            .icon(Lucide::ArrowDown)
            .disabled(running)
            .on_trigger({
                let this = this.clone();
                move |_, cx| {
                    this.update(cx, |view, cx| {
                        sync_engine::apply(&view.engine, view.record.clone(), cx)
                    })
                    .ok();
                }
            });
        let configure = Command::new("configure-connection", "Configure connection…")
            .icon(Lucide::Settings)
            .overflow()
            .on_trigger({
                let this = this.clone();
                move |window, cx| {
                    let changed: Rc<dyn Fn(&mut Window, &mut App)> = {
                        let this = this.clone();
                        Rc::new(move |_, cx| {
                            this.update(cx, |_, cx| cx.emit(Event::Changed)).ok();
                        })
                    };
                    this.update(cx, |view, cx| {
                        connect::open(view.record.clone(), changed, window, cx);
                    })
                    .ok();
                }
            });

        Screen::new(
            SharedString::from(format!("repo-{key}")),
            self.record.name.clone(),
        )
        .subtitle(key)
        .up(Some(back))
        .command(sync)
        .command(apply)
        .command(configure)
        .content(self.render_content(cx))
    }
}
