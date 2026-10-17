//! File status view: staged / unstaged lists, diff with hunk and line
//! staging, and the commit box (agent: status).
//!
//! Layout (SourceTree style): an operation banner on top while a merge /
//! rebase is in progress, the commit box at the bottom, the two file lists
//! on the left and the diff of the focused file on the right.
//!
//! Lists are virtualized and their row models are rebuilt only when the
//! status, filter, tree mode or collapsed folders change, so 100k changed
//! files cost nothing per frame.

mod commit;
mod list;
mod merge_tool;
mod tree;

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::RepoTab;
use crate::views::diff::{DiffMode, DiffView};
use egui::{Id, RichText, Ui};
use gk_core::content::DiffTarget;
use gk_core::ops::Side as MergeSide;
use gk_core::{MessageKind, RepoState, Snapshot};
use list::{Ev, ListCtx, ListState, Side};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub use commit::CommitBox;

/// Lossy UTF-8 paths of the current status, in status (byte) order.
#[derive(Default)]
struct Paths {
    status_gen: Option<u64>,
    staged: Vec<String>,
    unstaged: Vec<String>,
    conflicts: usize,
}

/// Index operation queued by this view: its paths are drawn dimmed until
/// the refreshed status arrives.
struct Pending {
    paths: HashSet<String>,
    all: Option<Side>,
    status_gen: u64,
    since: Instant,
}

#[derive(Default)]
pub struct StatusState {
    pub commit: CommitBox,
    pub diff: DiffView,
    staged: ListState,
    unstaged: ListState,
    /// The list whose cursor the diff shows.
    active: Side,
    filter: String,
    paths: Paths,
    pending: Option<Pending>,
    tools: merge_tool::MergeTools,
    /// Files whose merge tool exited: ask to mark them resolved.
    ask_resolved: Vec<String>,
    confirm_abort: bool,
}

impl StatusState {
    fn list(&self, side: Side) -> &ListState {
        match side {
            Side::Staged => &self.staged,
            Side::Unstaged => &self.unstaged,
        }
    }

    fn list_mut(&mut self, side: Side) -> &mut ListState {
        match side {
            Side::Staged => &mut self.staged,
            Side::Unstaged => &mut self.unstaged,
        }
    }

    /// Refreshes path caches and row models; keeps cursors valid.
    fn sync(&mut self, snap: &Snapshot, tree_mode: bool) {
        if self.paths.status_gen != Some(snap.status_gen) {
            self.paths.staged = snap.status.staged.iter().map(|e| e.path.to_string()).collect();
            self.paths.unstaged = snap.status.unstaged.iter().map(|e| e.path.to_string()).collect();
            self.paths.conflicts = snap.status.conflicts().count();
            self.paths.status_gen = Some(snap.status_gen);
        }
        let g = snap.status_gen;
        if self.staged.sync(g, &self.paths.staged, &self.filter, tree_mode) {
            self.staged.repair_cursor(&self.paths.staged);
        }
        if self.unstaged.sync(g, &self.paths.unstaged, &self.filter, tree_mode) {
            self.unstaged.repair_cursor(&self.paths.unstaged);
        }
        if let Some(p) = &self.pending {
            let done = snap.op.is_none() && !snap.status_loading && snap.status_gen != p.status_gen;
            if done || p.since.elapsed() > Duration::from_secs(20) {
                self.pending = None;
            }
        }
    }

    fn set_pending(&mut self, snap: &Snapshot, paths: &[String], all: Option<Side>) {
        let p = self.pending.get_or_insert_with(|| Pending {
            paths: HashSet::new(),
            all: None,
            status_gen: snap.status_gen,
            since: Instant::now(),
        });
        p.paths.extend(paths.iter().cloned());
        p.all = p.all.or(all);
        p.status_gen = snap.status_gen;
        p.since = Instant::now();
    }

    /// Diff target of the focused file.
    fn focused(&self, snap: &Snapshot) -> Option<(DiffTarget, DiffMode)> {
        let (entries, paths) = match self.active {
            Side::Staged => (&snap.status.staged, &self.paths.staged),
            Side::Unstaged => (&snap.status.unstaged, &self.paths.unstaged),
        };
        let cursor = self.list(self.active).cursor.as_ref()?;
        let e = entries.get(paths.binary_search(cursor).ok()?)?;
        let path = cursor.clone();
        Some(match self.active {
            Side::Staged => (
                DiffTarget::Staged { path, old_path: e.orig_path.as_ref().map(|o| o.to_string()) },
                DiffMode::Staged,
            ),
            Side::Unstaged => {
                let mode = if matches!(e.kind, gk_status::Kind::Conflict(_)) { DiffMode::ReadOnly } else { DiffMode::Unstaged };
                (DiffTarget::Unstaged { path }, mode)
            }
        })
    }
}

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    tab.status.sync(snap, tab.state.file_view_tree);
    collect_tool_exits(tab);
    let key = tab.path.clone();

    if snap.refs.state != RepoState::Clean {
        egui::Panel::top(Id::new(("fs-banner", &key))).show(ui, |ui| banner(ui, tab, snap, shared));
    }
    egui::Panel::bottom(Id::new(("fs-commit", &key)))
        .resizable(true)
        .default_size(170.0)
        .size_range(120.0..=520.0)
        .show(ui, |ui| commit::show(ui, tab, snap, shared));

    let mut events: Vec<(Side, Ev)> = Vec::new();
    egui::Panel::left(Id::new(("fs-lists", &key)))
        .resizable(true)
        .default_size(400.0)
        .size_range(220.0..=1200.0)
        .show(ui, |ui| lists(ui, tab, snap, shared, &mut events));
    for (side, ev) in events {
        handle(ui, tab, snap, shared, side, ev);
    }

    egui::CentralPanel::default().show(ui, |ui| {
        let st = &mut tab.status;
        match st.focused(snap) {
            Some((target, mode)) => st.diff.show(ui, &tab.session, &target, mode, snap.status_gen, shared),
            None => {
                st.diff.reset();
                ui.centered_and_justified(|ui| {
                    let text = if snap.status.is_clean() {
                        if snap.status_loading { "Reading status…" } else { "Nothing to commit — working tree clean" }
                    } else {
                        "Select a file to see its changes"
                    };
                    ui.label(RichText::new(text).color(shared.palette.muted));
                });
            }
        }
    });

    modals(ui, tab, snap, shared);
}

fn lists(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared, events: &mut Vec<(Side, Ev)>) {
    let p = shared.palette;
    ui.horizontal(|ui| {
        let tree = &mut tab.state.file_view_tree;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.toggle_value(tree, "Tree").on_hover_text("Group files by folder");
            if !tab.status.filter.is_empty() && ui.small_button("×").on_hover_text("Clear filter").clicked() {
                tab.status.filter.clear();
            }
            ui.add(
                egui::TextEdit::singleline(&mut tab.status.filter)
                    .hint_text("Filter files")
                    .desired_width(ui.available_width()),
            );
        });
    });
    // Rebuild immediately so the filter / mode change shows this frame.
    tab.status.sync(snap, tab.state.file_view_tree);

    let tree_mode = tab.state.file_view_tree;
    let has_tool = shared.settings.external_merge_tool.as_ref().is_some_and(|t| !t.command.trim().is_empty());
    let op_running = snap.op.is_some();
    let avail = ui.available_height();
    let key = tab.path.clone();

    let one = |ui: &mut Ui, st: &mut StatusState, side: Side, out: &mut Vec<(Side, Ev)>| {
        let (entries, total) = match side {
            Side::Staged => (&snap.status.staged, snap.status.staged.len()),
            Side::Unstaged => (&snap.status.unstaged, snap.status.unstaged.len()),
        };
        let filtered = !st.filter.trim().is_empty();
        let shown = st.list(side).model.files;
        let sel = st.list(side).target_count();
        let pending_here = st.pending.as_ref().is_some_and(|p| p.all == Some(side) || !p.paths.is_empty()) && op_running;
        let mut evs: Vec<Ev> = Vec::new();
        ui.horizontal(|ui| {
            let title = match side {
                Side::Staged => "Staged files",
                Side::Unstaged => "Unstaged files",
            };
            ui.label(RichText::new(title).strong());
            let count = if filtered { format!("({shown} of {total})") } else { format!("({total})") };
            ui.label(RichText::new(count).color(p.muted));
            if pending_here {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (all, some) = match side {
                    Side::Staged => ("Unstage all", "Unstage selected"),
                    Side::Unstaged => ("Stage all", "Stage selected"),
                };
                if ui.add_enabled(total > 0, egui::Button::new(all).small()).clicked() {
                    evs.push(Ev::All);
                }
                if ui.add_enabled(sel > 0, egui::Button::new(some).small()).clicked() {
                    evs.push(Ev::Toggle(st.list(side).targets()));
                }
            });
        });
        let paths: &[String] = match side {
            Side::Staged => &st.paths.staged,
            Side::Unstaged => &st.paths.unstaged,
        };
        let pending = |path: &str| st.pending.as_ref().is_some_and(|p| p.all == Some(side) || p.paths.contains(path));
        let ctx = ListCtx {
            side,
            entries,
            paths,
            palette: p,
            tree: tree_mode,
            active: st.active == side,
            pending: &pending,
            has_merge_tool: has_tool,
        };
        let ls = match side {
            Side::Staged => &mut st.staged,
            Side::Unstaged => &mut st.unstaged,
        };
        list::show(ui, ls, &ctx, &mut evs);
        out.extend(evs.into_iter().map(|e| (side, e)));
    };

    egui::Panel::top(Id::new(("fs-staged", &key)))
        .resizable(true)
        .default_size((avail * 0.4).max(90.0))
        .size_range(70.0..=(avail - 90.0).max(90.0))
        .show(ui, |ui| one(ui, &mut tab.status, Side::Staged, events));
    egui::CentralPanel::no_frame().show(ui, |ui| {
        ui.add_space(4.0);
        one(ui, &mut tab.status, Side::Unstaged, events)
    });
}

fn handle(ui: &Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared, side: Side, ev: Ev) {
    let session = tab.session.clone();
    match ev {
        Ev::Activate => {
            let st = &mut tab.status;
            st.active = side;
            let other = match side {
                Side::Staged => Side::Unstaged,
                Side::Unstaged => Side::Staged,
            };
            st.list_mut(other).clear();
        }
        Ev::FocusDiff => tab.status.diff.request_focus(),
        Ev::Toggle(paths) => {
            if paths.is_empty() {
                return;
            }
            tab.status.set_pending(snap, &paths, None);
            let n = paths.len();
            let what = if n == 1 { paths[0].clone() } else { format!("{n} files") };
            match side {
                Side::Unstaged => session.run_index_op(format!("Stage {what}"), move |r| r.stage(&paths)),
                Side::Staged => session.run_index_op(format!("Unstage {what}"), move |r| r.unstage(&paths)),
            }
        }
        Ev::All => {
            tab.status.set_pending(snap, &[], Some(side));
            match side {
                Side::Unstaged => session.run_index_op("Stage all", |r| r.stage_all()),
                Side::Staged => session.run_index_op("Unstage all", |r| r.unstage_all()),
            }
        }
        Ev::Discard { tracked, untracked } => {
            if !tracked.is_empty() || !untracked.is_empty() {
                tab.dialog = Some(Dialog::discard_paths(tracked, untracked));
            }
        }
        Ev::Stash(paths) => tab.dialog = Some(Dialog::stash_paths(paths)),
        Ev::Ignore(patterns) => {
            let label = format!("Ignore {}", patterns.join(", "));
            session.run_index_op(label, move |r| r.ignore(&patterns));
        }
        Ev::Open(p) => {
            if let Err(e) = open::that(session.repo().abs(&p)) {
                session.push_message(MessageKind::Error, format!("Cannot open {p}: {e}"));
            }
        }
        Ev::Reveal(p) => reveal(&session.repo().abs(&p)),
        Ev::Copy(text) => ui.ctx().copy_text(text),
        Ev::History(p) => crate::views::search::open_file_history(tab, p),
        Ev::Blame(p) => crate::views::blame::open_blame(tab, p, None),
        Ev::ResolveOurs(paths) => {
            tab.status.set_pending(snap, &paths, None);
            session.run_index_op("Resolve using mine", move |r| r.resolve_with(&paths, MergeSide::Ours));
        }
        Ev::ResolveTheirs(paths) => {
            tab.status.set_pending(snap, &paths, None);
            session.run_index_op("Resolve using theirs", move |r| r.resolve_with(&paths, MergeSide::Theirs));
        }
        Ev::MarkResolved(paths) => {
            tab.status.set_pending(snap, &paths, None);
            session.run_index_op("Mark resolved", move |r| r.mark_resolved(&paths));
        }
        Ev::MergeEditor(p) => crate::views::merge::open_merge(tab, p),
        Ev::MergeTool(p) => match shared.settings.external_merge_tool.clone() {
            Some(tool) if !tool.command.trim().is_empty() => {
                tab.status.tools.launch(tool, session.repo().clone(), p, ui.ctx().clone());
            }
            _ => session.push_message(MessageKind::Error, "No external merge tool is configured (Settings)."),
        },
    }
}

fn reveal(abs: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", abs.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(abs.parent().unwrap_or(abs));
    }
}

fn collect_tool_exits(tab: &mut RepoTab) {
    for exit in tab.status.tools.take_finished() {
        match exit.error {
            None => {
                if !tab.status.ask_resolved.contains(&exit.path) {
                    tab.status.ask_resolved.push(exit.path);
                }
            }
            Some(e) => tab.session.push_message(MessageKind::Error, format!("Merge tool for {}: {e}", exit.path)),
        }
    }
}

fn banner(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &Shared) {
    let p = shared.palette;
    let state = snap.refs.state;
    let conflicts = tab.status.paths.conflicts;
    let (title, can_skip, can_continue) = match state {
        RepoState::Clean => return,
        RepoState::Merging => ("Merge in progress", false, true),
        RepoState::Rebasing { interactive: true } => ("Interactive rebase in progress", true, true),
        RepoState::Rebasing { .. } => ("Rebase in progress", true, true),
        RepoState::CherryPicking => ("Cherry-pick in progress", true, true),
        RepoState::Reverting => ("Revert in progress", true, true),
        RepoState::ApplyingMailbox => ("Applying patches (git am)", true, true),
        RepoState::Bisecting => ("Bisect in progress", false, false),
    };
    let busy = snap.op.is_some();
    egui::Frame::new()
        .fill(p.conflict.gamma_multiply(0.15))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠").color(p.conflict).strong());
                ui.label(RichText::new(title).strong());
                if conflicts > 0 {
                    ui.label(
                        RichText::new(format!("{conflicts} conflicted file(s) — resolve them, then continue")).color(p.conflict),
                    );
                    if tab.status.tools.is_running() {
                        ui.spinner();
                        ui.label(RichText::new("merge tool open").color(p.muted));
                    }
                } else if can_continue {
                    ui.label(RichText::new("All conflicts resolved").color(p.muted));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let abort = if state == RepoState::Bisecting { "Reset" } else { "Abort" };
                    if ui.add_enabled(!busy, egui::Button::new(abort)).clicked() {
                        tab.status.confirm_abort = true;
                    }
                    if can_skip && ui.add_enabled(!busy, egui::Button::new("Skip")).on_hover_text("Skip the current commit").clicked() {
                        tab.session.run_op("Skip", move |r, _, _| r.skip_op(state));
                    }
                    if can_continue {
                        let b = ui
                            .add_enabled(!busy && conflicts == 0, egui::Button::new(RichText::new("Continue").strong()))
                            .on_disabled_hover_text("Resolve all conflicts first");
                        if b.clicked() {
                            tab.session.run_op("Continue", move |r, _, _| r.continue_op(state));
                        }
                    }
                });
            });
        });
}

fn modals(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &Shared) {
    let p = shared.palette;
    if tab.status.confirm_abort {
        let state = snap.refs.state;
        let mut decision: Option<bool> = None;
        let resp = egui::Modal::new(Id::new(("fs-abort", &tab.path))).show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            ui.heading("Abort operation");
            ui.label(format!("Abort the {}? Conflict resolutions made so far are lost.", state.label().to_lowercase()));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Abort").color(p.removed_fg)).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if resp.should_close() && decision.is_none() {
            decision = Some(false);
        }
        if let Some(d) = decision {
            tab.status.confirm_abort = false;
            if d && state != RepoState::Clean {
                tab.session.run_op("Abort", move |r, _, _| r.abort_op(state).map(|_| String::new()));
            }
        }
    }

    if let Some(path) = tab.status.ask_resolved.first().cloned() {
        let mut decision: Option<bool> = None;
        let resp = egui::Modal::new(Id::new(("fs-mark-resolved", &tab.path))).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.heading("Merge tool closed");
            ui.label(format!("Mark {path} as resolved?"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Mark resolved").clicked() {
                    decision = Some(true);
                }
                if ui.button("Not yet").clicked() {
                    decision = Some(false);
                }
            });
        });
        if resp.should_close() && decision.is_none() {
            decision = Some(false);
        }
        if let Some(d) = decision {
            tab.status.ask_resolved.remove(0);
            if d {
                let paths = vec![path];
                tab.status.set_pending(snap, &paths, None);
                tab.session.run_index_op("Mark resolved", move |r| r.mark_resolved(&paths));
            }
        }
    }
}

#[cfg(test)]
mod smoke {
    //! Headless frames over a real repository: lists, tree mode, unified and
    //! side-by-side diffs must render without panicking.
    use super::*;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").args(args).current_dir(dir).output().map(|o| o.status.success());
        assert!(matches!(ok, Ok(true)), "git {args:?} failed");
    }

    #[test]
    fn renders_headless() {
        let root = std::env::temp_dir().join(format!("gk-fs-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "core.autocrlf", "false"]);
        let old: String = (0..60).map(|i| format!("line {i}\tx\n")).collect();
        std::fs::write(repo.join("f.txt"), &old).unwrap();
        std::fs::write(repo.join("src/deep/a.rs"), "fn a() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let new = old.replace("line 3\t", "LINE three\t").replace("line 50\tx\n", "") + "tail";
        std::fs::write(repo.join("f.txt"), new).unwrap();
        std::fs::write(repo.join("src/deep/a.rs"), "fn a() { b() }\n").unwrap();
        std::fs::write(repo.join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
        git(&repo, &["add", "src"]);
        unsafe { std::env::set_var("GITKWAKEN_CONFIG_DIR", root.join("cfg")) };

        let ctx = egui::Context::default();
        let mut settings = gk_settings::Settings::default();
        let log = gk_cli::CommandLog::default();
        let mut tab = RepoTab::open(&repo, &ctx, log.clone(), Vec::new(), &settings).unwrap();
        let mut actions = Vec::new();
        let mut show_log = false;
        let mut frames = |tab: &mut RepoTab, settings: &mut gk_settings::Settings, n: usize| {
            for _ in 0..n {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                    ..Default::default()
                };
                let snap = tab.session.snapshot();
                let _ = ctx.run_ui(input, |ui| {
                    let mut shared = Shared {
                        settings: &mut *settings,
                        palette: crate::theme::Palette::dark(),
                        log: &log,
                        actions: &mut actions,
                        show_log: &mut show_log,
                    };
                    show(ui, tab, &snap, &mut shared);
                });
                std::thread::sleep(Duration::from_millis(15));
            }
        };

        // Wait for the first status.
        for _ in 0..200 {
            frames(&mut tab, &mut settings, 1);
            let s = tab.session.snapshot();
            if !s.status.unstaged.is_empty() && !s.status.staged.is_empty() {
                break;
            }
        }
        frames(&mut tab, &mut settings, 2);
        let snap = tab.session.snapshot();
        assert!(snap.status.unstaged.iter().any(|e| e.path == "f.txt"));

        let row = tab.status.unstaged.row_of("f.txt", &tab.status.paths.unstaged).unwrap();
        tab.status.unstaged.select_only("f.txt".into(), row);
        tab.status.active = Side::Unstaged;
        assert!(matches!(tab.status.focused(&snap), Some((DiffTarget::Unstaged { .. }, DiffMode::Unstaged))));
        frames(&mut tab, &mut settings, 40);
        let (lines, rows, _) = tab.status.diff.shown_stats().expect("diff loaded");
        assert!(lines >= 4 && rows > lines, "unified rows include hunk headers");

        // Tree mode, side by side, whole status.
        tab.state.file_view_tree = true;
        settings.diff.side_by_side = true;
        tab.status.diff = DiffView::default();
        frames(&mut tab, &mut settings, 40);
        let (lines, rows, bin) = tab.status.diff.shown_stats().expect("side-by-side diff loaded");
        assert!(lines > 0 && rows > 0 && !bin);
        assert!(tab.status.unstaged.model.rows.iter().any(|r| matches!(r, list::Row::Dir { .. })) || tab.status.staged.model.tree.is_some());

        // Binary file and a staged file.
        let row = tab.status.unstaged.row_of("bin.dat", &tab.status.paths.unstaged).unwrap();
        tab.status.unstaged.select_only("bin.dat".into(), row);
        frames(&mut tab, &mut settings, 20);
        assert!(tab.status.diff.shown_stats().is_some_and(|s| s.2), "binary diff");
        let row = tab.status.staged.row_of("src/deep/a.rs", &tab.status.paths.staged).unwrap();
        tab.status.staged.select_only("src/deep/a.rs".into(), row);
        tab.status.active = Side::Staged;
        frames(&mut tab, &mut settings, 30);

        // Filter down to nothing.
        tab.status.filter = "no-such-file".into();
        frames(&mut tab, &mut settings, 5);
        assert_eq!(tab.status.unstaged.model.files, 0);

        drop(tab);
        let _ = std::fs::remove_dir_all(&root);
    }
}
