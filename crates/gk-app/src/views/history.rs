//! Commit graph and history list (agent: history).
//!
//! The table is virtualized: only visible rows are laid out, row metadata is
//! requested for the visible range (plus a small prefetch) and loads in the
//! background, so a million-commit history scrolls as smoothly as a small one.

pub mod filter;
pub mod graph;
pub mod keys;
pub mod selection;
pub mod table;
pub mod task;
#[cfg(test)]
mod ui_tests;

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use egui::{Id, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use gk_core::history::Scope;
use gk_core::{Label, ObjectId, Snapshot};
use keys::ListKeys;
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
    /// Message / author / date / path / pickaxe filters (flat list).
    pub filter: filter::FilterState,
    /// Identity of the last seen history, to prune the selection on refresh.
    history_ptr: usize,
    auto_selected: bool,
    /// Text (e.g. a full commit message) to put on the clipboard once loaded.
    pub(crate) clipboard: Oneshot<String>,
    /// "Jump to:" field (SHA prefix, branch, tag or HEAD).
    jump: String,
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
            filter: Default::default(),
            history_ptr: 0,
            auto_selected: false,
            clipboard: Oneshot::default(),
            jump: String::new(),
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
    {
        let revs = scope_revs(&tab.session.options().scope, snap);
        let git = tab.session.repo().git().clone();
        tab.history.filter.sync(ui.ctx(), &git, revs, ptr);
    }
    ui.add_space(2.0);

    if tab.history.filter.active() {
        let mut frac = tab.state.layout.list_split;
        let (top, bottom) = split(
            ui,
            Id::new(("history-split", &tab.path)),
            &mut frac,
            true,
            80.0,
        );
        tab.state.layout.list_split = frac;
        in_rect(ui, top, "history-filtered", |ui| {
            filtered_table(ui, tab, snap, shared)
        });
        in_rect(ui, bottom, "history-details", |ui| {
            crate::views::details::show(ui, tab, snap, shared)
        });
        return;
    }

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

/// History scope from `RepoState::history_scope`: `all`, `current` or
/// `revs:<name> <name>…` (ref names cannot contain spaces).
pub fn scope_from_state(s: &str) -> Scope {
    match s.strip_prefix("revs:") {
        _ if s == "current" => Scope::Current,
        _ if s == "local" => Scope::Local,
        Some(list) => {
            let v: Vec<String> = list.split_whitespace().map(str::to_string).collect();
            if v.is_empty() {
                Scope::All
            } else {
                Scope::Revs(v)
            }
        }
        None => Scope::All,
    }
}

/// Inverse of [`scope_from_state`].
pub fn scope_to_state(s: &Scope) -> String {
    match s {
        Scope::All => "all".into(),
        Scope::Local => "local".into(),
        Scope::Current => "current".into(),
        Scope::Revs(v) => format!("revs:{}", v.join(" ")),
    }
}

/// `git log` revision arguments for the commits a scope shows.
pub fn scope_revs(scope: &Scope, snap: &Snapshot) -> Vec<String> {
    match scope {
        Scope::All | Scope::Local => {
            let mut v: Vec<String> = if *scope == Scope::All {
                vec!["--branches".into(), "--remotes".into(), "--tags".into()]
            } else {
                vec!["--branches".into(), "--tags".into()]
            };
            if snap.refs.head_id.is_some() {
                v.push("HEAD".into());
            }
            v
        }
        Scope::Current => vec!["HEAD".into()],
        Scope::Revs(r) => r.clone(),
    }
}

fn set_scope(tab: &mut RepoTab, scope: Scope) {
    let mut o = tab.session.options();
    tab.state.history_scope = scope_to_state(&scope);
    o.scope = scope;
    tab.session.set_options(o);
}

/// Branches picked in the history filter ("Show branches:" of the branch picker); empty while
/// the history shows all branches or the current one.
pub fn filtered_branches(scope: &Scope) -> &[String] {
    match scope {
        Scope::Revs(v) => v,
        _ => &[],
    }
}

/// Adds `name` (a local branch, or `remote/branch`) to the history filter or removes it, like
/// its checkbox in the branch picker; an empty filter shows all branches again.
pub fn toggle_branch_filter(tab: &mut RepoTab, name: &str) {
    let mut v = filtered_branches(&tab.session.options().scope).to_vec();
    if v.iter().any(|p| p == name) {
        v.retain(|p| p != name);
    } else {
        v.push(name.to_string());
    }
    set_scope(
        tab,
        if v.is_empty() {
            Scope::All
        } else {
            Scope::Revs(v)
        },
    );
}

/// Text of the branch picker button.
pub fn scope_label(s: &Scope) -> String {
    match s {
        Scope::All | Scope::Local => "All branches".into(),
        Scope::Current => "Current branch".into(),
        Scope::Revs(v) if v.len() == 1 => v[0].clone(),
        Scope::Revs(v) => format!("{} branches", v.len()),
    }
}

/// Branch picker: all, current, or any set of branches (`Scope::Revs`).
fn branch_picker(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot) {
    let scope = tab.session.options().scope;
    let mut new: Option<Scope> = None;
    ui.menu_button(format!("{} ▾", scope_label(&scope)), |ui| {
        let all = matches!(scope, Scope::All | Scope::Local);
        if ui.selectable_label(all, "All branches").clicked() {
            // Keeps the "Show remote branches" choice.
            new = Some(if scope == Scope::Local {
                Scope::Local
            } else {
                Scope::All
            });
            ui.close();
        }
        if ui
            .selectable_label(scope == Scope::Current, "Current branch")
            .clicked()
        {
            new = Some(Scope::Current);
            ui.close();
        }
        ui.separator();
        ui.label(RichText::new("Show branches:").weak());
        let picked: Vec<String> = match &scope {
            Scope::Revs(v) => v.clone(),
            _ => Vec::new(),
        };
        let mut toggle = |ui: &mut Ui, name: &str| {
            let mut on = picked.iter().any(|p| p == name);
            if ui.checkbox(&mut on, name).changed() {
                let mut v = picked.clone();
                if on {
                    v.push(name.to_string());
                } else {
                    v.retain(|p| p != name);
                }
                new = Some(if v.is_empty() {
                    Scope::All
                } else {
                    Scope::Revs(v)
                });
            }
        };
        egui::ScrollArea::vertical()
            .id_salt("branch-picker")
            .max_height(320.0)
            .show(ui, |ui| {
                for b in &snap.refs.branches {
                    toggle(ui, &b.name);
                }
                if !snap.refs.remote_branches.is_empty() {
                    ui.menu_button("Remote branches", |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("branch-picker-remote")
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for b in &snap.refs.remote_branches {
                                    toggle(ui, &b.full());
                                }
                            });
                    });
                }
            });
    })
    .response
    .on_hover_text("Branches shown in the history");
    if let Some(s) = new
        && s != scope
    {
        set_scope(tab, s);
    }
}

/// "Jump to:" target: empty or `HEAD`, a branch / remote branch / tag name,
/// or a commit id prefix (4+ hex digits) of a loaded commit.
pub fn resolve_jump(snap: &Snapshot, text: &str) -> Option<ObjectId> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("HEAD") {
        return head_oid(snap);
    }
    let r = &snap.refs;
    let target = r
        .branches
        .iter()
        .find(|b| b.name == t)
        .map(|b| b.target.as_str())
        .or_else(|| {
            r.tags
                .iter()
                .find(|x| x.name == t)
                .map(|x| x.target.as_str())
        })
        .or_else(|| {
            r.remote_branches
                .iter()
                .find(|b| b.full() == t)
                .map(|b| b.target.as_str())
        });
    if let Some(hex) = target {
        return ObjectId::from_hex(hex.as_bytes()).ok();
    }
    let lower = t.to_ascii_lowercase();
    if lower.len() < 4 || !lower.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    snap.history
        .ids
        .iter()
        .find(|id| id.to_hex().to_string().starts_with(&lower))
        .copied()
}

/// SourceTree's history header: "All branches ▾", "Show remote branches",
/// Undo on the left; message filter, Filters and "Jump to:" on the right.
fn toolbar(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot) {
    ui.horizontal(|ui| {
        branch_picker(ui, tab, snap);
        let scope = tab.session.options().scope;
        let mut remotes = scope != Scope::Local;
        let toggles = matches!(scope, Scope::All | Scope::Local);
        if ui
            .add_enabled(
                toggles,
                egui::Checkbox::new(&mut remotes, "Show remote branches"),
            )
            .on_hover_text("Include remote branches in All branches")
            .changed()
        {
            set_scope(tab, if remotes { Scope::All } else { Scope::Local });
        }
        ui.separator();
        let head = head_oid(snap);
        if ui
            .add_enabled(head.is_some(), egui::Button::new("↶ Undo…"))
            .on_hover_text("Undo the last operation (commit, merge, rebase, reset, pull, checkout)")
            .clicked()
        {
            tab.dialog = Some(Dialog::undo());
        }
        if snap.history_loading {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Right to left: HEAD button, "Jump to:" field and label.
            let mut jump: Option<ObjectId> = None;
            if ui
                .add_enabled(head.is_some(), egui::Button::new("HEAD"))
                .on_hover_text("Jump to HEAD")
                .clicked()
            {
                jump = head;
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut tab.history.jump)
                    .hint_text("SHA, branch or tag")
                    .desired_width(140.0),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                jump = resolve_jump(snap, &tab.history.jump);
                if jump.is_none() {
                    tab.session.push_message(
                        gk_core::MessageKind::Info,
                        format!("No loaded commit matches \"{}\"", tab.history.jump.trim()),
                    );
                }
            }
            ui.label("Jump to:");
            if let Some(h) = jump {
                tab.history.select_commit(h);
                ui.memory_mut(|m| m.request_focus(list_id(tab)));
            }
            ui.separator();
            let f = &mut tab.history.filter;
            let n = f.edit.extra_count();
            let label = if n > 0 {
                format!("Filters ({n})")
            } else {
                "Filters".to_string()
            };
            if ui
                .selectable_label(f.open, label)
                .on_hover_text("Filter by author, date, path or content (-S / -G)")
                .clicked()
            {
                f.open = !f.open;
            }
            if f.edit.is_active()
                && ui
                    .small_button("✖")
                    .on_hover_text("Clear filters")
                    .clicked()
            {
                f.clear();
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut f.edit.text)
                    .hint_text("Filter by message…")
                    .desired_width(200.0),
            );
            if r.changed() {
                f.edited();
            }
        });
    });
    if tab.history.filter.open {
        filter_row(ui, &mut tab.history.filter);
    }
}

/// Second toolbar row: author, dates, path and pickaxe.
fn filter_row(ui: &mut Ui, f: &mut filter::FilterState) {
    ui.horizontal_wrapped(|ui| {
        let mut changed = false;
        let mut field = |ui: &mut Ui, label: &str, v: &mut String, hint: &str, w: f32| {
            ui.label(label);
            changed |= ui
                .add(
                    egui::TextEdit::singleline(v)
                        .hint_text(hint)
                        .desired_width(w),
                )
                .changed();
        };
        field(ui, "Author:", &mut f.edit.author, "name or email", 130.0);
        field(ui, "Since:", &mut f.edit.since, "2026-01-01", 90.0);
        field(ui, "Until:", &mut f.edit.until, "2026-12-31", 90.0);
        field(ui, "Path:", &mut f.edit.path, "file or folder", 150.0);
        let before = f.edit.pickaxe_mode;
        egui::ComboBox::from_id_salt("pickaxe-mode")
            .width(50.0)
            .selected_text(match f.edit.pickaxe_mode {
                filter::Pickaxe::String => "-S",
                filter::Pickaxe::Regex => "-G",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut f.edit.pickaxe_mode,
                    filter::Pickaxe::String,
                    "-S  occurrences of a string changed",
                );
                ui.selectable_value(
                    &mut f.edit.pickaxe_mode,
                    filter::Pickaxe::Regex,
                    "-G  diff lines match a regex",
                );
            });
        changed |= before != f.edit.pickaxe_mode && !f.edit.pickaxe.is_empty();
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut f.edit.pickaxe)
                    .hint_text("content (pickaxe)")
                    .desired_width(130.0),
            )
            .changed();
        if changed {
            f.edited();
        }
        if f.running() {
            ui.spinner();
        }
        if f.active() {
            let n = f.rows.len();
            ui.label(RichText::new(format!("{n} match{}", if n == 1 { "" } else { "es" })).weak());
        }
    });
}

/// The filtered history: matching commits as a flat list (no graph).
fn filtered_table(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let rows = tab.history.filter.rows.clone();
    let n = rows.len();
    if let Some(e) = tab.history.filter.error() {
        ui.colored_label(shared.palette.removed_fg, e);
    }
    if n == 0 {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            if tab.history.filter.running() {
                ui.spinner();
                ui.label("Filtering…");
            } else {
                ui.label(RichText::new("No commits match the filters.").weak());
            }
        });
        return;
    }
    let row_h = crate::theme::Density::of(shared.settings.compact)
        .history_row(shared.settings.history.row_height);
    let id = Id::new(("history-filtered-list", &tab.path));
    let fs = &tab.history.filter;
    let key_at = |r: usize| rows.get(r).copied();
    let row_of = |k: &ObjectId| fs.row_of(k);
    let focused = ui.memory(|m| m.has_focus(id));
    let mut scroll = None;
    if focused {
        let st = &mut tab.history.sel;
        for (key, shift) in table::nav_keys(ui) {
            let cur = st.cursor_row(row_of);
            if let Some(t) =
                selection::nav_target(key, cur, n, tab.history.page_rows.saturating_sub(1))
            {
                st.move_to(t, shift, key_at, row_of);
                scroll = Some(table::ScrollTo::Reveal(t));
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C)) && !st.is_empty()
        {
            let ids = st.sorted(row_of);
            ui.ctx().copy_text(
                ids.iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    if let Some(k) = tab.history.scroll_to.take()
        && let Some(r) = row_of(&k)
    {
        scroll = Some(table::ScrollTo::Jump(r));
    }
    let k = ListKeys::read(ui, focused);
    let left = ui.max_rect().left();
    let right = left + ui.available_width();
    let rects = table::layout_columns(left, right, 0.0, &tab.history.cols);
    table::header(ui, id.with("header"), &mut tab.history.cols, &rects, false);
    let st = cell_style(ui, shared);
    let cols = tab.history.cols;
    let session = &tab.session;
    let sel = &tab.history.sel;
    let visuals = ui.visuals().clone();
    let head = head_oid(snap);
    let out = table::virtual_list(ui, id, n, row_h, scroll, |f, range| {
        let pre_a = range.start.saturating_sub(PREFETCH);
        let pre_b = (range.end + PREFETCH).min(n);
        let metas = session.metas(&rows[pre_a..pre_b]);
        let rects = table::layout_columns(f.x.min, f.x.max, 0.0, &cols);
        for r in range {
            let cid = rows[r];
            let rect = f.row_rect(r);
            table::paint_row_bg(
                f,
                r,
                rect,
                sel.contains(&cid),
                focused,
                st.palette,
                &visuals,
            );
            let meta = metas.get(r - pre_a).and_then(|m| m.as_deref());
            let labels = table::labels_of(&snap.refs.labels, &cid);
            table::paint_commit_cells(
                f.painter,
                rect,
                &rects,
                &st,
                &cid,
                meta,
                labels,
                head == Some(cid),
            );
        }
    });
    tab.history.page_rows = out.visible.len().max(1);
    if let Some((r, m)) = out.clicked {
        tab.history.sel.click(r, m.command, m.shift, key_at, row_of);
    }
    if let Some(cid) = out.double_clicked.and_then(key_at) {
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
    let ids = tab.history.sel.sorted(row_of);
    let cursor = tab.history.sel.cursor_row(row_of);
    list_keys(tab, snap, k, &ids);
    if k.menu && !ids.is_empty() {
        tab.history.menu_ids = ids;
    }
    if !tab.history.menu_ids.is_empty() {
        let ids = std::mem::take(&mut tab.history.menu_ids);
        out.response
            .context_menu(|ui| commit_menu(ui, tab, snap, &ids));
        let pos = out.row_pos(cursor.unwrap_or(0));
        keys::keyboard_menu(&out.response, id.with("kb-menu"), k.menu, pos, |ui| {
            commit_menu(ui, tab, snap, &ids)
        });
        tab.history.menu_ids = ids;
    }
}

/// Enter / Ctrl+B / Ctrl+T on the selected commits of a commit list (newest
/// first). Shift+F10 is handled by the caller (it owns the menu).
pub(crate) fn list_keys(tab: &mut RepoTab, snap: &Snapshot, k: ListKeys, ids: &[ObjectId]) {
    let [id] = ids else { return };
    let hex = id.to_string();
    if k.enter {
        tab.dialog = Some(Dialog::checkout(checkout_rev(snap, id)));
    } else if k.branch {
        tab.dialog = Some(Dialog::branch(snap, Some(hex)));
    } else if k.tag {
        tab.dialog = Some(Dialog::tag(Some(hex)));
    }
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
    let row_h = crate::theme::Density::of(shared.settings.compact).history_row(hs.row_height);
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
    let keys_pressed = ListKeys::read(ui, ui.memory(|m| m.has_focus(id)));
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
    let ids = tab.history.sel.sorted(row_of);
    let cursor = tab.history.sel.cursor_row(row_of);
    list_keys(tab, snap, keys_pressed, &ids);
    if keys_pressed.menu && !ids.is_empty() {
        tab.history.menu_ids = ids;
    }
    if !tab.history.menu_ids.is_empty() {
        let ids = std::mem::take(&mut tab.history.menu_ids);
        out.response
            .context_menu(|ui| commit_menu(ui, tab, snap, &ids));
        let pos = out.row_pos(cursor.unwrap_or(off));
        keys::keyboard_menu(
            &out.response,
            id.with("kb-menu"),
            keys_pressed.menu,
            pos,
            |ui| commit_menu(ui, tab, snap, &ids),
        );
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
        keys::menu_item(ui, enabled, text, "")
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
        if keys::menu_item(ui, true, "Checkout…", keys::CHECKOUT_KEY) {
            dialog = Some(Dialog::checkout(hex.clone()));
        }
        ui.separator();
        if keys::menu_item(ui, true, "Create branch here…", keys::BRANCH_KEY) {
            dialog = Some(Dialog::branch(snap, Some(hex.clone())));
        }
        if keys::menu_item(ui, true, "Create tag here…", keys::TAG_KEY) {
            dialog = Some(Dialog::tag(Some(hex.clone())));
        }
        ui.separator();
        // A branch tip merges / rebases by name (nicer merge message, and
        // the dialog shows what the user clicked on).
        let rev = checkout_rev(snap, &first);
        if item(ui, true, "Merge into current…") {
            dialog = Some(Dialog::merge(snap, Some(rev.clone())));
        }
        if item(ui, true, "Rebase current onto…") {
            dialog = Some(Dialog::rebase(snap, Some(rev)));
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
    if single {
        ui.separator();
        if item(ui, true, "Add/Edit note…") {
            dialog = Some(Dialog::edit_note(hex.clone()));
        }
        if item(ui, true, "Remove note…") {
            dialog = Some(Dialog::remove_note(hex.clone()));
        }
    }
    if let Some(d) = dialog {
        tab.dialog = Some(d);
    }
}
