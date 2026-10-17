//! Headless end-to-end test of the history view: a real repository, a real
//! session and simulated egui input.

use crate::app::{Action, Shared};
use crate::tab::{RepoTab, View};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use gk_core::ObjectId;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@x",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn make_repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gk-history-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    for i in 0..4 {
        std::fs::write(dir.join("f.txt"), format!("v{i}\n")).unwrap();
        if i == 1 {
            std::fs::write(dir.join("g.txt"), "g\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    std::fs::write(dir.join("f.txt"), "dirty\n").unwrap();
    dir
}

struct Harness {
    ctx: egui::Context,
    tab: RepoTab,
    settings: gk_settings::Settings,
    log: gk_cli::CommandLog,
    time: f64,
}

impl Harness {
    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 0.02;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
            time: Some(self.time),
            modifiers,
            events,
            ..Default::default()
        };
        let snap = self.tab.session.snapshot();
        let mut actions: Vec<Action> = Vec::new();
        let mut show_log = false;
        let (tab, settings, log) = (&mut self.tab, &mut self.settings, &self.log);
        let _ = self.ctx.run_ui(input, |ui| {
            let mut shared = Shared {
                settings: &mut *settings,
                palette: crate::theme::Palette::dark(),
                log,
                actions: &mut actions,
                show_log: &mut show_log,
            };
            egui::CentralPanel::default().show(ui, |ui| match tab.view {
                View::Search => crate::views::search::show(ui, tab, &snap, &mut shared),
                _ => super::show(ui, tab, &snap, &mut shared),
            });
        });
    }

    fn idle(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(Vec::new(), Modifiers::NONE);
        }
    }

    fn row_pos(&self, row: usize) -> Pos2 {
        let r = self
            .ctx
            .read_response(super::list_id(&self.tab))
            .expect("list rendered")
            .rect;
        let h = self.settings.history.row_height.clamp(14.0, 60.0);
        pos2(r.left() + 300.0, r.top() + (row as f32 + 0.5) * h)
    }

    fn click(&mut self, row: usize, modifiers: Modifiers) {
        let pos = self.row_pos(row);
        self.frame(vec![Event::PointerMoved(pos)], modifiers);
        let b = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        };
        self.frame(vec![b(true)], modifiers);
        self.frame(vec![b(false)], modifiers);
        // Far apart in time, so consecutive clicks never form a double click.
        self.time += 5.0;
        self.idle(1);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let ev = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frame(vec![ev(true)], modifiers);
        self.frame(vec![ev(false)], modifiers);
    }

    fn selected(&self) -> Vec<ObjectId> {
        let snap = self.tab.session.snapshot();
        self.tab.history.sel.sorted(|k| snap.history.row(k))
    }
}

#[test]
fn history_view_selection_keyboard_and_worktree_row() {
    let dir = make_repo();
    let ctx = egui::Context::default();
    let settings = gk_settings::Settings::default();
    let log = gk_cli::CommandLog::default();
    let mut tab = RepoTab::open(&dir, &ctx, log.clone(), Vec::new(), &settings).expect("open repo");
    tab.view = View::History;
    let start = Instant::now();
    loop {
        let s = tab.session.snapshot();
        if s.history.len() == 4 && !s.status.is_clean() && !s.history_loading {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "repository did not load"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut h = Harness {
        ctx,
        tab,
        settings,
        log,
        time: 0.0,
    };
    h.idle(3);
    let ids = h.tab.session.snapshot().history.ids.clone();

    // HEAD (newest commit) is selected initially.
    assert_eq!(h.selected(), vec![ids[0]]);

    // Row 0 is "Uncommitted changes"; commits start at row 1.
    h.click(3, Modifiers::NONE);
    assert_eq!(h.selected(), vec![ids[2]]);

    h.key(Key::ArrowUp, Modifiers::NONE);
    assert_eq!(h.selected(), vec![ids[1]]);

    h.key(Key::ArrowDown, Modifiers::SHIFT);
    h.key(Key::ArrowDown, Modifiers::SHIFT);
    assert_eq!(h.selected(), vec![ids[1], ids[2], ids[3]]);

    h.key(Key::Home, Modifiers::NONE);
    assert_eq!(h.selected(), vec![ids[0]], "Home skips the worktree row");

    h.click(4, Modifiers::COMMAND);
    assert_eq!(h.selected(), vec![ids[0], ids[3]]);
    // Two selected: the details pane lists the files changed between them.
    let start = Instant::now();
    let paths = loop {
        h.idle(1);
        if let Some(p) = h.tab.details.compared_paths() {
            break p;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "compare did not load"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(paths, vec!["f.txt".to_string(), "g.txt".to_string()]);

    h.click(2, Modifiers::SHIFT);
    assert_eq!(h.selected(), vec![ids[1], ids[2], ids[3]]);

    // Right click inside the selection targets the whole selection.
    let pos = h.row_pos(2);
    h.frame(vec![Event::PointerMoved(pos)], Modifiers::NONE);
    let rb = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![rb(true)], Modifiers::NONE);
    h.frame(vec![rb(false)], Modifiers::NONE);
    h.idle(2);
    assert_eq!(h.selected(), vec![ids[1], ids[2], ids[3]]);
    assert_eq!(h.tab.history.menu_ids, vec![ids[1], ids[2], ids[3]]);
    h.frame(
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    h.time += 5.0;
    h.idle(2);

    // Right click outside the selection selects that commit only.
    let pos = h.row_pos(1);
    h.frame(vec![Event::PointerMoved(pos)], Modifiers::NONE);
    let rb = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![rb(true)], Modifiers::NONE);
    h.frame(vec![rb(false)], Modifiers::NONE);
    h.idle(2);
    assert_eq!(h.selected(), vec![ids[0]]);
    assert_eq!(h.tab.history.menu_ids, vec![ids[0]]);
    h.frame(
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    h.time += 5.0;
    h.idle(2);

    h.click(0, Modifiers::NONE);
    assert_eq!(h.tab.view, View::FileStatus);

    // Text search (debounced), then the results list.
    h.tab.search.start_query("c2".into());
    h.tab.view = View::Search;
    let wait = |h: &mut Harness, what: &str, f: &dyn Fn(&Harness) -> bool| {
        let start = Instant::now();
        while !f(h) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for {what}"
            );
            h.time += 0.1;
            h.idle(1);
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&mut h, "search results", &|h| {
        h.tab.search.rows.as_ref().is_some_and(|r| r.len() == 1)
    });
    h.idle(2);
    let pos = {
        let r = h
            .ctx
            .read_response(egui::Id::new(("search-list", &h.tab.path)))
            .expect("results list")
            .rect;
        pos2(r.left() + 300.0, r.top() + 5.0)
    };
    h.frame(vec![Event::PointerMoved(pos)], Modifiers::NONE);
    let b = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![b(true)], Modifiers::NONE);
    h.frame(vec![b(false)], Modifiers::NONE);
    let sel: Vec<ObjectId> = h.tab.search.sel.iter().copied().collect();
    assert_eq!(sel, vec![ids[1]], "c2 is the second newest commit");

    // File history lists every commit touching f.txt.
    crate::views::search::open_file_history(&mut h.tab, "f.txt".into());
    wait(&mut h, "file history", &|h| {
        h.tab
            .search
            .file_result
            .as_ref()
            .is_some_and(|f| f.entries.len() == 4)
    });
    assert_eq!(h.tab.search.path_at(&ids[3]).as_deref(), Some("f.txt"));

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}
