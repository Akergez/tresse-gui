//! The commands that have a keyboard form.
//!
//! Each is one action whether it comes from a key, a button or a menu, so the
//! entry points cannot disagree about what it does.

gpui_kit::actions!(tresse_gui, [Quit]);

/// The key context the window's shortcuts live in. Used once there are
/// commands that belong to a focused region (stage S2).
#[allow(dead_code)]
pub const CONTEXT: &str = "TresseGui";
