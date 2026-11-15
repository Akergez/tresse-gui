use gpui_adaptive::{AppFrame, Screen};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Subscription, Window};

use crate::repos::{self, Repositories};

/// What the window is showing.
pub enum Stage {
    /// Nothing has been decided yet: the window has only just opened.
    Starting,
    /// The folders this installation manages.
    Repositories(Entity<Repositories>),
}

pub struct Shell {
    stage: Stage,
    /// The system switching between light and dark, which a "System" theme
    /// has to follow while the window is open.
    _appearance: Subscription,
}

impl Shell {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Only now is there a window to ask what the desktop looks like.
        crate::appearance::apply(Some(window), cx);
        let appearance = window
            .observe_window_appearance(|window, cx| crate::appearance::apply(Some(window), cx));
        Shell {
            stage: Stage::Starting,
            _appearance: appearance,
        }
    }

    /// Decides what this window is for. Runs once, right after it opens.
    pub(super) fn start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let repositories = cx.new(|cx| Repositories::new(cx));
        self.stage = Stage::Repositories(repositories);
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = AppFrame::new("Tresse GUI");
        match &self.stage {
            Stage::Starting => frame,
            Stage::Repositories(repositories) => {
                let count = repositories.read(cx).len();
                let subtitle = if count == 1 {
                    "1 folder".to_string()
                } else {
                    format!("{count} folders")
                };
                let page = Screen::new("folders", "Folders")
                    .subtitle(subtitle)
                    .command(repos::add_command(repositories.downgrade()))
                    .content(repositories.clone());
                frame.child(page)
            }
        }
    }
}
