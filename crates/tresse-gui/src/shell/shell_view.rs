use gpui_adaptive::{AppFrame, Empty};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Subscription, Window};

use crate::ui::Lucide;

/// What the window is showing.
pub enum Stage {
    /// Nothing has been decided yet: the window has only just opened.
    Starting,
    /// No folder is set up here, so ask which to add.
    Welcome,
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
        // Stage S2 replaces this with the registry of folders: until there is
        // one, every launch has nothing set up.
        self.stage = Stage::Welcome;
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let frame = AppFrame::new("Tresse GUI");
        match &self.stage {
            Stage::Starting => frame,
            Stage::Welcome => frame.child(
                Empty::new(Lucide::FolderOpen, "No folders yet")
                    .body("Add a folder to keep its history and sync it with Tresse."),
            ),
        }
    }
}
