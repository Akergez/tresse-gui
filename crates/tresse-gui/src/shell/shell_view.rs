use std::path::PathBuf;

use gpui_adaptive::{AppFrame, Screen};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Subscription, Window};

use crate::repo::{self, RepoView};
use crate::repos::{self, Repositories};

/// What the window is showing.
pub enum Stage {
    /// Nothing has been decided yet: the window has only just opened.
    Starting,
    /// The folders this installation manages.
    Repositories,
    /// One folder's page, over the list.
    Repository(Entity<RepoView>),
}

pub struct Shell {
    stage: Stage,
    repositories: Entity<Repositories>,
    /// The system switching between light and dark, which a "System" theme
    /// has to follow while the window is open.
    _appearance: Subscription,
    _repositories: Subscription,
    _view: Option<Subscription>,
}

impl Shell {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Only now is there a window to ask what the desktop looks like.
        crate::appearance::apply(Some(window), cx);
        let appearance = window
            .observe_window_appearance(|window, cx| crate::appearance::apply(Some(window), cx));
        let repositories = cx.new(|cx| Repositories::new(cx));
        let subscription = cx.subscribe(&repositories, |shell, _, event: &repos::Event, cx| {
            shell.repositories_event(event, cx)
        });
        Shell {
            stage: Stage::Starting,
            repositories,
            _appearance: appearance,
            _repositories: subscription,
            _view: None,
        }
    }

    /// Decides what this window is for. Runs once, right after it opens.
    pub(super) fn start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Repositories;
        cx.notify();
    }

    fn repositories_event(&mut self, event: &repos::Event, cx: &mut Context<Self>) {
        let repos::Event::Open(path) = event;
        self.open_repo(path.clone(), cx);
    }

    /// Opens one folder's page over the list. The list stays alive under it,
    /// so going back finds it as it was.
    fn open_repo(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = repos::folder_name(&path);
        let engine = crate::sync_engine::SyncEngine::global(cx);
        let view = cx.new(|cx| RepoView::new(path, name, engine, cx));
        let subscription =
            cx.subscribe(&view, |shell, view, event: &repo::Event, cx| match event {
                repo::Event::Back => {
                    shell.stage = Stage::Repositories;
                    cx.notify();
                }
                repo::Event::Changed => {
                    shell.repositories.update(cx, |repositories, cx| {
                        repositories.reload(cx);
                        cx.notify();
                    });
                    view.update(cx, |view, cx| view.reload(cx));
                }
            });
        self._view = Some(subscription);
        self.stage = Stage::Repository(view);
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = AppFrame::new("Tresse GUI");
        match &self.stage {
            Stage::Starting => frame,
            Stage::Repositories => {
                let count = self.repositories.read(cx).len();
                let subtitle = if count == 1 {
                    "1 folder".to_string()
                } else {
                    format!("{count} folders")
                };
                let page = Screen::new("folders", "Folders")
                    .subtitle(subtitle)
                    .command(repos::add_command(self.repositories.downgrade()))
                    .content(self.repositories.clone());
                frame.child(page)
            }
            Stage::Repository(view) => frame.child(view.clone()),
        }
    }
}
