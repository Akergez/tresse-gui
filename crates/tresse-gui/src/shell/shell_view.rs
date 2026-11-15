use gpui_adaptive::prim::column;
use gpui_adaptive::{AppFrame, Command, Screen};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, FocusHandle, Subscription, Window};

use crate::repo::{self, RepoView};
use crate::repos::{self, Repositories};
use crate::settings::RepoRecord;
use crate::shell::CONTEXT;
use crate::shell::{AddFolder, OpenSettings};
use crate::ui::Lucide;

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
    /// The window's own focus, so the shortcuts work wherever the keyboard
    /// was last.
    focus: FocusHandle,
    /// The system switching between light and dark, which a "System" theme
    /// has to follow while the window is open.
    _appearance: Subscription,
    _repositories: Subscription,
    _view: Option<Subscription>,
}

impl Shell {
    /// What the window is showing, for the scripted checks.
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The list of folders, for the scripted checks.
    pub fn repositories(&self) -> &Entity<Repositories> {
        &self.repositories
    }
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
            focus: cx.focus_handle(),
            _appearance: appearance,
            _repositories: subscription,
            _view: None,
        }
    }

    /// Decides what this window is for. Runs once, right after it opens.
    pub(super) fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.stage = Stage::Repositories;
        cx.notify();
    }

    fn repositories_event(&mut self, event: &repos::Event, cx: &mut Context<Self>) {
        let repos::Event::Open(record) = event;
        self.open_repo(record.clone(), cx);
    }

    /// Opens one folder's page over the list. The list stays alive under it,
    /// so going back finds it as it was.
    fn open_repo(&mut self, record: RepoRecord, cx: &mut Context<Self>) {
        let engine = crate::sync_engine::SyncEngine::global(cx);
        let view = cx.new(|cx| RepoView::new(record, engine, cx));
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
        let content = match &self.stage {
            Stage::Starting => frame.into_any_element(),
            Stage::Repositories => {
                let count = self.repositories.read(cx).len();
                let subtitle = if count == 1 {
                    "1 folder".to_string()
                } else {
                    format!("{count} folders")
                };
                let settings = Command::new("settings", "Settings…")
                    .icon(Lucide::Settings)
                    .overflow()
                    .on_trigger(|window, cx| crate::settings_dialog::open(window, cx));
                let page = Screen::new("folders", "Folders")
                    .subtitle(subtitle)
                    .command(repos::add_command(self.repositories.downgrade()))
                    .command(settings)
                    .content(self.repositories.clone());
                frame.child(page).into_any_element()
            }
            Stage::Repository(view) => frame.child(view.clone()).into_any_element(),
        };
        column()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &OpenSettings, window, cx| {
                crate::settings_dialog::open(window, cx)
            }))
            .on_action(cx.listener(|shell, _: &AddFolder, window, cx| {
                let repositories = shell.repositories.clone();
                repositories.update(cx, |repositories, cx| repositories.add(window, cx));
            }))
            .child(content)
    }
}
