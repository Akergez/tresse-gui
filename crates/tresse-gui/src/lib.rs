//! Tresse GUI: folders versioned and synchronized with Tresse.
//!
//! The window lists the folders the person keeps in Tresse, and what is in
//! each: whether it has a remote, when it last synced and how the last sync
//! went. A folder is added from the desktop's file dialog or, on a phone,
//! from the system's directory picker, and is either an existing repository
//! or a new one made in place.
//!
//! The engine behind it is `tresse-client`; this crate is the interface,
//! drawn with the `gpui-adaptive` design system (gpui-kit underneath), once
//! for a pointer and a finger alike.
//!
//! This is a library so that Android can load it: there the system starts a
//! Java activity, which loads this as a shared object and calls
//! [`android_main`]. Everywhere else `main.rs` calls [`run`].

mod appearance;
mod connect;
mod paths;
mod repo;
mod repos;
mod settings;
mod settings_dialog;
mod shell;
mod sync_engine;
mod ui;

/// Names the application to the system. The directories the settings live in
/// are named after it, so it must never change.
pub const APP_ID: &str = "app.akergez.TresseGui";

/// The desktop entry point: what `main` is.
#[cfg(not(target_os = "android"))]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,tresse_gui=info".into()),
        )
        .init();

    gpui_kit::application()
        .with_assets(gpui_adaptive::AllAssets)
        .run(start);
}

/// What every platform does once the toolkit is running.
fn start(cx: &mut gpui_kit::App) {
    gpui_kit::init(cx);
    // On Android `launch::run` has done this already.
    #[cfg(not(target_os = "android"))]
    gpui_android_glue::init(cx);
    gpui_adaptive::init(input_mode(), cx);
    settings::Settings::install(cx);
    sync_engine::SyncEngine::install(cx);
    appearance::init(cx);
    appearance::apply(None, cx);
    shell::init(cx);
    shell::open_window(cx);
}

/// A finger on Android; a pointer everywhere else, unless
/// `TRESSE_GUI_INPUT=touch` asks for a desktop window drawn for a finger,
/// which is how the phone's skin is looked at and scripted without a phone.
fn input_mode() -> gpui_adaptive::InputMode {
    if cfg!(target_os = "android") || std::env::var("TRESSE_GUI_INPUT").as_deref() == Ok("touch") {
        gpui_adaptive::InputMode::Touch
    } else {
        gpui_adaptive::InputMode::Pointer
    }
}

/// The Android entry point, called by `android-activity` on a thread of its
/// own once the activity has loaded this library.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: gpui_android_glue::launch::AndroidApp) {
    use gpui_android_glue::launch::{Config, run};
    run(
        app,
        Config::new("tresse-gui").filter("warn,tresse_gui=info"),
        gpui_adaptive::AllAssets,
        start,
    );
}
