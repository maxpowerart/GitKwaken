//! Commit graph and history list (agent: history).
//!
//! The table is virtualized: only visible rows are laid out, row metadata is
//! requested for the visible range (plus a small prefetch) and loads in the
//! background, so a million-commit history scrolls as smoothly as a small one.

pub mod graph;
pub mod selection;
pub mod table;
pub mod task;
#[cfg(test)]
mod ui_tests;

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use egui::{Id, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use gk_core::{Label, ObjectId, Snapshot};
use selection::Selection;
use std::sync::Arc;
use table::{CellStyle, Columns};
use task::Oneshot;

/// Rows of metadata requested beyond the visible range on each side.
const PREFETCH: usize = 40;

pub struct HistoryState {
    pub sel: Selection<ObjectId>,
    /// Pending "scroll this commit into view".
    pub scroll_to: Option<ObjectId>,
    /// `scroll_to` is a jump (placed near the top) rather than a keyboard step.
    scroll_jump: bool,
    pub cols: Columns,
    /// Commits the open context menu applies to, in display order.
    menu_ids: Vec<ObjectId>,
    /// Visible rows, for PageUp/PageDown.
    page_rows: usize,
    /// Graph column width used by the header (last frame's rows).
    graph_w: f32,
    quick_filter: String,
    /// Identity of the last seen history, to prune the selection on refresh.
    history_ptr: usize,
    auto_selected: bool,
    /// Text (e.g. a full commit message) to put on the clipboard once loaded.
    pub(crate) clipboard: Oneshot<String>,
}

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            sel: Selection::default(),
            scroll_to: None,
            scroll_jump: false,
            cols: Columns::default(),
            menu_ids: Vec::new(),
            page_rows: 20,
            graph_w: 40.0,
            quick_filter: String::new(),
            history_ptr: 0,
            auto_selected: false,
            clipboard: Oneshot::default(),
        }
    }
}

impl HistoryState {
    /// Selects only `id` and scrolls it into view, about a quarter down the
    /// list unless it is already in the upper half.
    pub fn select_commit(&mut self, id: ObjectId) {
        self.sel.select_only(id);
        self.scroll_to = Some(id);
        self.scroll_jump = true;
    }
}

/// Dev/benchmark aid: `GITKWAKEN_AUTOSCROLL=<rows per frame>` scrolls the
/// commit table down by that many rows every frame (and keeps repainting),
/// e.g. to measure scrolling frame times with `GK_DX11_FRAME_STATS=1`.
fn autoscroll_rows() -> Option<f32> {
    static ROWS: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *ROWS.get_or_init(|| {
        std::env::var("GITKWAKEN_AUTOSCROLL")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|r| *r != 0.0 && r.is_finite())
    })
}

pub fn head_oid(snap: &Snapshot) -> Option<ObjectId> {
    snap.refs
        .head_id
        .as_deref()
        .and_then(|h| ObjectId::from_hex(h.as_bytes()).ok())
}

/// Splits the remaining space of `ui` in two with a draggable handle.
/// `vertical`: top/bottom, else left/right. `frac` is the first part's share.
pub fn split(ui: &mut Ui, id: Id, frac: &mut f32, vertical: bool, min: f32) -> (Rect, Rect) {
    let rect = ui.available_rect_before_wrap();
    ui.advance_cursor_after_rect(rect);
    let handle = 5.0;
    let total = if vertical {
        rect.height()
    } else {
        rect.width()
    } - handle;
    let lo = min.min(total / 2.0).max(0.0);
    let hi = (total - lo).max(lo);
    let mut first = (total * frac.clamp(0.0, 1.0)).clamp(lo, hi);
    let start = if vertical { rect.top() } else { rect.left() };
    let handle_rect = |first: f32| {
        if vertical {
            Rect::from_min_size(pos2(rect.left(), start + first), vec2(rect.width(), handle))
        } else {
            Rect::from_min_size(pos2(start + first, rect.top()), vec2(handle, rect.height()))
        }
    };
    let resp = ui.interact(handle_rect(first), id, Sense::drag());
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let pos = if vertical { p.y } else { p.x };
        first = (pos - start - handle / 2.0).clamp(lo, hi);
        if total > 0.0 {
            *frac = first / total;
        }
    }
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.ctx().set_cursor_icon(if vertical {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    }
    let h = handle_rect(first);
    let stroke = if active {
        ui.visuals().widgets.active.bg_stroke
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    if vertical {
        ui.painter().hline(h.x_range(), h.center().y, stroke);
    } else {
        ui.painter().vline(h.center().x, h.y_range(), stroke);
    }
    if vertical {
        (
            Rect::from_min_max(rect.min, pos2(rect.right(), h.top())),
            Rect::from_min_max(pos2(rect.left(), h.bottom()), rect.max),
        )
    } else {
        (
            Rect::from_min_max(rect.min, pos2(h.left(), rect.bottom())),
            Rect::from_min_max(pos2(h.right(), rect.top()), rect.max),
        )
    }
}

/// Runs `f` in a child ui confined to `rect`.
pub fn in_rect<R>(ui: &mut Ui, rect: Rect, salt: &str, f: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().max_rect(rect).id_salt(salt), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        f(ui)
    })
    .inner
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    poll_clipboard(ui, tab);
    // Sidebar clicks on branches/tags jump to their commit.
    if let Some(hex) = tab.sidebar.jump_to.take()
        && let Ok(id) = ObjectId::from_hex(hex.trim().as_bytes())
    {
        tab.history.select_commit(id);
        tab.history.auto_selected = true;
    }
    let hist = snap.history.clone();
    let ptr = Arc::as_ptr(&hist) as usize;
    if ptr != tab.history.history_ptr {
        tab.history.history_ptr = ptr;
        if !hist.is_empty() {
            tab.history.sel.retain(|id| hist.row(id));
        }
    }
    if !tab.history.auto_selected
        && tab.history.sel.is_empty()
        && !hist.is_empty()
        && let Some(h) = head_oid(snap).filter(|h| hist.row(h).is_some())
    {
        tab.history.select_commit(h);
        tab.history.auto_selected = true;
    }

    toolbar(ui, tab, snap);
    ui.add_space(2.0);

    if hist.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            if snap.history_loading {
                ui.spinner();
                ui.label("Loading history…");
            } else if snap.status.is_clean() {
                ui.label("No commits yet.");
            } else {
                ui.label("No commits yet.");
                if ui.link("Show uncommitted changes").clicked() {
                    tab.view = View::FileStatus;
                }
            }
        });
        return;
    }

    let mut frac = tab.state.layout.list_split;
    let (top, bottom) = split(
        ui,
        Id::new(("history-split", &tab.path)),
        &mut frac,
        true,
        80.0,
    );
    tab.state.layout.list_split = frac;
    in_rect(ui, top, "history-table", |ui| {
        commit_table(ui, tab, snap, shared)
    });
    in_rect(ui, bottom, "history-details", |ui| {
        crate::views::details::show(ui, tab, snap, shared)
    });
}

fn set_scope(tab: &mut RepoTab, current: bool) {
    let mut o = tab.session.options();
    o.scope = if current {
        gk_core::history::Scope::Current
    } else {
        gk_core::history::Scope::All
    };
    tab.session.set_options(o);
    tab.state.history_scope = if current { "current" } else { "all" }.into();
}

fn toolbar(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot) {
    ui.horizontal(|ui| {
        let current = tab.state.history_scope == "current";
        if ui.selectable_label(!current, "All branches").clicked() && current {
            set_scope(tab, false);
        }
        if ui.selectable_label(current, "Current branch").clicked() && !current {
            set_scope(tab, true);
        }
        ui.separator();
        let head = head_oid(snap);
        if ui
            .add_enabled(head.is_some(), egui::Button::new("Jump to HEAD"))
            .clicked()
            && let Some(h) = head
        {
            tab.history.select_commit(h);
            ui.memory_mut(|m| m.request_focus(list_id(tab)));
        }
        if snap.history_loading {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut tab.history.quick_filter)
                    .hint_text("Filter commits…")
                    .desired_width(200.0),
            );
            if r.changed() && !tab.history.quick_filter.trim().is_empty() {
                let q = std::mem::take(&mut tab.history.quick_filter);
                tab.search.start_query(q);
                tab.view = View::Search;
            }
        });
    });
}

fn list_id(tab: &RepoTab) -> Id {
    Id::new(("history-list", &tab.path))
}

/// Puts loaded clipboard text (e.g. a full message) on the clipboard.
pub(crate) fn poll_clipboard(ui: &Ui, tab: &RepoTab) {
    if let Some(t) = tab.history.clipboard.take() {
        ui.ctx().copy_text(t);
    }
}

pub(crate) fn cell_style<'a>(ui: &Ui, shared: &'a Shared) -> CellStyle<'a> {
    let style = ui.style();
    CellStyle {
        font: egui::TextStyle::Body.resolve(style),
        small: egui::TextStyle::Small.resolve(style),
        mono: egui::TextStyle::Monospace.resolve(style),
        text: ui.visuals().text_color(),
        muted: shared.palette.muted,
        palette: &shared.palette,
        settings: &shared.settings.history,
    }
}

fn commit_table(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let hist = snap.history.clone();
    let hs = &shared.settings.history;
    let row_h = hs.row_height.clamp(14.0, 60.0);
    let lane_w = hs.graph_lane_width.clamp(6.0, 40.0);
    let dirty = !snap.status.is_clean();
    let changes = snap.status.staged.len() + snap.status.unstaged.len();
    let off = dirty as usize;
    let n = hist.len() + off;
    let head = head_oid(snap);
    let head_row = head.and_then(|h| hist.row(&h));
    let id = list_id(tab);
    let row_of = |k: &ObjectId| hist.row(k).map(|r| r + off);
    let key_at = |r: usize| r.checked_sub(off).and_then(|r| hist.ids.get(r).copied());

    // Keyboard navigation while the list has focus.
    if ui.memory(|m| m.has_focus(id)) {
        let st = &mut tab.history;
        for (key, shift) in table::nav_keys(ui) {
            let cur = st.sel.cursor_row(row_of);
            if let Some(t) = selection::nav_target(key, cur, n, st.page_rows.saturating_sub(1)) {
                let t = t.max(off);
                st.sel.move_to(t, shift, key_at, row_of);
                st.scroll_to = key_at(t);
                st.scroll_jump = false;
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C))
            && !st.sel.is_empty()
        {
            let ids = st.sel.sorted(row_of);
            ui.ctx().copy_text(
                ids.iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    let jump = std::mem::take(&mut tab.history.scroll_jump);
    let mut scroll = tab
        .history
        .scroll_to
        .take()
        .and_then(|k| row_of(&k))
        .map(|r| {
            if jump {
                table::ScrollTo::Jump(r)
            } else {
                table::ScrollTo::Reveal(r)
            }
        });
    if let Some(rows) = autoscroll_rows() {
        scroll = scroll.or(Some(table::ScrollTo::By(rows * row_h)));
        ui.ctx().request_repaint();
    }

    let left = ui.max_rect().left();
    let right = left + ui.available_width();
    let rects = table::layout_columns(left, right, tab.history.graph_w, &tab.history.cols);
    table::header(ui, id.with("header"), &mut tab.history.cols, &rects, true);

    let st = cell_style(ui, shared);
    let cols = tab.history.cols;
    let session = &tab.session;
    let sel = &tab.history.sel;
    let focused = ui.memory(|m| m.has_focus(id));
    let visuals = ui.visuals().clone();
    let bg = visuals.panel_fill;
    let mut new_gw = tab.history.graph_w;
    let graph_cap = cols.graph_cap(lane_w);
    let out = table::virtual_list(ui, id, n, row_h, scroll, |f, range| {
        let crange = range.start.saturating_sub(off)..range.end.saturating_sub(off);
        let pre_a = crange.start.saturating_sub(PREFETCH);
        let pre_b = (crange.end + PREFETCH).min(hist.len());
        let metas = if pre_a < pre_b {
            session.metas(&hist.ids[pre_a..pre_b])
        } else {
            Vec::new()
        };
        let head_lane = head_row
            .and_then(|r| hist.layout.rows.get(r))
            .map(|r| r.lane)
            .unwrap_or(0);
        let extra = (dirty && range.start == 0).then_some(head_lane);
        new_gw = graph::column_width(&hist.layout, crange.start, crange.end, lane_w, extra)
            .min(graph_cap);
        let rects = table::layout_columns(f.x.min, f.x.max, new_gw, &cols);
        let graph_clip =
            Rect::from_x_y_ranges(rects.graph, f.clip_y).intersect(f.painter.clip_rect());
        let gp = f.painter.with_clip_rect(graph_clip);
        for r in range.clone() {
            let rect = f.row_rect(r);
            if r < off {
                table::paint_row_bg(f, r, rect, false, focused, st.palette, &visuals);
                let text = format!("Uncommitted changes ({changes})");
                let desc = Rect::from_x_y_ranges(rects.desc, rect.y_range());
                table::cell_text(f.painter, desc, text, &st.font, st.muted);
                continue;
            }
            let cr = r - off;
            let cid = hist.ids[cr];
            let selected = sel.contains(&cid);
            let row_fill = table::row_bg(f, r, selected, focused, st.palette, &visuals);
            if let Some(c) = row_fill {
                f.painter.rect_filled(rect, 0.0, c);
            }
            let is_head = head == Some(cid);
            graph::paint_row(
                &gp,
                &graph::RowGraph {
                    layout: &hist.layout,
                    row: cr,
                    rect,
                    left: rects.graph.min,
                    lane_w,
                    is_head,
                    bg,
                    head_color: st.palette.head,
                },
            );
            graph::fade_overflow(
                &gp,
                &hist.layout,
                cr,
                Rect::from_x_y_ranges(rects.graph, rect.y_range()),
                lane_w,
                bg,
                row_fill,
            );
            let meta = cr
                .checked_sub(pre_a)
                .and_then(|i| metas.get(i))
                .and_then(|m| m.as_deref());
            let labels = table::labels_of(&snap.refs.labels, &cid);
            table::paint_commit_cells(f.painter, rect, &rects, &st, &cid, meta, labels, is_head);
        }
        if dirty {
            // Worktree node on HEAD's lane, dashed into the HEAD commit.
            let x = graph::lane_x(rects.graph.min, head_lane, lane_w);
            let c = pos2(x, f.row_rect(0).center().y);
            let radius = (lane_w * 0.3).clamp(3.0, row_h * 0.3);
            let head_c = head_row.map(|hr| pos2(x, f.row_rect(hr + off).center().y - radius));
            graph::paint_worktree_node(
                &gp,
                c,
                head_c,
                (f.clip_y.min, f.clip_y.max),
                radius,
                st.muted,
                bg,
            );
        }
    });
    if (new_gw - tab.history.graph_w).abs() > 0.5 {
        tab.history.graph_w = new_gw;
        ui.ctx().request_repaint();
    }
    tab.history.page_rows = out.visible.len().max(1);

    if let Some((r, m)) = out.clicked {
        if r < off {
            tab.view = View::FileStatus;
        } else {
            tab.history.sel.click(r, m.command, m.shift, key_at, row_of);
        }
    }
    if let Some(r) = out.double_clicked.filter(|r| *r >= off)
        && let Some(cid) = key_at(r)
    {
        tab.dialog = Some(Dialog::checkout(checkout_rev(snap, &cid)));
    }
    if let Some(r) = out.secondary {
        if let Some(cid) = key_at(r) {
            if !tab.history.sel.contains(&cid) {
                tab.history.sel.select_only(cid);
            }
            tab.history.menu_ids = tab.history.sel.sorted(row_of);
        } else {
            tab.history.menu_ids.clear();
        }
    }
    if !tab.history.menu_ids.is_empty() {
        let ids = std::mem::take(&mut tab.history.menu_ids);
        out.response
            .context_menu(|ui| commit_menu(ui, tab, snap, &ids));
        tab.history.menu_ids = ids;
    }
}

/// What to check out for a commit: its first non-current local branch, else the id.
pub fn checkout_rev(snap: &Snapshot, id: &ObjectId) -> String {
    table::labels_of(&snap.refs.labels, id)
        .and_then(|ls| {
            ls.iter().find_map(|l| match l {
                Label::Branch {
                    name,
                    is_head: false,
                } => Some(name.clone()),
                _ => None,
            })
        })
        .unwrap_or_else(|| id.to_string())
}

/// Context menu for commits. `ids` are in display order (newest first).
pub fn commit_menu(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, ids: &[ObjectId]) {
    let Some(first) = ids.first().copied() else {
        return;
    };
    let single = ids.len() == 1;
    let hex = first.to_string();
    let newest_first: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    let oldest_first: Vec<String> = newest_first.iter().rev().cloned().collect();
    let mut dialog: Option<Dialog> = None;
    fn item(ui: &mut Ui, enabled: bool, text: &str) -> bool {
        let clicked = ui.add_enabled(enabled, egui::Button::new(text)).clicked();
        if clicked {
            ui.close();
        }
        clicked
    }
    if single {
        if let Some(ls) = table::labels_of(&snap.refs.labels, &first) {
            for l in ls {
                if let Label::Branch {
                    name,
                    is_head: false,
                } = l
                    && item(ui, true, &format!("Checkout {name}"))
                {
                    dialog = Some(Dialog::checkout(name.clone()));
                }
            }
        }
        if item(ui, true, "Checkout…") {
            dialog = Some(Dialog::checkout(hex.clone()));
        }
        ui.separator();
        if item(ui, true, "Create branch here…") {
            dialog = Some(Dialog::branch(snap, Some(hex.clone())));
        }
        if item(ui, true, "Create tag here…") {
            dialog = Some(Dialog::tag(Some(hex.clone())));
        }
        ui.separator();
        if item(ui, true, "Merge into current…") {
            dialog = Some(Dialog::merge(snap, Some(hex.clone())));
        }
        if item(ui, true, "Rebase current onto…") {
            dialog = Some(Dialog::rebase(snap, Some(hex.clone())));
        }
        if item(ui, true, "Interactive rebase children of this commit…") {
            dialog = Some(Dialog::interactive_rebase(snap, hex.clone()));
        }
        ui.separator();
    }
    let n = ids.len();
    let suffix = if single {
        String::new()
    } else {
        format!(" {n} commits")
    };
    if item(ui, true, &format!("Cherry-pick{suffix}…")) {
        dialog = Some(Dialog::cherry_pick(oldest_first.clone()));
    }
    if item(ui, true, &format!("Revert{suffix}…")) {
        dialog = Some(Dialog::revert(newest_first.clone()));
    }
    if single && item(ui, true, "Reset current branch to here…") {
        dialog = Some(Dialog::reset(hex.clone()));
    }
    ui.separator();
    if item(ui, true, &format!("Create patch{suffix}…")) {
        dialog = Some(Dialog::create_patch(oldest_first.clone()));
    }
    if single && item(ui, true, "Archive…") {
        dialog = Some(Dialog::archive(hex.clone()));
    }
    ui.separator();
    if item(ui, true, if single { "Copy SHA" } else { "Copy SHAs" }) {
        ui.ctx().copy_text(newest_first.join("\n"));
    }
    if item(
        ui,
        true,
        if single {
            "Copy short SHA"
        } else {
            "Copy short SHAs"
        },
    ) {
        ui.ctx().copy_text(
            newest_first
                .iter()
                .map(|s| crate::util::short_id(s))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if item(
        ui,
        true,
        if single {
            "Copy message"
        } else {
            "Copy messages"
        },
    ) {
        let git = tab.session.repo().git().clone();
        let session = tab.session.clone();
        let revs = newest_first.clone();
        tab.history.clipboard.spawn(ui.ctx(), move || {
            let mut args = vec!["show", "-s", "--format=%B"];
            args.extend(revs.iter().map(|s| s.as_str()));
            match git.read(&args) {
                Ok(s) => Some(s),
                Err(e) => {
                    session.push_message(
                        gk_core::MessageKind::Error,
                        format!("Copy message failed: {e:#}"),
                    );
                    None
                }
            }
        });
    }
    if let Some(d) = dialog {
        tab.dialog = Some(d);
    }
}
