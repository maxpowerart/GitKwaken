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
mod conflict_guard;
mod dnd;
mod filter;
mod ignore;
mod list;
mod merge_tool;
mod tree;

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::RepoTab;
use crate::views::diff::{DiffMode, DiffView};
use conflict_guard::{Answer, StageOp};
use dnd::DragFiles;
use egui::{Id, RichText, Sense, Ui};
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
    /// Files per status chip bit (both lists), see [`list::KIND_FILTERS`].
    kind_counts: [usize; 8],
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
    /// File to focus once the status has loaded (see [`Self::preselect`]).
    preselect: Option<String>,
    /// Stage requests touching conflicted files, checked for markers first.
    guard: conflict_guard::Guard,
    /// Status filter chips ([`list::KIND_FILTERS`] mask; 0 = all).
    kinds: u8,
    /// "Ignore › Custom pattern…" editor.
    ignore_custom: Option<ignore::IgnoreCustom>,
    /// Stage all (true) / unstage all (false) requested by a shortcut.
    want_all: Option<bool>,
}

impl StatusState {
    /// Focuses `path` (unstaged first, else staged) as soon as it shows up
    /// in the status. Used by `GITKWAKEN_SELECT` (see `tab.rs`).
    pub fn preselect(&mut self, path: String) {
        self.preselect = Some(path);
    }

    /// Stage (`true`) or unstage (`false`) everything on the next frame of
    /// the view (keyboard shortcuts; conflicted files are still checked).
    pub fn request_all(&mut self, stage: bool) {
        self.want_all = Some(stage);
    }

    fn apply_preselect(&mut self) {
        let Some(path) = self.preselect.as_deref() else {
            return;
        };
        for side in [Side::Unstaged, Side::Staged] {
            let (ls, paths) = match side {
                Side::Staged => (&mut self.staged, &self.paths.staged),
                Side::Unstaged => (&mut self.unstaged, &self.paths.unstaged),
            };
            if let Some(row) = ls.row_of(path, paths) {
                ls.select_only(path.to_string(), row);
                self.active = side;
                self.preselect = None;
                return;
            }
        }
    }

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
            self.paths.staged = snap
                .status
                .staged
                .iter()
                .map(|e| e.path.to_string())
                .collect();
            self.paths.unstaged = snap
                .status
                .unstaged
                .iter()
                .map(|e| e.path.to_string())
                .collect();
            self.paths.conflicts = snap.status.conflicts().count();
            let mut counts = [0usize; 8];
            for e in snap.status.staged.iter().chain(snap.status.unstaged.iter()) {
                counts[list::kind_bit(e.kind).trailing_zeros() as usize] += 1;
            }
            self.paths.kind_counts = counts;
            self.paths.status_gen = Some(snap.status_gen);
        }
        let g = snap.status_gen;
        if self.staged.sync(
            g,
            &self.paths.staged,
            &snap.status.staged,
            &self.filter,
            self.kinds,
            tree_mode,
        ) {
            self.staged.repair_cursor(&self.paths.staged);
        }
        if self.unstaged.sync(
            g,
            &self.paths.unstaged,
            &snap.status.unstaged,
            &self.filter,
            self.kinds,
            tree_mode,
        ) {
            self.unstaged.repair_cursor(&self.paths.unstaged);
        }
        if let Some(p) = &self.pending {
            let done = snap.op.is_none() && !snap.status_loading && snap.status_gen != p.status_gen;
            if done || p.since.elapsed() > Duration::from_secs(20) {
                self.pending = None;
            }
        }
        if self.preselect.is_some() && !snap.status_loading {
            self.apply_preselect();
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

    /// The conflicted files among unstaged `paths`.
    fn conflicted_among(&self, snap: &Snapshot, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .filter(|p| {
                self.paths
                    .unstaged
                    .binary_search(p)
                    .ok()
                    .and_then(|i| snap.status.unstaged.get(i))
                    .is_some_and(|e| matches!(e.kind, gk_status::Kind::Conflict(_)))
            })
            .cloned()
            .collect()
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
                DiffTarget::Staged {
                    path,
                    old_path: e.orig_path.as_ref().map(|o| o.to_string()),
                },
                DiffMode::Staged,
            ),
            Side::Unstaged => {
                let mode = if matches!(e.kind, gk_status::Kind::Conflict(_)) {
                    DiffMode::ReadOnly
                } else {
                    DiffMode::Unstaged
                };
                (DiffTarget::Unstaged { path }, mode)
            }
        })
    }
}

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    tab.status.sync(snap, tab.state.file_view_tree);
    collect_tool_exits(tab);
    for op in tab.status.guard.poll() {
        run_stage(tab, snap, op);
    }
    if let Some(stage) = tab.status.want_all.take() {
        let side = if stage { Side::Unstaged } else { Side::Staged };
        let any = match side {
            Side::Staged => !snap.status.staged.is_empty(),
            Side::Unstaged => !snap.status.unstaged.is_empty(),
        };
        if any {
            handle(ui, tab, snap, shared, side, Ev::All);
        }
    }
    let key = tab.path.clone();

    if snap.refs.state != RepoState::Clean {
        egui::Panel::top(Id::new(("fs-banner", &key))).show(ui, |ui| banner(ui, tab, snap, shared));
    }
    egui::Panel::top(Id::new(("fs-top", &key)))
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            left: 0,
            right: 0,
            top: 2,
            bottom: 4,
        }))
        .show(ui, |ui| top_row(ui, tab, shared.palette));
    // Rebuild immediately so the filter / mode change shows this frame.
    tab.status.sync(snap, tab.state.file_view_tree);
    use crate::views::panel_size::{Axis, remember, restore};
    let commit_id = Id::new(("fs-commit", &key));
    let commit_limit = ui.available_height().min(520.0);
    let layout = tab.state.layout;
    restore(
        ui.ctx(),
        commit_id,
        Axis::Height,
        layout.commit_height,
        commit_limit,
    );
    let commit = egui::Panel::bottom(commit_id)
        .resizable(true)
        .default_size(170.0)
        .size_range(120.0..=520.0)
        .show(ui, |ui| commit::show(ui, tab, snap, shared));
    remember(
        commit.response.rect,
        Axis::Height,
        commit_limit,
        &mut tab.state.layout.commit_height,
    );

    let mut events: Vec<(Side, Ev)> = Vec::new();
    let lists_id = Id::new(("fs-lists", &key));
    let lists_limit = ui.available_width().min(1200.0);
    restore(
        ui.ctx(),
        lists_id,
        Axis::Width,
        layout.file_list_width,
        lists_limit,
    );
    let shown = egui::Panel::left(lists_id)
        .resizable(true)
        .default_size((ui.available_width() * 0.42).clamp(220.0, 400.0))
        .size_range(220.0..=1200.0)
        .show(ui, |ui| lists(ui, tab, snap, shared, &mut events));
    remember(
        shown.response.rect,
        Axis::Width,
        lists_limit,
        &mut tab.state.layout.file_list_width,
    );
    for (side, ev) in events {
        handle(ui, tab, snap, shared, side, ev);
    }

    egui::CentralPanel::default().show(ui, |ui| {
        let st = &mut tab.status;
        match st.focused(snap) {
            Some((target, mode)) => {
                st.diff
                    .show(ui, &tab.session, &target, mode, snap.status_gen, shared)
            }
            None => {
                st.diff.reset();
                ui.centered_and_justified(|ui| {
                    let text = if snap.status.is_clean() {
                        if snap.status_loading {
                            "Reading status…"
                        } else {
                            "Nothing to commit — working tree clean"
                        }
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

fn lists(
    ui: &mut Ui,
    tab: &mut RepoTab,
    snap: &Snapshot,
    shared: &mut Shared,
    events: &mut Vec<(Side, Ev)>,
) {
    let p = shared.palette;

    let tree_mode = tab.state.file_view_tree;
    let has_tool = shared
        .settings
        .external_merge_tool
        .as_ref()
        .is_some_and(|t| !t.command.trim().is_empty());
    let has_diff_tool = shared
        .settings
        .external_diff_tool
        .as_ref()
        .is_some_and(|t| !t.command.trim().is_empty());
    let row_pad = crate::theme::Density::of(shared.settings.compact).file_row_pad();
    let op_running = snap.op.is_some();
    let avail = ui.available_height();
    let key = tab.path.clone();
    let dnd_scope = Id::new(("fs-dnd", &key));

    // Returns true while the list accepts a hovering drag payload.
    let one = |ui: &mut Ui, st: &mut StatusState, side: Side, out: &mut Vec<(Side, Ev)>| {
        let zone = ui.max_rect();
        let (entries, total) = match side {
            Side::Staged => (&snap.status.staged, snap.status.staged.len()),
            Side::Unstaged => (&snap.status.unstaged, snap.status.unstaged.len()),
        };
        let filtered = !st.filter.trim().is_empty() || st.kinds != 0;
        let shown = st.list(side).model.files;
        let sel = st.list(side).target_count();
        let index_op = st
            .pending
            .as_ref()
            .is_some_and(|p| p.all == Some(side) || !p.paths.is_empty())
            && op_running;
        // Conflicted files being checked for markers before staging.
        let checking = side == Side::Unstaged && st.guard.is_busy();
        let pending_here = index_op || checking;
        let mut evs: Vec<Ev> = Vec::new();
        // SourceTree header band: "Staged files (N)", then "Unstage All"
        // and "Unstage Selected" on the right.
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                // Buttons first (right to left), then the title in the room left over,
                // cut with "…" rather than drawn under the buttons. One row high.
                let row_h = ui
                    .spacing()
                    .interact_size
                    .y
                    .max(ui.text_style_height(&egui::TextStyle::Body) + 4.0);
                let width = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(width, row_h),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let (all, some) = match side {
                            Side::Staged => ("Unstage All", "Unstage Selected"),
                            Side::Unstaged => ("Stage All", "Stage Selected"),
                        };
                        // Right to left: "… Selected" ends up rightmost.
                        if ui
                            .add_enabled(sel > 0, egui::Button::new(some).small())
                            .clicked()
                        {
                            evs.push(Ev::Toggle(st.list(side).targets()));
                        }
                        if ui
                            .add_enabled(total > 0, egui::Button::new(all).small())
                            .clicked()
                        {
                            evs.push(Ev::All);
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let title = match side {
                                Side::Staged => "Staged files",
                                Side::Unstaged => "Unstaged files",
                            };
                            let count = if filtered {
                                format!(" ({shown} of {total})")
                            } else {
                                format!(" ({total})")
                            };
                            let font = egui::TextStyle::Body.resolve(ui.style());
                            let mut job = egui::text::LayoutJob::default();
                            let strong = ui.visuals().strong_text_color();
                            job.append(title, 0.0, egui::TextFormat::simple(font.clone(), strong));
                            job.append(&count, 0.0, egui::TextFormat::simple(font, p.muted));
                            let r = ui.add(egui::Label::new(job).truncate());
                            if r.hovered() {
                                r.on_hover_text(format!("{title}{count}"));
                            }
                            if pending_here {
                                ui.spinner();
                            }
                        });
                    },
                );
            });
        let paths: &[String] = match side {
            Side::Staged => &st.paths.staged,
            Side::Unstaged => &st.paths.unstaged,
        };
        let pending = |path: &str| {
            st.pending
                .as_ref()
                .is_some_and(|p| p.all == Some(side) || p.paths.contains(path))
        };
        let ctx = ListCtx {
            side,
            entries,
            paths,
            palette: p,
            tree: tree_mode,
            active: st.active == side,
            pending: &pending,
            has_merge_tool: has_tool,
            dnd_scope,
            has_diff_tool,
            row_pad,
        };
        let ls = match side {
            Side::Staged => &mut st.staged,
            Side::Unstaged => &mut st.unstaged,
        };
        list::show(ui, ls, &ctx, &mut evs);
        out.extend(evs.into_iter().map(|e| (side, e)));

        // Drop zone: the whole list panel, header included. Hover-only, so
        // rows underneath keep their clicks.
        let zone_resp = ui.interact(zone, Id::new(("fs-drop", &key, side)), Sense::hover());
        let accepting = zone_resp
            .dnd_hover_payload::<DragFiles>()
            .is_some_and(|pl| dnd::accepts(&pl, dnd_scope, side));
        if accepting {
            dnd::paint_target(ui, zone, p);
        }
        if let Some(pl) = zone_resp.dnd_release_payload::<DragFiles>() {
            let source: &[String] = match pl.from {
                Side::Staged => &st.paths.staged,
                Side::Unstaged => &st.paths.unstaged,
            };
            if let Some(paths) = dnd::drop_paths(&pl, dnd_scope, side, source) {
                out.push((pl.from, Ev::Toggle(paths)));
            }
        }
        accepting
    };

    // SourceTree puts the staged list on top, Fork the unstaged one (Settings › General).
    let (first, second) = if shared.settings.staged_first {
        (Side::Staged, Side::Unstaged)
    } else {
        (Side::Unstaged, Side::Staged)
    };
    use crate::views::panel_size::{Axis, remember, restore};
    let first_id = Id::new(("fs-first", &key, first));
    let first_limit = (avail - 90.0).max(90.0);
    let saved = tab.state.layout.file_split_height;
    restore(ui.ctx(), first_id, Axis::Height, saved, first_limit);
    let first_panel = egui::Panel::top(first_id)
        .resizable(true)
        .default_size((avail * 0.4).max(90.0))
        .size_range(70.0..=(avail - 90.0).max(90.0))
        // No inner margin: keep the top list aligned with the bottom one.
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            bottom: 4,
            ..Default::default()
        }))
        .show(ui, |ui| one(ui, &mut tab.status, first, events));
    remember(
        first_panel.response.rect,
        Axis::Height,
        first_limit,
        &mut tab.state.layout.file_split_height,
    );
    let over_first = first_panel.inner;
    let over_second = egui::CentralPanel::no_frame()
        .show(ui, |ui| {
            ui.add_space(4.0);
            one(ui, &mut tab.status, second, events)
        })
        .inner;
    let (over_staged, over_unstaged) = if first == Side::Staged {
        (over_first, over_second)
    } else {
        (over_second, over_first)
    };

    if let Some(pl) = egui::DragAndDrop::payload::<DragFiles>(ui.ctx())
        && pl.scope == dnd_scope
    {
        let over = if over_staged {
            Some(Side::Staged)
        } else if over_unstaged {
            Some(Side::Unstaged)
        } else {
            None
        };
        dnd::paint_ghost(ui, &pl, over, p);
    }
}

/// "Pending files, sorted by path ▾" label for a [`list::KIND_FILTERS`] mask.
fn show_label(kinds: u8) -> String {
    let what = match list::KIND_FILTERS.iter().find(|(bit, ..)| *bit == kinds) {
        _ if kinds == 0 => "Pending",
        Some((_, _, label)) => label,
        None => "Filtered",
    };
    format!("{what} files, sorted by path")
}

/// SourceTree's File Status header row: the "Pending files ▾" combo and the
/// list-mode button on the left, then the fuzzy filter and status chips.
fn top_row(ui: &mut Ui, tab: &mut RepoTab, p: crate::theme::Palette) {
    ui.horizontal(|ui| {
        let kinds = &mut tab.status.kinds;
        egui::ComboBox::from_id_salt(("fs-show", &tab.path))
            .selected_text(show_label(*kinds))
            .width(230.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(kinds, 0, "Pending files");
                for &(bit, _, label) in list::KIND_FILTERS.iter() {
                    ui.selectable_value(kinds, bit, format!("{label} files"));
                }
            })
            .response
            .on_hover_text("Which changed files to list (combine with the chips on the right)");
        let tree = &mut tab.state.file_view_tree;
        let icon = if *tree { "🌲 ▾" } else { "☰ ▾" };
        ui.menu_button(icon, |ui| {
            if ui.selectable_label(!*tree, "☰ Flat list").clicked() {
                *tree = false;
                ui.close();
            }
            if ui.selectable_label(*tree, "🌲 Tree view").clicked() {
                *tree = true;
                ui.close();
            }
        })
        .response
        .on_hover_text("List mode: flat list or folder tree");
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut tab.status.filter)
                .hint_text("Filter files (fuzzy)")
                .desired_width(220.0),
        );
        if !tab.status.filter.is_empty()
            && ui.small_button("×").on_hover_text("Clear filter").clicked()
        {
            tab.status.filter.clear();
        }
        kind_chips(ui, &mut tab.status.kinds, &tab.status.paths.kind_counts, p);
    });
}

/// Status filter chips (M A D R ? U), combinable; laid out left to right.
fn kind_chips(ui: &mut Ui, kinds: &mut u8, counts: &[usize; 8], p: crate::theme::Palette) {
    ui.spacing_mut().item_spacing.x = 2.0;
    for &(bit, code, label) in list::KIND_FILTERS.iter() {
        let n = counts[bit.trailing_zeros() as usize];
        let on = *kinds & bit != 0;
        let text = RichText::new(code).monospace().color(if n == 0 && !on {
            p.muted.gamma_multiply(0.6)
        } else {
            p.muted
        });
        let text = if on { text.strong() } else { text };
        if ui
            .selectable_label(on, text)
            .on_hover_text(format!("Show only {label} files ({n}); combine several"))
            .clicked()
        {
            *kinds ^= bit;
        }
    }
    ui.add_space(4.0);
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
            match side {
                Side::Unstaged => {
                    let conflicted = tab.status.conflicted_among(snap, &paths);
                    guarded_stage(ui, tab, snap, StageOp::Paths(paths), conflicted);
                }
                Side::Staged => {
                    tab.status.set_pending(snap, &paths, None);
                    let what = what(&paths);
                    session.run_index_op(format!("Unstage {what}"), move |r| r.unstage(&paths))
                }
            }
        }
        Ev::All => match side {
            Side::Unstaged => {
                let conflicted: Vec<String> = snap
                    .status
                    .conflicts()
                    .map(|e| e.path.to_string())
                    .collect();
                guarded_stage(ui, tab, snap, StageOp::All, conflicted);
            }
            Side::Staged => {
                tab.status.set_pending(snap, &[], Some(side));
                session.run_index_op("Unstage all", |r| r.unstage_all());
            }
        },
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
            session.run_index_op("Resolve using mine", move |r| {
                r.resolve_with(&paths, MergeSide::Ours)
            });
        }
        Ev::ResolveTheirs(paths) => {
            tab.status.set_pending(snap, &paths, None);
            session.run_index_op("Resolve using theirs", move |r| {
                r.resolve_with(&paths, MergeSide::Theirs)
            });
        }
        Ev::MarkResolved(paths) => {
            tab.status.set_pending(snap, &paths, None);
            session.run_index_op("Mark resolved", move |r| r.mark_resolved(&paths));
        }
        Ev::MergeEditor(p) => crate::views::merge::open_merge(tab, p),
        Ev::IgnoreCustom(suggestion) => {
            tab.status.ignore_custom = Some(ignore::IgnoreCustom::new(suggestion));
        }
        Ev::ExternalDiff(path) => {
            let target = match side {
                Side::Staged => DiffTarget::Staged {
                    old_path: tab
                        .status
                        .paths
                        .staged
                        .binary_search(&path)
                        .ok()
                        .and_then(|i| snap.status.staged.get(i))
                        .and_then(|e| e.orig_path.as_ref().map(|o| o.to_string())),
                    path,
                },
                Side::Unstaged => DiffTarget::Unstaged { path },
            };
            crate::views::diff::open_external(&session, &target, shared);
        }
        Ev::MergeTool(p) => match shared.settings.external_merge_tool.clone() {
            Some(tool) if !tool.command.trim().is_empty() => {
                tab.status
                    .tools
                    .launch(tool, session.repo().clone(), p, ui.ctx().clone());
            }
            _ => session.push_message(
                MessageKind::Error,
                "No external merge tool is configured (Settings).",
            ),
        },
    }
}

fn what(paths: &[String]) -> String {
    match paths {
        [one] => one.clone(),
        _ => format!("{} files", paths.len()),
    }
}

/// Stages `op`, unless `conflicted` files are involved: those are first
/// checked for leftover conflict markers in the background (see
/// [`conflict_guard`]); the stage runs once they are clean or confirmed.
fn guarded_stage(
    ui: &Ui,
    tab: &mut RepoTab,
    snap: &Snapshot,
    op: StageOp,
    conflicted: Vec<String>,
) {
    if conflicted.is_empty() {
        return run_stage(tab, snap, op);
    }
    let repo = tab.session.repo();
    let files = conflicted
        .into_iter()
        .map(|p| {
            let abs = repo.abs(&p);
            (p, abs)
        })
        .collect();
    tab.status.guard.check(op, files, ui.ctx());
}

/// Runs a stage request (the unstaged list's Stage / Stage all).
fn run_stage(tab: &mut RepoTab, snap: &Snapshot, op: StageOp) {
    let session = tab.session.clone();
    match op {
        StageOp::Paths(paths) => {
            if paths.is_empty() {
                return;
            }
            tab.status.set_pending(snap, &paths, None);
            let what = what(&paths);
            session.run_index_op(format!("Stage {what}"), move |r| r.stage(&paths));
        }
        StageOp::All => {
            tab.status.set_pending(snap, &[], Some(Side::Unstaged));
            session.run_index_op("Stage all", |r| r.stage_all());
        }
    }
}

fn reveal(abs: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", abs.display()))
            .spawn();
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
            Some(e) => tab.session.push_message(
                MessageKind::Error,
                format!("Merge tool for {}: {e}", exit.path),
            ),
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
                        RichText::new(format!(
                            "{conflicts} conflicted file(s) — resolve them, then continue"
                        ))
                        .color(p.conflict),
                    );
                    if tab.status.tools.is_running() {
                        ui.spinner();
                        ui.label(RichText::new("merge tool open").color(p.muted));
                    }
                } else if can_continue {
                    ui.label(RichText::new("All conflicts resolved").color(p.muted));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let abort = if state == RepoState::Bisecting {
                        "Reset"
                    } else {
                        "Abort"
                    };
                    if ui.add_enabled(!busy, egui::Button::new(abort)).clicked() {
                        tab.status.confirm_abort = true;
                    }
                    if can_skip
                        && ui
                            .add_enabled(!busy, egui::Button::new("Skip"))
                            .on_hover_text("Skip the current commit")
                            .clicked()
                    {
                        tab.session.run_op("Skip", move |r, _, _| r.skip_op(state));
                    }
                    if can_continue {
                        let b = ui
                            .add_enabled(
                                !busy && conflicts == 0,
                                egui::Button::new(RichText::new("Continue").strong()),
                            )
                            .on_disabled_hover_text("Resolve all conflicts first");
                        if b.clicked() {
                            tab.session
                                .run_op("Continue", move |r, _, _| r.continue_op(state));
                        }
                    }
                });
            });
        });
}

fn modals(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &Shared) {
    let p = shared.palette;
    if let Some(dlg) = &mut tab.status.ignore_custom {
        match dlg.show(ui.ctx(), tab.session.repo(), p.muted, p.removed_fg) {
            ignore::Outcome::Open => {}
            ignore::Outcome::Cancel => tab.status.ignore_custom = None,
            ignore::Outcome::Ignore(pattern) => {
                tab.status.ignore_custom = None;
                let label = format!("Ignore {pattern}");
                tab.session
                    .run_index_op(label, move |r| r.ignore(&[pattern]));
            }
        }
    }
    if tab.status.confirm_abort {
        let state = snap.refs.state;
        let mut decision: Option<bool> = None;
        let resp = egui::Modal::new(Id::new(("fs-abort", &tab.path))).show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            ui.heading("Abort operation");
            ui.label(format!(
                "Abort the {}? Conflict resolutions made so far are lost.",
                state.label().to_lowercase()
            ));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Abort").color(p.removed_fg))
                    .clicked()
                {
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
                tab.session.run_op("Abort", move |r, _, _| {
                    r.abort_op(state).map(|_| String::new())
                });
            }
        }
    }

    if let Some(path) = tab.status.ask_resolved.first().cloned() {
        let mut decision: Option<bool> = None;
        let resp =
            egui::Modal::new(Id::new(("fs-mark-resolved", &tab.path))).show(ui.ctx(), |ui| {
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
                tab.session
                    .run_index_op("Mark resolved", move |r| r.mark_resolved(&paths));
            }
        }
    }

    if let Some(marked) = tab.status.guard.asking() {
        let question = conflict_guard::question(marked);
        let can_open = !marked.is_empty();
        let mut answer: Option<Answer> = None;
        let resp =
            egui::Modal::new(Id::new(("fs-conflict-guard", &tab.path))).show(ui.ctx(), |ui| {
                ui.set_width(460.0);
                ui.heading("Conflict markers");
                ui.label(question);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if can_open && ui.button("Open merge editor").clicked() {
                        answer = Some(Answer::OpenMergeEditor);
                    }
                    if ui
                        .button(RichText::new("Stage anyway").color(p.conflict))
                        .clicked()
                    {
                        answer = Some(Answer::StageAnyway);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        if resp.should_close() && answer.is_none() {
            answer = Some(Answer::Cancel);
        }
        if let Some(a) = answer {
            let (op, open) = tab.status.guard.answer(a);
            if let Some(op) = op {
                run_stage(tab, snap, op);
            }
            if let Some(path) = open {
                crate::views::merge::open_merge(tab, path);
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
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success());
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
        let new = old
            .replace("line 3\t", "LINE three\t")
            .replace("line 50\tx\n", "")
            + "tail";
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
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 900.0),
                    )),
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

        let row = tab
            .status
            .unstaged
            .row_of("f.txt", &tab.status.paths.unstaged)
            .unwrap();
        tab.status.unstaged.select_only("f.txt".into(), row);
        tab.status.active = Side::Unstaged;
        assert!(matches!(
            tab.status.focused(&snap),
            Some((DiffTarget::Unstaged { .. }, DiffMode::Unstaged))
        ));
        frames(&mut tab, &mut settings, 40);
        let (lines, rows, _) = tab.status.diff.shown_stats().expect("diff loaded");
        assert!(
            lines >= 4 && rows > lines,
            "unified rows include hunk headers"
        );

        // Tree mode, side by side, whole status.
        tab.state.file_view_tree = true;
        settings.diff.side_by_side = true;
        tab.status.diff = DiffView::default();
        frames(&mut tab, &mut settings, 40);
        let (lines, rows, bin) = tab
            .status
            .diff
            .shown_stats()
            .expect("side-by-side diff loaded");
        assert!(lines > 0 && rows > 0 && !bin);
        assert!(
            tab.status
                .unstaged
                .model
                .rows
                .iter()
                .any(|r| matches!(r, list::Row::Dir { .. }))
                || tab.status.staged.model.tree.is_some()
        );

        // Binary file and a staged file.
        let row = tab
            .status
            .unstaged
            .row_of("bin.dat", &tab.status.paths.unstaged)
            .unwrap();
        tab.status.unstaged.select_only("bin.dat".into(), row);
        frames(&mut tab, &mut settings, 20);
        assert!(
            tab.status.diff.shown_stats().is_some_and(|s| s.2),
            "binary diff"
        );
        let row = tab
            .status
            .staged
            .row_of("src/deep/a.rs", &tab.status.paths.staged)
            .unwrap();
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
