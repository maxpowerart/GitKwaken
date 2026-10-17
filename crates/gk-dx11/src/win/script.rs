//! Scripted input for end-to-end UI tests: `GK_DX11_INPUT_SCRIPT=<file>`.
//!
//! Events are injected straight into egui's raw input, so tests never touch
//! the real mouse or keyboard. One command per line, coordinates in points:
//!
//! ```text
//! wait 500              # milliseconds
//! move 100 200
//! click 100 200         # also: dblclick, rclick
//! drag 10 20 300 400
//! scroll 0 -120 400 300 # dx dy at x y
//! key Enter             # egui key name, optional: ctrl shift alt
//! type hello world
//! shot D:/out.png       # screenshot of the next rendered frame
//! log some text         # appends to GK_DX11_INPUT_LOG (if set)
//! exit
//! ```

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, pos2, vec2};
use std::sync::Mutex;
use std::time::{Duration, Instant};

enum Step {
    Wait(Duration),
    Frame(Vec<Event>, Modifiers),
    Shot(std::path::PathBuf),
    Log(String),
    Exit,
}

struct Player {
    steps: Vec<Step>,
    next: usize,
    wait_until: Option<Instant>,
    pointer: Pos2,
}

static PLAYER: Mutex<Option<Player>> = Mutex::new(None);
static PENDING_SHOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
static EXIT_AFTER_FRAME: Mutex<bool> = Mutex::new(false);

fn button(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn parse(text: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let f = |s: Option<&str>| s.and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut it = rest.split_whitespace();
        let frame = |e: Vec<Event>| Step::Frame(e, Modifiers::NONE);
        match cmd {
            "wait" => steps.push(Step::Wait(Duration::from_millis(f(it.next()) as u64))),
            "move" => steps.push(frame(vec![Event::PointerMoved(pos2(
                f(it.next()),
                f(it.next()),
            ))])),
            "click" | "rclick" | "dblclick" => {
                let p = pos2(f(it.next()), f(it.next()));
                let b = if cmd == "rclick" {
                    PointerButton::Secondary
                } else {
                    PointerButton::Primary
                };
                let times = if cmd == "dblclick" { 2 } else { 1 };
                steps.push(frame(vec![Event::PointerMoved(p)]));
                for _ in 0..times {
                    steps.push(frame(vec![button(p, b, true)]));
                    steps.push(frame(vec![button(p, b, false)]));
                }
            }
            "drag" => {
                let a = pos2(f(it.next()), f(it.next()));
                let b = pos2(f(it.next()), f(it.next()));
                steps.push(frame(vec![Event::PointerMoved(a)]));
                steps.push(frame(vec![button(a, PointerButton::Primary, true)]));
                for i in 1..=12 {
                    let t = i as f32 / 12.0;
                    steps.push(frame(vec![Event::PointerMoved(a + (b - a) * t)]));
                }
                steps.push(frame(vec![button(b, PointerButton::Primary, false)]));
            }
            "scroll" => {
                let d = vec2(f(it.next()), f(it.next()));
                let p = pos2(f(it.next()), f(it.next()));
                steps.push(frame(vec![Event::PointerMoved(p)]));
                steps.push(frame(vec![Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: d,
                    phase: egui::TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                }]));
            }
            "key" => {
                let Some(key) = it.next().and_then(Key::from_name) else {
                    continue;
                };
                let mut m = Modifiers::NONE;
                for flag in it {
                    match flag {
                        "ctrl" => {
                            m.ctrl = true;
                            m.command = true;
                        }
                        "shift" => m.shift = true,
                        "alt" => m.alt = true,
                        _ => {}
                    }
                }
                let ev = |pressed| Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers: m,
                };
                steps.push(Step::Frame(vec![ev(true)], m));
                steps.push(Step::Frame(vec![ev(false)], m));
            }
            "type" => steps.push(frame(vec![Event::Text(rest.to_string())])),
            "shot" => steps.push(Step::Shot(rest.trim().into())),
            "log" => steps.push(Step::Log(rest.to_string())),
            "exit" => steps.push(Step::Exit),
            _ => log::warn!("gk-dx11 script: unknown command '{cmd}'"),
        }
    }
    steps
}

/// Loads the script on first use. Returns true while the script is running
/// (the caller keeps repainting).
pub fn inject(input: &mut RawInput) -> bool {
    let mut guard = PLAYER.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let Some(path) = std::env::var_os("GK_DX11_INPUT_SCRIPT") else {
            return false;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        *guard = Some(Player {
            steps: parse(&text),
            next: 0,
            wait_until: None,
            pointer: Pos2::ZERO,
        });
    }
    let p = guard.as_mut().expect("player");
    if *EXIT_AFTER_FRAME.lock().unwrap_or_else(|e| e.into_inner()) {
        std::process::exit(0);
    }
    if let Some(t) = p.wait_until {
        if Instant::now() < t {
            keep_pointer(input, p.pointer);
            return true;
        }
        p.wait_until = None;
    }
    // Non-frame steps run back to back; one frame step per rendered frame.
    while let Some(step) = p.steps.get(p.next) {
        p.next += 1;
        match step {
            Step::Wait(d) => {
                p.wait_until = Some(Instant::now() + *d);
                break;
            }
            Step::Shot(path) => {
                *PENDING_SHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone())
            }
            Step::Log(text) => {
                if let Some(log) = std::env::var_os("GK_DX11_INPUT_LOG") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log)
                    {
                        let _ = writeln!(f, "{text}");
                    }
                }
            }
            Step::Exit => {
                *EXIT_AFTER_FRAME.lock().unwrap_or_else(|e| e.into_inner()) = true;
                break;
            }
            Step::Frame(events, mods) => {
                for e in events {
                    match e {
                        Event::PointerMoved(pos) | Event::PointerButton { pos, .. } => {
                            p.pointer = *pos
                        }
                        _ => {}
                    }
                }
                input.events.extend(events.iter().cloned());
                input.modifiers = *mods;
                break;
            }
        }
    }
    keep_pointer(input, p.pointer);
    p.next < p.steps.len()
        || p.wait_until.is_some()
        || *EXIT_AFTER_FRAME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hover stays where the script left the pointer, even if the real mouse moves.
fn keep_pointer(input: &mut RawInput, pos: Pos2) {
    input.events.retain(|e| {
        !matches!(
            e,
            Event::PointerMoved(_) | Event::PointerGone | Event::MouseMoved(_)
        )
    });
    if pos != Pos2::ZERO
        && !input
            .events
            .iter()
            .any(|e| matches!(e, Event::PointerButton { .. }))
    {
        input.events.insert(0, Event::PointerMoved(pos));
    }
}

/// Screenshot requested by a `shot` command, taken after the frame renders.
pub fn take_shot() -> Option<std::path::PathBuf> {
    PENDING_SHOT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let s = parse("wait 10\nclick 5 6\nkey Enter ctrl\ntype hi\nshot a.png\nexit\n# c\nbogus");
        assert!(matches!(s[0], Step::Wait(_)));
        // click = move + press + release
        assert!(matches!(&s[1], Step::Frame(e, _) if matches!(e[0], Event::PointerMoved(_))));
        assert!(
            matches!(&s[4], Step::Frame(e, m) if m.ctrl && matches!(e[0], Event::Key { key: Key::Enter, pressed: true, .. }))
        );
        assert!(matches!(&s[6], Step::Frame(e, _) if matches!(&e[0], Event::Text(t) if t == "hi")));
        assert!(matches!(s[7], Step::Shot(_)));
        assert!(matches!(s[8], Step::Exit));
        assert_eq!(s.len(), 9);
    }
}
