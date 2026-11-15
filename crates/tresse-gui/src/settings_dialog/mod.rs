//! The settings dialog: how the window looks.
//!
//! Appearance takes effect as it is chosen; there is nothing to save, so the
//! form is a panel with Done.

use gpui_adaptive::{ActiveScale as _, Fields, Form, Segments, Setting, Swatches, Text};
use gpui_adaptive_colors::Color;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Window, rems};

#[cfg(not(target_os = "android"))]
use crate::appearance::InterfaceSize;
use crate::appearance::{self, ColorChoice, SEEDS, ThemeChoice};

struct SettingsForm;

impl SettingsForm {
    fn render_appearance(&self, cx: &mut Context<Self>) -> Fields {
        let current = appearance::theme_choice(cx);
        #[cfg(not(target_os = "android"))]
        let size = appearance::interface_size(cx);
        let themes = ThemeChoice::ALL
            .iter()
            .fold(
                Segments::new("theme", current.label()),
                |segments, choice| segments.segment(choice.label(), choice.label()),
            )
            .on_change(|key, window, cx| {
                if let Some(choice) = ThemeChoice::ALL.iter().find(|c| c.label() == key.as_ref()) {
                    appearance::set_theme_choice(*choice, window, cx);
                }
            });
        #[cfg(not(target_os = "android"))]
        let sizes = InterfaceSize::ALL
            .iter()
            .fold(
                Segments::new("interface-size", size.label()),
                |segments, size| segments.segment(size.label(), size.label()),
            )
            .on_change(|key, _, cx| {
                if let Some(size) = InterfaceSize::ALL
                    .iter()
                    .find(|s| s.label() == key.as_ref())
                {
                    appearance::set_interface_size(*size, cx);
                }
            });
        let fields = Fields::new()
            .child(Text::title("Appearance"))
            .child(
                Setting::new("Theme")
                    .detail("System follows the desktop as it changes")
                    .control(themes),
            )
            .child(
                Setting::new("Color")
                    .detail("System follows the desktop's accent color")
                    .below(color_choices(appearance::color_choice(cx))),
            );
        // A phone has its own font size setting; the application does not put
        // a second one beside it.
        #[cfg(not(target_os = "android"))]
        let fields = fields.child(
            Setting::new("Size")
                .detail("Text, controls and spacing together")
                .control(sizes),
        );
        fields
    }
}

impl Render for SettingsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let space = cx.scale().space_xl;
        gpui_adaptive::prim::column()
            .gap(space)
            .child(self.render_appearance(cx))
    }
}

/// The system's colour, then ours as swatches. A swatch shows the seed
/// itself rather than what a scheme makes of it, since that is what tells
/// them apart.
fn color_choices(current: ColorChoice) -> Swatches {
    let picked = match current {
        ColorChoice::System => None,
        ColorChoice::Seed(color) => SEEDS
            .into_iter()
            .find(|(stored, _)| Color::parse_hex(stored) == Some(color))
            .map(|(stored, _)| stored),
    };
    SEEDS
        .into_iter()
        .filter_map(|(stored, name)| Some((stored, name, Color::parse_hex(stored)?)))
        .fold(
            Swatches::new("colors", picked).unset("System"),
            |swatches, (stored, name, color)| {
                swatches.swatch(stored, name, gpui_adaptive_colors::hsla(color))
            },
        )
        .on_pick(|key, _, cx| {
            let choice = key
                .and_then(|key| Color::parse_hex(key))
                .map_or(ColorChoice::System, ColorChoice::Seed);
            appearance::set_color_choice(choice, cx);
        })
}

pub fn open(window: &mut Window, cx: &mut App) {
    let form = cx.new(|_| SettingsForm);
    Form::new("Settings", form)
        .width(rems(40.))
        .open(window, cx);
}
