//! A scripted pair of hands, for looking at the interface without a human.
//!
//! `TRESSE_GUI_SCRIPT="wait:1500;expect:stage=folders;expect:folders=0;quit"`
//! plays those steps into the window once it opens. Nothing here is reachable
//! without the variable; it exists because a Wayland session gives a test no
//! other way to press a button and see what the screen looks like afterwards.
//!
//! Steps:
//! - `wait:<ms>`
//! - `click:<x>,<y>`, `rclick:<x>,<y>`, `move:<x>,<y>`
//! - `press:<x>,<y>`, `release:<x>,<y>` — the left button down and up apart,
//!   with `move`s and `wait`s between them for a drag
//! - `scroll:<x>,<y>,<dy>`
//! - `key:<keystroke>` — `ctrl-n`, `escape`, `enter`
//! - `type:<text>`
//! - `shot:<name>` — runs `$TRESSE_GUI_SHOT_CMD <name>`
//! - `frames:<name>` — draws the window afresh on every frame for two seconds
//!   and prints `script: frames <name>=<per second>`
//! - `expect:<what>=<value>` — ends the process with status 1 unless it holds
//! - `quit`
//!
//! `expect` is what turns a script into a test (`tests/ui/` and
//! `build-aux/ui-tests.sh`): a click that lands beside its button changes
//! nothing, and without a check the script would still run to its `quit`. It
//! reads the application's state, not the picture, so it does not depend on
//! fonts or on how a compositor draws:
//! - `stage=starting|folders|repository`
//! - `folders=<n>` — how many folders the list holds
//! - `folder=<name>` — the folder whose page is open
//! - `folder-remote=yes|no` — whether that folder has a connection
//! - `folder-problem=yes|no` — whether the page reports something wrong
//! - `sync=idle|running|done|failed` — that folder's last sync
//! - `dialog=open|closed` — whether a form or a sheet is over the page
//! - `theme=light|dark`
//! - `layout=compact|medium|expanded` — the width class of the last frame
//!
//! Coordinates are divided by `TRESSE_GUI_SCRIPT_SCALE`, so they can be read
//! straight off a screenshot taken on a scaled display.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_adaptive::{ActiveColors as _, Env, Sheet};
use gpui_kit::{
    AnyWindowHandle, App, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, ScrollDelta, ScrollWheelEvent,
    TouchPhase, Window, point,
};

use crate::shell::Stage;
use crate::sync_engine::{Phase, SyncEngine};

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Wait(u64),
    Click(f32, f32, bool),
    Press(f32, f32),
    Release(f32, f32),
    Move(f32, f32),
    Scroll(f32, f32, f32),
    Key(String),
    Type(String),
    Shot(String),
    Frames(String),
    Expect(String, String),
    Quit,
}

/// How long `frames:` draws for.
const FRAMES_FOR: Duration = Duration::from_secs(2);

/// Draws the whole window again on every frame the display offers for
/// [`FRAMES_FOR`] and says how many a second that came to. Nothing is cached
/// between those frames, which is what an animation or a drag costs: a number
/// well under the display's rate is a screen that cannot keep up.
fn count_frames(name: String, started: Instant, drawn: u32, window: &mut Window) {
    window.refresh();
    window.on_next_frame(move |window, _| {
        let elapsed = started.elapsed();
        if elapsed < FRAMES_FOR {
            count_frames(name, started, drawn + 1, window);
        } else {
            let rate = (drawn + 1) as f32 / elapsed.as_secs_f32();
            eprintln!("script: frames {name}={rate:.0}");
        }
    });
}

fn numbers(text: &str) -> Option<Vec<f32>> {
    text.split(',').map(|n| n.trim().parse().ok()).collect()
}

fn parse(script: &str) -> Vec<Step> {
    script
        .split(';')
        .filter_map(|step| {
            let step = step.trim();
            let (verb, rest) = step.split_once(':').unwrap_or((step, ""));
            Some(match verb {
                "wait" => Step::Wait(rest.parse().ok()?),
                "click" | "rclick" => match numbers(rest)?[..] {
                    [x, y] => Step::Click(x, y, verb == "rclick"),
                    _ => return None,
                },
                "press" => match numbers(rest)?[..] {
                    [x, y] => Step::Press(x, y),
                    _ => return None,
                },
                "release" => match numbers(rest)?[..] {
                    [x, y] => Step::Release(x, y),
                    _ => return None,
                },
                "move" => match numbers(rest)?[..] {
                    [x, y] => Step::Move(x, y),
                    _ => return None,
                },
                "scroll" => match numbers(rest)?[..] {
                    [x, y, dy] => Step::Scroll(x, y, dy),
                    _ => return None,
                },
                "key" => Step::Key(rest.to_string()),
                "type" => Step::Type(rest.to_string()),
                "shot" => Step::Shot(rest.to_string()),
                "frames" => Step::Frames(rest.to_string()),
                "expect" => {
                    let (what, value) = rest.split_once('=')?;
                    Step::Expect(what.trim().to_string(), value.trim().to_string())
                }
                "quit" => Step::Quit,
                _ => {
                    tracing::warn!(step, "unknown script step");
                    return None;
                }
            })
        })
        .collect()
}

/// Plays `TRESSE_GUI_SCRIPT` into `window`, if there is one to play.
pub fn play(window: AnyWindowHandle, cx: &mut App) {
    let Ok(script) = std::env::var("TRESSE_GUI_SCRIPT") else {
        return;
    };
    next(window, parse(&script).into(), cx);
}

/// Coordinates are read off screenshots, which are in device pixels;
/// `TRESSE_GUI_SCRIPT_SCALE` is the factor between those and the window's own.
fn at(x: f32, y: f32) -> Point<Pixels> {
    let scale = std::env::var("TRESSE_GUI_SCRIPT_SCALE")
        .ok()
        .and_then(|scale| scale.parse::<f32>().ok())
        .filter(|scale| *scale > 0.0)
        .unwrap_or(1.0);
    point(Pixels::from(x / scale), Pixels::from(y / scale))
}

fn pointer(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        cx,
    );
}

/// What `expect:<what>` compares against, or `None` for a name it does not
/// know — which fails the script too, so a misspelt check cannot pass.
fn observe(what: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    let shell = crate::shell::current(cx)?;
    let shell = shell.read(cx);
    // The ones that are about the window rather than about a folder.
    match what {
        "stage" => {
            return Some(
                match shell.stage() {
                    Stage::Starting => "starting",
                    Stage::Repositories => "folders",
                    Stage::Repository(_) => "repository",
                }
                .to_string(),
            );
        }
        "folders" => return Some(shell.repositories().read(cx).len().to_string()),
        "dialog" | "sheet" => {
            let open = Sheet::is_open(window, cx);
            return Some(if open { "open" } else { "closed" }.to_string());
        }
        "theme" => {
            let dark = cx.colors().dark;
            return Some(if dark { "dark" } else { "light" }.to_string());
        }
        "layout" => {
            return Some(Env::of(window, cx).width().name().to_string());
        }
        _ => {}
    }

    let Stage::Repository(view) = shell.stage() else {
        return None;
    };
    let view = view.read(cx);
    let record = view.record();
    Some(
        match what {
            "folder" => record.name.as_str(),
            "folder-remote" => {
                if view.remote().is_some() {
                    "yes"
                } else {
                    "no"
                }
            }
            "folder-problem" => {
                if view.problem().is_some() {
                    "yes"
                } else {
                    "no"
                }
            }
            "sync" => {
                let status = SyncEngine::global(cx).read(cx).status(&record.path);
                match status.phase {
                    Phase::Idle => "idle",
                    Phase::Committing | Phase::Exchanging | Phase::Applying => "running",
                    Phase::Done => "done",
                    Phase::Failed => "failed",
                }
            }
            _ => return None,
        }
        .to_string(),
    )
}

fn next(handle: AnyWindowHandle, mut steps: VecDeque<Step>, cx: &mut App) {
    let Some(step) = steps.pop_front() else {
        return;
    };
    // Every step is followed by a short pause: what a step does is usually
    // queued rather than done, and the next one should see its result.
    let mut pause = 120;
    tracing::info!(?step, "script");
    let _ = handle.update(cx, |_, window, cx| match &step {
        Step::Wait(ms) => pause = *ms,
        Step::Move(x, y) => pointer(window, at(*x, *y), cx),
        Step::Press(x, y) => {
            let position = at(*x, *y);
            pointer(window, position, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        }
        Step::Release(x, y) => {
            let position = at(*x, *y);
            pointer(window, position, cx);
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        Step::Click(x, y, right) => {
            let position = at(*x, *y);
            let button = if *right {
                MouseButton::Right
            } else {
                MouseButton::Left
            };
            pointer(window, position, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        Step::Scroll(x, y, dy) => {
            pointer(window, at(*x, *y), cx);
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: at(*x, *y),
                    delta: ScrollDelta::Pixels(point(Pixels::ZERO, Pixels::from(*dy))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
        }
        Step::Key(key) => match Keystroke::parse(key) {
            Ok(keystroke) => {
                window.dispatch_keystroke(keystroke.clone(), cx);
                // And up again, as a real key goes: a focused button is
                // pressed by Space or Enter as the key is let go.
                window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
            }
            Err(error) => tracing::warn!(%error, key, "not a keystroke"),
        },
        Step::Type(text) => {
            for letter in text.chars() {
                let keystroke = Keystroke {
                    modifiers: Modifiers::default(),
                    key: letter.to_string(),
                    key_char: Some(letter.to_string()),
                };
                window.dispatch_keystroke(keystroke, cx);
            }
        }
        Step::Frames(name) => {
            pause = FRAMES_FOR.as_millis() as u64 + 300;
            count_frames(name.clone(), Instant::now(), 0, window);
        }
        Step::Shot(name) => {
            if let Ok(command) = std::env::var("TRESSE_GUI_SHOT_CMD") {
                let _ = std::process::Command::new(command).arg(name).status();
            }
        }
        Step::Expect(what, value) => {
            let found = observe(what, window, cx);
            if found.as_deref() != Some(value) {
                // Straight out, with a status the runner can see: the steps
                // after a failed check would only be acting on the wrong screen.
                eprintln!("script: expected {what}={value}, found {found:?}");
                std::process::exit(1);
            }
        }
        Step::Quit => cx.quit(),
    });
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(pause))
            .await;
        cx.update(|cx| next(handle, steps, cx));
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_steps_between_semicolons() {
        assert_eq!(
            parse(
                "wait:200; click:10,20 ;rclick:1,2;key:ctrl-n;type:a:b;shot:x;expect:folder=Notes;quit"
            ),
            [
                Step::Wait(200),
                Step::Click(10.0, 20.0, false),
                Step::Click(1.0, 2.0, true),
                Step::Key("ctrl-n".into()),
                Step::Type("a:b".into()),
                Step::Shot("x".into()),
                Step::Expect("folder".into(), "Notes".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn a_step_that_does_not_parse_is_skipped() {
        assert_eq!(
            parse("click:1;bogus;wait:x;scroll:1,2,3"),
            [Step::Scroll(1.0, 2.0, 3.0)]
        );
    }
}
