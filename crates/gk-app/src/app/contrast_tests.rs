//! Every view of the real app, painted headless in each built-in style and theme, hovered
//! and clicked all over: no text may be hard to read on what is painted under it (selected
//! and hovered rows, chips, badges, menus, dialogs). See [`crate::contrast::check`].

use super::{Action, GkApp};
use crate::contrast::{self, Violation};
use crate::tab::View;
use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const SCREEN: egui::Vec2 = vec2(1400.0, 900.0);

const UGLY: &str = r##"name = "Ugly"
[dark]
panel = "#444444"
window = "#4A4A4A"
text = "#666666"
accent = "#7FD7CF"
on_accent = "#FFFFFF"
remote = "#D0C0FF"
selection = "#5A5A5A"
muted = "#555555"
chrome = "#777777"
[light]
panel = "#BBBBBB"
text = "#999999"
accent = "#1A1A40"
on_accent = "#000000"
tag = "#FFE070"
muted = "#AAAAAA"
"##;

/// The minimum contrast checked; `GK_CONTRAST_MIN` raises it to look for weak spots.
fn min_ratio() -> f32 {
    std::env::var("GK_CONTRAST_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(contrast::SECONDARY)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@x",
            "-c",
            "core.autocrlf=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// A clone with everything the views color differently: branches ahead of and behind their
/// upstream, remote-only commits, a tag, a stash, staged, modified, deleted and new files.
fn make_repo(root: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    let (remote, work, other) = (
        root.join("remote.git"),
        root.join("work"),
        root.join("other"),
    );
    std::fs::create_dir_all(&work).unwrap();
    git(root, &["init", "-q", "--bare", "remote.git"]);
    git(&work, &["init", "-q"]);
    git(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    for i in 0..6 {
        commit(
            &work,
            "a.txt",
            &format!("{i}\n"),
            &format!("feat: change {i}"),
        );
    }
    std::fs::write(work.join("b.txt"), "b\n").unwrap();
    std::fs::write(work.join("c.txt"), "c\n").unwrap();
    commit(&work, "d.txt", "d\n", "more files");
    git(&work, &["tag", "v1.0"]);
    git(&work, &["branch", "scopes"]);
    git(&work, &["push", "-q", "-u", "origin", "main", "scopes"]);
    git(
        &work,
        &["branch", "--set-upstream-to=origin/scopes", "scopes"],
    );
    git(root, &["clone", "-q", remote.to_str().unwrap(), "other"]);
    commit(&other, "e.txt", "e\n", "remote work on main");
    git(&other, &["checkout", "-q", "scopes"]);
    for i in 0..3 {
        commit(&other, "s.txt", &format!("{i}\n"), &format!("scopes {i}"));
    }
    git(&other, &["push", "-q", "origin", "main", "scopes"]);
    commit(&work, "a.txt", "local\n", "local work");
    git(&work, &["fetch", "-q", "origin"]);
    std::fs::write(work.join("a.txt"), "stashed\n").unwrap();
    git(&work, &["stash", "push", "-q", "-m", "wip"]);
    std::fs::write(work.join("a.txt"), "modified\n").unwrap();
    std::fs::write(work.join("n.txt"), "new\n").unwrap();
    git(&work, &["add", "n.txt"]);
    std::fs::remove_file(work.join("c.txt")).unwrap();
    std::fs::write(work.join("u.txt"), "untracked\n").unwrap();
    work
}

struct Run {
    ctx: egui::Context,
    app: GkApp,
    time: f64,
    label: String,
    found: Vec<(String, Violation)>,
}

impl Run {
    fn frame(&mut self, events: Vec<Event>) {
        self.paint(events, false);
    }

    /// Lets hover and open animations finish, then checks what the frame shows.
    fn settle(&mut self) {
        self.idle(2);
        self.time += 1.0;
        self.paint(Vec::new(), true);
    }

    fn paint(&mut self, events: Vec<Event>, check: bool) {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let out = self.ctx.run_ui(input, |ui| gk_dx11::App::ui(&mut *app, ui));
        if !check {
            return;
        }
        let base = self.ctx.global_style().visuals.panel_fill;
        for v in contrast::check(&out.shapes, base, min_ratio()) {
            if !self
                .found
                .iter()
                .any(|(_, f)| f.text == v.text && f.fg == v.fg && f.bg == v.bg)
            {
                self.found.push((self.label.clone(), v));
            }
        }
    }

    fn idle(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(Vec::new());
        }
    }

    fn hover(&mut self, p: Pos2) {
        self.frame(vec![Event::PointerMoved(p)]);
        self.settle();
    }

    fn click(&mut self, p: Pos2) {
        self.frame(vec![Event::PointerMoved(p)]);
        let b = |pressed| Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![b(true)]);
        self.frame(vec![b(false)]);
        // Never a double click.
        self.time += 2.0;
        self.settle();
    }

    /// Hovers a grid over `area`, then clicks down its rows at `x`.
    fn sweep(&mut self, area: Rect, step: f32, click_x: Option<f32>) {
        let mut y = area.top() + step / 2.0;
        while y < area.bottom() {
            let mut x = area.left() + 20.0;
            while x < area.right() {
                self.hover(pos2(x, y));
                x += 260.0;
            }
            if let Some(cx) = click_x {
                self.click(pos2(cx, y));
            }
            y += step;
        }
    }

    fn wait_loaded(&mut self) {
        let start = Instant::now();
        loop {
            self.idle(1);
            let ready = self.app.tabs.first().is_some_and(|t| {
                let s = t.session.snapshot();
                !s.history_loading && !s.status_loading && s.history.len() > 5
            });
            if ready {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "repository did not load"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        self.idle(5);
    }

    fn view(&mut self, v: View) {
        self.app.tabs[0].view = v;
        self.idle(3);
    }
}

#[test]
fn every_view_is_readable_in_every_style_and_theme() {
    let root = std::env::temp_dir().join(format!("gk-contrast-{}", std::process::id()));
    let repo = make_repo(&root);
    // Settings, recent repositories and window state go to a throwaway folder.
    let config = root.join("config");
    std::fs::create_dir_all(&config).unwrap();
    // A user style that gets its colors wrong: the app must still be readable.
    std::fs::create_dir_all(config.join("styles")).unwrap();
    std::fs::write(config.join("styles").join("ugly.toml"), UGLY).unwrap();
    unsafe {
        std::env::set_var(gk_settings::CONFIG_DIR_ENV, &config);
        std::env::set_var("GITKWAKEN_NO_UPDATE_CHECK", "1");
    }

    let mut all = Vec::new();
    for style in ["Kwak", "Classic", "Ugly"] {
        for theme in [gk_settings::Theme::Dark, gk_settings::Theme::Light] {
            let ctx = egui::Context::default();
            let settings = gk_settings::Settings {
                style: style.to_string(),
                theme,
                ..Default::default()
            };
            let app = GkApp::new(&ctx, settings, vec![repo.clone()], Default::default());
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            let mut r = Run {
                ctx,
                app,
                time: 0.0,
                label: format!("{style} {theme:?}"),
                found: Vec::new(),
            };
            r.wait_loaded();
            let body = Rect::from_min_max(pos2(0.0, 135.0), pos2(SCREEN.x, SCREEN.y - 30.0));
            let sidebar = Rect::from_min_max(pos2(0.0, 135.0), pos2(290.0, SCREEN.y - 30.0));
            let main = Rect::from_min_max(pos2(300.0, 180.0), pos2(SCREEN.x, SCREEN.y - 30.0));

            for (v, name) in [
                (View::History, "history"),
                (View::FileStatus, "file status"),
            ] {
                r.label = format!("{style} {theme:?} {name}");
                r.view(v);
                r.sweep(main, 27.0, Some(520.0));
                r.view(v);
                r.sweep(sidebar, 13.0, Some(120.0));
            }
            for (v, name) in [(View::Reflog, "reflog"), (View::Search, "search")] {
                r.label = format!("{style} {theme:?} {name}");
                r.view(v);
                r.sweep(main, 27.0, Some(520.0));
            }
            // Menus: open each, hover down its items.
            r.label = format!("{style} {theme:?} menus");
            r.view(View::History);
            for x in [20.0, 60.0, 105.0, 170.0, 240.0, 300.0, 350.0] {
                r.click(pos2(x, 14.0));
                for y in (40..520).step_by(22) {
                    r.hover(pos2(x + 40.0, y as f32));
                }
                r.frame(vec![Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }]);
                r.idle(2);
            }
            // Settings dialog.
            r.label = format!("{style} {theme:?} settings");
            r.app
                .actions
                .push(Action::Global(crate::dialogs::GlobalDialog::settings()));
            r.settle();
            r.sweep(body, 30.0, None);
            all.extend(r.found);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    // One line per color pair (the same mistake repeats on every row).
    let mut report: Vec<String> = Vec::new();
    let mut pairs = Vec::new();
    for (where_, v) in &all {
        if !pairs.contains(&(v.fg, v.bg)) {
            pairs.push((v.fg, v.bg));
            report.push(format!("{where_}: {v}"));
        }
    }
    if let Some(path) = std::env::var_os("GK_CONTRAST_REPORT") {
        let _ = std::fs::write(path, report.join("\n"));
    }
    report.truncate(80);
    assert!(
        all.is_empty(),
        "{} hard-to-read texts (contrast below {}:1):\n{}",
        all.len(),
        min_ratio(),
        report.join("\n")
    );
}
