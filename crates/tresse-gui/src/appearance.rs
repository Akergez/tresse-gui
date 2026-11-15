//! Light or dark, in which colours, and how large.
//!
//! Stage S6 brings the full choice: light, dark or the system's, a Material
//! You scheme made from the system's colour or one of ours, and the
//! interface size, all kept in `settings.json`. Until then a "System" theme
//! is followed and the rem is the library's own base font.

use gpui_adaptive::theme::Theme;
use gpui_adaptive_colors::{Color, Options};
use gpui_kit::{App, Pixels, Window};

/// The colour where the system has none to give: the application icon's
/// green.
const OWN_COLOR: Color = Color::from_u32(0x2f9e6e);

/// Hands the theme's colours to the scheme. Once, at startup, before the
/// first [`apply`].
pub fn init(cx: &mut App) {
    gpui_adaptive_colors::init(Options::new(OWN_COLOR), cx);
}

/// The rem the interface is drawn at.
pub fn rem(cx: &App) -> Pixels {
    Theme::global(cx).font_size
}

/// Makes the theme say what the system says. Call at startup, again once
/// there is a window — on Linux only a window knows what the desktop's
/// appearance is — and whenever it changes.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let mode = gpui_android_glue::appearance::system_mode(window.as_deref(), cx);
    // The clock and the icons beside it are drawn over our own top bar, and
    // have to be told which of its colours to stand out against.
    gpui_android_glue::appearance::set_status_bar_for(mode);
    if Theme::global(cx).mode != mode {
        Theme::change(mode, None, cx);
    }
}
