//! Reusable diff viewer: unified / side-by-side, word highlights, line
//! numbers, hunk actions and line selection for partial staging.
//! Used by the file status view (editable) and commit details (read-only).
//!
//! Rendering is virtualized: per frame only the visible rows are laid out.
//! The row layout and the removed/added pairing are computed once per
//! loaded diff ([`model::Layout`]); word highlights lazily per visible line.

mod external;
mod guard;
mod model;
mod preview;
mod syntax;

use crate::app::Shared;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, RichText, Sense, Ui, pos2, vec2};
use gk_core::content::DiffTarget;
use gk_core::{Load, Session};
use gk_diff::{FileDiff, Line, LineKind};
use model::{Layout, LineKey, NONE, Press, Row, Selection};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use syntax::{Highlights, HlCache, Span};

/// Below this width the diff header puts the path on its own row.
const NARROW_TOOLBAR: f32 = 560.0;
/// Below this width the header toggles use short labels.
const SHORT_LABELS: f32 = 470.0;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What actions the viewer offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// Commit history: no staging actions.
    ReadOnly,
    /// Index vs worktree: stage / discard hunks and lines.
    Unstaged,
    /// HEAD vs index: unstage hunks and lines.
    Staged,
}

/// Delay before a loading spinner appears (avoids flicker on fast loads).
const SPINNER_DELAY: Duration = Duration::from_millis(150);
/// Word-highlight cache entries kept before the cache is reset.
const INLINE_CACHE_MAX: usize = 4096;

/// Highlight ranges of a paired (removed, added) line.
type Inline = Arc<(Vec<Range<usize>>, Vec<Range<usize>>)>;

struct Shown {
    target: DiffTarget,
    diff: Arc<FileDiff>,
    layout: Layout,
    tab: usize,
}

use crate::keymap::Act as KAct;

/// Keymap actions of the diff body (see `keymap.rs`, scope `Diff`).
const DIFF_ACTS: [KAct; 7] = [
    KAct::DiffNextHunk,
    KAct::DiffPrevHunk,
    KAct::DiffStage,
    KAct::DiffUnstage,
    KAct::DiffDiscard,
    KAct::DiffSelectDown,
    KAct::DiffSelectUp,
];

#[derive(Debug, Clone, Copy)]
enum Act {
    StageHunk(usize),
    UnstageHunk(usize),
    DiscardHunk(usize),
    StageSel,
    UnstageSel,
    DiscardSel,
    CopySel,
    CopyPatch,
    External,
}

struct ConfirmDiscard {
    path: String,
    keys: Vec<LineKey>,
    what: String,
}

/// Per-view state (scroll, selected lines, options). Keep one per place it is shown.
#[derive(Default)]
pub struct DiffView {
    init: bool,
    ignore_ws: bool,
    word_diff: bool,
    side_by_side: bool,
    whole_file: bool,
    shown: Option<Shown>,
    loading_since: Option<Instant>,
    /// Startup trace only (`GITKWAKEN_STARTUP_LOG`): when the shown target was
    /// first requested, to log request-to-rendered time once.
    trace_open: Option<(DiffTarget, Instant)>,
    /// Same, to log request-to-visible-lines-highlighted time once.
    trace_hl: Option<(DiffTarget, Instant)>,
    inline: HashMap<u32, Inline>,
    sel: Selection,
    /// Line where the current drag selection started.
    drag_from: Option<u32>,
    confirm: Option<ConfirmDiscard>,
    focus_requested: bool,
    scroll_to: Option<f32>,
    offset: f32,
    view_h: f32,
    row_h: f32,
    /// Worktree size of a binary file, per target.
    file_size: Option<(DiffTarget, Option<u64>)>,
    /// Syntax highlight results of recently shown diffs.
    syntax: HlCache,
    /// Before / after preview of changed images.
    images: preview::ImagePreview,
    /// Large-file gate (`settings.diff.max_file_kb`).
    size_guard: guard::SizeGuard,
    /// Show the raw text of an LFS pointer diff.
    lfs_raw: bool,
    /// Hunk the keyboard acts on (next / previous hunk, S / U / Delete);
    /// `None` or off screen: the hunk at the top of the view.
    hunk_cursor: Option<usize>,
    /// Moving end of a keyboard line selection (Shift+Down / Shift+Up).
    line_cursor: Option<u32>,
}

impl DiffView {
    /// Gives the diff body keyboard focus on the next frame (arrows/PageUp/
    /// PageDown scroll, Ctrl+C copies selected lines, Esc leaves).
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    pub fn clear_selection(&mut self) {
        self.sel.clear();
        self.drag_from = None;
        self.line_cursor = None;
    }

    /// Forgets the displayed diff (e.g. when nothing is selected any more).
    pub fn reset(&mut self) {
        self.shown = None;
        self.loading_since = None;
        self.clear_selection();
        self.inline.clear();
    }

    /// Renders the diff of `target`. `status_gen` invalidates worktree diffs
    /// when files change on disk.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &Session,
        target: &DiffTarget,
        mode: DiffMode,
        status_gen: u64,
        shared: &mut Shared,
    ) {
        if !self.init {
            let s = &shared.settings.diff;
            self.ignore_ws = s.ignore_whitespace;
            self.word_diff = s.word_diff;
            self.side_by_side = s.side_by_side;
            self.init = true;
        }
        let context = if self.whole_file {
            u32::MAX
        } else {
            shared.settings.diff.context_lines
        };
        let tab = shared.settings.diff.tab_width.clamp(1, 16) as usize;
        zoom_code_font(ui, shared);
        // Worktree content only changes with the status; other targets are immutable.
        let diff_gen = match target {
            DiffTarget::Blobs { .. } => 0,
            _ => status_gen,
        };
        let mut failed: Option<String> = None;
        if crate::startup::active()
            && self.shown.as_ref().is_none_or(|s| &s.target != target)
            && self.trace_open.as_ref().is_none_or(|(t, _)| t != target)
        {
            self.trace_open = Some((target.clone(), Instant::now()));
            self.trace_hl = self.trace_open.clone();
        }
        if shared.settings.diff.syntax_highlight {
            // While the diff loads, so its first highlight is not delayed.
            syntax::warm(target.path());
        }
        // Large files: measure first, diff only within the limit or on request.
        let limit = shared.settings.diff.max_file_kb.saturating_mul(1024);
        let gate = self
            .size_guard
            .check(ui.ctx(), session.repo(), target, diff_gen, limit);
        let load = match gate {
            guard::Gate::Open => session.diff(target, diff_gen, self.ignore_ws, context),
            guard::Gate::Pending => Load::Loading,
            guard::Gate::TooLarge(n) => {
                self.loading_since = None;
                let mut acts: Vec<Act> = Vec::new();
                self.toolbar(ui, target, mode, false, false, &mut acts, shared);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new(format!("File is large ({})", human_size(n))).strong());
                ui.label(
                    RichText::new(format!(
                        "Files over {} are not diffed automatically (Settings › Diff).",
                        human_size(limit)
                    ))
                    .color(shared.palette.muted),
                );
                ui.horizontal(|ui| {
                    if ui.button("Show anyway").clicked() {
                        self.size_guard.allow(target);
                        ui.ctx().request_repaint();
                    }
                    if ui.button("Open in external diff tool").clicked() {
                        open_external(session, target, shared);
                    }
                });
                return;
            }
        };
        match load {
            Load::Ready(d) => {
                self.loading_since = None;
                self.accept(target, d, tab);
            }
            Load::Loading => {
                let since = *self.loading_since.get_or_insert_with(Instant::now);
                let el = since.elapsed();
                if el < SPINNER_DELAY {
                    ui.ctx().request_repaint_after(SPINNER_DELAY - el);
                }
            }
            Load::Failed(e) => {
                self.loading_since = None;
                failed = Some(e);
            }
        }
        let spinner = self
            .loading_since
            .is_some_and(|t| t.elapsed() >= SPINNER_DELAY);
        let current = self.shown.as_ref().is_some_and(|s| &s.target == target);

        let mut acts: Vec<Act> = Vec::new();
        self.toolbar(ui, target, mode, current, spinner, &mut acts, shared);
        ui.separator();

        if let Some(e) = failed {
            ui.add_space(8.0);
            ui.colored_label(
                shared.palette.removed_fg,
                format!("Cannot show the diff: {e}"),
            );
        } else if let Some(shown) = self.shown.take() {
            let d = shown.diff.clone();
            if d.binary && preview::is_image(shown.target.path()) {
                self.images
                    .show(ui, session.repo(), &shown.target, diff_gen, &shared.palette);
            } else if d.binary {
                self.binary_info(ui, session, &shown.target, shared);
            } else if let Some((old, new)) = guard::lfs_pointers(&d).filter(|_| !self.lfs_raw) {
                lfs_info(ui, old, new, shared);
                if ui.small_button("Show pointer text").clicked() {
                    self.lfs_raw = true;
                }
            } else if d.lines.is_empty() {
                ui.add_space(8.0);
                let msg = if self.ignore_ws {
                    "No changes (whitespace ignored)"
                } else {
                    "No content changes"
                };
                ui.label(RichText::new(msg).color(shared.palette.muted));
            } else {
                let editable = current && self.editable(mode, &shown.target);
                self.body(ui, &shown, mode, editable, &mut acts, shared);
                if let Some((t, at)) = self.trace_open.take_if(|(t, _)| *t == shown.target) {
                    let lines = shown.diff.lines.len();
                    crate::startup::record(
                        &format!("diff_open:{}:{lines}_lines", t.path()),
                        at.elapsed(),
                    );
                }
            }
            self.shown = Some(shown);
        } else if spinner {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading diff…").color(shared.palette.muted));
            });
        }

        for a in acts {
            self.apply(a, ui, session, shared);
        }
        self.confirm_modal(ui, session, shared);
    }

    /// Takes a freshly loaded diff, keeping selection and caches when only
    /// the `Arc` changed but not the content.
    fn accept(&mut self, target: &DiffTarget, d: Arc<FileDiff>, tab: usize) {
        if self.shown.as_ref().is_none_or(|s| &s.target != target) {
            self.lfs_raw = false;
        }
        if let Some(s) = &mut self.shown {
            if &s.target == target && s.tab == tab {
                if Arc::ptr_eq(&s.diff, &d) {
                    return;
                }
                let same =
                    s.diff.lines == d.lines && s.diff.text == d.text && s.diff.binary == d.binary;
                if same {
                    self.syntax.rekey(&s.diff, &d);
                    s.diff = d;
                    return;
                }
                // Same file, new content: line indices are invalid now. The
                // hunk cursor stays: after staging hunk 2, hunk 2 is the next one.
                self.clear_selection();
                self.hunk_cursor = self
                    .hunk_cursor
                    .map(|h| h.min(d.hunks.len().saturating_sub(1)));
            } else if &s.target != target {
                self.clear_selection();
                self.hunk_cursor = None;
                self.scroll_to = Some(0.0);
            }
        } else {
            self.clear_selection();
            self.hunk_cursor = None;
            self.scroll_to = Some(0.0);
        }
        self.inline.clear();
        self.shown = Some(Shown {
            layout: Layout::build(&d, tab),
            target: target.clone(),
            diff: d,
            tab,
        });
    }

    /// Hunk/line actions need line positions that match a full-context
    /// diff of the same file.
    fn editable(&self, mode: DiffMode, target: &DiffTarget) -> bool {
        match (mode, target) {
            (DiffMode::ReadOnly, _) => false,
            (_, DiffTarget::Blobs { .. }) => false,
            // Unstaging lines of a rename would read HEAD at the new path.
            (
                DiffMode::Staged,
                DiffTarget::Staged {
                    old_path: Some(_), ..
                },
            ) => false,
            _ => true,
        }
    }

    /// Diff header: file path, change counts and view controls.
    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ui: &mut Ui,
        target: &DiffTarget,
        _mode: DiffMode,
        current: bool,
        spinner: bool,
        acts: &mut Vec<Act>,
        shared: &mut Shared,
    ) {
        let p = shared.palette;
        let path_labels = |ui: &mut Ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let path = target.path();
                let (dir, name) = split_path(path);
                ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                    .on_hover_text(path);
                if !dir.is_empty() {
                    ui.add(egui::Label::new(RichText::new(dir).color(p.muted)).truncate());
                }
            });
        };
        // Narrow panes: the path gets its own row above the controls.
        let narrow = ui.available_width() < NARROW_TOOLBAR;
        let short = ui.available_width() < SHORT_LABELS;
        if narrow {
            ui.horizontal(|ui| path_labels(ui));
        }
        // Controls are laid out first (right to left) so the path label only
        // gets the space left over and truncates instead of overlapping them.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has_diff = current
                    && self
                        .shown
                        .as_ref()
                        .is_some_and(|s| !s.diff.lines.is_empty());
                if ui
                    .add_enabled(has_diff, egui::Button::new("⎘").small())
                    .on_hover_text("Copy as patch")
                    .clicked()
                {
                    acts.push(Act::CopyPatch);
                }
                let s = &mut shared.settings.diff;
                if ui
                    .toggle_value(&mut self.side_by_side, "Split")
                    .on_hover_text("Side-by-side view")
                    .changed()
                {
                    s.side_by_side = self.side_by_side;
                }
                if ui
                    .toggle_value(&mut self.word_diff, "Words")
                    .on_hover_text("Highlight changed words")
                    .changed()
                {
                    s.word_diff = self.word_diff;
                }
                if ui
                    .toggle_value(&mut self.ignore_ws, if short { "WS" } else { "Ignore WS" })
                    .on_hover_text("Ignore whitespace (disables hunk and line staging)")
                    .changed()
                {
                    s.ignore_whitespace = self.ignore_ws;
                }
                ui.toggle_value(
                    &mut self.whole_file,
                    if short { "Whole" } else { "Whole file" },
                )
                .on_hover_text("Show the whole file");
                ui.add_enabled_ui(!self.whole_file, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut s.context_lines)
                            .range(0..=100)
                            .speed(0.1),
                    )
                    .on_hover_text("Context lines");
                });
                ui.separator();
                let hunks = self
                    .shown
                    .as_ref()
                    .filter(|_| current)
                    .map(|s| s.layout.hunk_rows(self.side_by_side).len())
                    .unwrap_or(0);
                let hint = |text: &str, a: KAct| match crate::keymap::text(ui.ctx(), a) {
                    sc if sc.is_empty() => text.to_string(),
                    sc => format!("{text} ({sc})"),
                };
                let (next_hint, prev_hint) = (
                    hint("Next hunk", KAct::DiffNextHunk),
                    hint("Previous hunk", KAct::DiffPrevHunk),
                );
                if ui
                    .add_enabled(hunks > 0, egui::Button::new("▼").small())
                    .on_hover_text(next_hint)
                    .clicked()
                {
                    self.jump_hunk(true);
                }
                if ui
                    .add_enabled(hunks > 0, egui::Button::new("▲").small())
                    .on_hover_text(prev_hint)
                    .clicked()
                {
                    self.jump_hunk(false);
                }
                if let Some(s) = self.shown.as_ref().filter(|_| current)
                    && !s.diff.binary
                {
                    // Right to left: deletions first so the order reads "+a −d".
                    ui.label(RichText::new(format!("−{}", s.diff.deletions)).color(p.removed_fg));
                    ui.label(RichText::new(format!("+{}", s.diff.additions)).color(p.added_fg));
                }
                if spinner {
                    ui.spinner();
                }
                if !narrow {
                    path_labels(ui);
                }
            });
        });
    }

    fn jump_hunk(&mut self, next: bool) {
        if let Some(s) = self.shown.take() {
            self.jump_in(&s.layout, next);
            self.shown = Some(s);
        }
    }

    /// First and last display row on screen (from the last frame).
    fn view_rows(&self) -> (u32, u32) {
        if self.row_h <= 0.0 {
            return (0, 0);
        }
        let top = (self.offset / self.row_h).round() as u32;
        let bottom = ((self.offset + self.view_h) / self.row_h).floor() as u32;
        (top, bottom.max(top))
    }

    /// The hunk cursor when it is on screen.
    fn hunk_cursor_on_screen(&self, layout: &Layout) -> Option<usize> {
        let hr = layout.hunk_rows(self.side_by_side);
        let total = layout.rows(self.side_by_side).len() as u32;
        let (top, bottom) = self.view_rows();
        self.hunk_cursor
            .filter(|&h| model::hunk_on_screen(hr, total, h, top, bottom))
    }

    /// The hunk keyboard actions apply to: the hunk cursor when on screen,
    /// else the hunk at the top of the view.
    fn current_hunk(&self, layout: &Layout) -> Option<usize> {
        self.hunk_cursor_on_screen(layout)
            .or_else(|| model::hunk_at(layout.hunk_rows(self.side_by_side), self.view_rows().0))
    }

    /// Moves the hunk cursor to the next / previous hunk and scrolls its
    /// header to the top.
    fn jump_in(&mut self, layout: &Layout, next: bool) {
        if self.row_h <= 0.0 {
            return;
        }
        let hr = layout.hunk_rows(self.side_by_side);
        let top = self.view_rows().0;
        let target = match self.hunk_cursor_on_screen(layout) {
            Some(h) if next => (h + 1 < hr.len()).then_some(h + 1),
            Some(h) => h.checked_sub(1),
            None if next => hr.iter().position(|&r| r > top),
            None => hr.iter().rposition(|&r| r < top),
        };
        if let Some(t) = target {
            self.hunk_cursor = Some(t);
            self.scroll_to = Some(hr[t] as f32 * self.row_h);
        }
    }

    fn binary_info(
        &mut self,
        ui: &mut Ui,
        session: &Session,
        target: &DiffTarget,
        shared: &Shared,
    ) {
        let p = shared.palette;
        let path = target.path();
        let image = preview::is_image(path);
        if !matches!(&self.file_size, Some((t, _)) if t == target) {
            // One stat per target, only for files that exist in the worktree.
            let size = match target {
                DiffTarget::Unstaged { path } => std::fs::metadata(session.repo().abs(path))
                    .ok()
                    .map(|m| m.len()),
                _ => None,
            };
            self.file_size = Some((target.clone(), size));
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if image {
                "Image changed"
            } else {
                "Binary file"
            })
            .strong(),
        );
        if let Some((_, Some(n))) = &self.file_size {
            ui.label(RichText::new(format!("Working copy: {}", human_size(*n))).color(p.muted));
        }
        if matches!(target, DiffTarget::Unstaged { .. }) && ui.button("Open file").clicked() {
            let _ = open::that(session.repo().abs(path));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn body(
        &mut self,
        ui: &mut Ui,
        shown: &Shown,
        mode: DiffMode,
        editable: bool,
        acts: &mut Vec<Act>,
        shared: &Shared,
    ) {
        let p = shared.palette;
        let d = &*shown.diff;
        let sbs = self.side_by_side;
        let rows = shown.layout.rows(sbs);
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let small = egui::TextStyle::Small.resolve(ui.style());
        let (font_h, char_w) = ui
            .ctx()
            .fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
        let row_h = (font_h + 5.0).round().max(16.0)
            + crate::theme::Density::of(shared.settings.compact).row_extra();
        self.row_h = row_h;
        let num_w = shown.layout.digits as f32 * char_w + 12.0;
        let sign_w = char_w + 8.0;
        let text_max = shown.layout.max_cols as f32 * char_w + 48.0;
        let tab = shown.tab;
        let body_id = ui.make_persistent_id("gk-diff-body");
        let text_color = ui.visuals().text_color();
        let faint = ui.visuals().faint_bg_color;
        let hl = |kind: LineKind| {
            if kind == LineKind::Added {
                p.added_word
            } else {
                p.removed_word
            }
        };
        let mut sa = egui::ScrollArea::both()
            .auto_shrink([false, false])
            .id_salt("gk-diff-scroll");
        if let Some(y) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset(y.max(0.0));
        }
        let out = sa.show_viewport(ui, |ui, vp| {
            let view_w = vp.width();
            let (gutter, content_w) = if sbs {
                let g = num_w + sign_w;
                (g, view_w + (text_max - (view_w / 2.0 - g)).max(0.0))
            } else {
                let g = 2.0 * num_w + sign_w;
                (g, g + text_max.max(view_w - g))
            };
            ui.set_min_width(content_w);
            ui.set_min_height(rows.len() as f32 * row_h);
            let origin = ui.max_rect().min;
            let vis = Rect::from_min_size(origin + vp.min.to_vec2(), vp.size());
            let scroll_x = vp.min.x;
            let first = ((vp.min.y / row_h).floor().max(0.0) as usize).min(rows.len());
            let last = (((vp.max.y / row_h).ceil() as usize) + 1).min(rows.len());

            // Token colors of the visible lines (and a margin), computed in
            // the background; plain text until a batch lands.
            let syn: Option<Arc<Highlights>> = if shared.settings.diff.syntax_highlight {
                let vis_lines = model::row_lines(&rows[first..last], &d.hunks);
                let syn = self.syntax.get(
                    &shown.diff,
                    p.dark,
                    shown.target.path(),
                    vis_lines.clone(),
                    Some(ui.ctx()),
                );
                if syn.as_ref().is_some_and(|h| h.covers(vis_lines))
                    && let Some((t, at)) = self.trace_hl.take_if(|(t, _)| *t == shown.target)
                {
                    let lines = d.lines.len();
                    crate::startup::record(
                        &format!("diff_highlighted:{}:{lines}_lines", t.path()),
                        at.elapsed(),
                    );
                }
                syn
            } else {
                None
            };
            let spans_of = |i: u32, kind: LineKind, old_side: bool| -> &[Span] {
                syn.as_deref()
                    .map_or(&[], |h| h.spans(i as usize, kind, old_side))
            };

            // One interaction surface for selection; buttons added later sit on top.
            let body = ui.interact(vis, body_id, Sense::click_and_drag());
            let row_at = |pos: Pos2| -> Option<usize> {
                let r = ((pos.y - origin.y) / row_h).floor();
                (r >= 0.0 && (r as usize) < rows.len()).then_some(r as usize)
            };
            let left_side = |pos: Pos2| pos.x < vis.center().x;
            // Line under the pointer; hunk headers map to their first line.
            let line_at = |pos: Pos2| -> Option<u32> {
                match rows[row_at(pos)?] {
                    Row::Hunk(h) => d.hunks.get(h as usize).map(|hk| hk.lines.start as u32),
                    Row::Line(i) => Some(i),
                    Row::Pair(a, b) => {
                        let (x, y) = if left_side(pos) { (a, b) } else { (b, a) };
                        if x != NONE {
                            Some(x)
                        } else if y != NONE {
                            Some(y)
                        } else {
                            None
                        }
                    }
                }
            };
            let is_changed = |i: u32| {
                d.lines
                    .get(i as usize)
                    .is_some_and(|l| l.kind != LineKind::Context)
            };
            let on_header =
                |pos: Pos2| row_at(pos).is_some_and(|r| matches!(rows[r], Row::Hunk(_)));

            // ---- selection input
            let (pressed, shift, command) = ui.input(|i| {
                (
                    i.pointer.primary_pressed(),
                    i.modifiers.shift,
                    i.modifiers.command,
                )
            });
            if pressed && body.is_pointer_button_down_on() {
                body.request_focus();
                if let Some(pos) = body.interact_pointer_pos() {
                    let at = if on_header(pos) {
                        Press::Header
                    } else {
                        line_at(pos).map_or(Press::Empty, Press::Line)
                    };
                    self.drag_from = self.sel.press(at, shift, command, self.drag_from, &d.lines);
                    self.line_cursor = None;
                }
            }
            if body.dragged()
                && let (Some(from), Some(pos)) =
                    (self.drag_from, ui.input(|i| i.pointer.latest_pos()))
            {
                let clamped = pos2(pos.x, pos.y.clamp(vis.top() + 1.0, vis.bottom() - 1.0));
                if let Some(i) = line_at(clamped) {
                    self.sel.range(from, i, &d.lines, false);
                    self.sel.anchor = Some(from);
                }
                // Auto-scroll while dragging past the edges.
                if pos.y > vis.bottom() - row_h * 0.5 {
                    self.scroll_to = Some(vp.min.y + row_h);
                    ui.ctx().request_repaint();
                } else if pos.y < vis.top() + row_h * 0.5 {
                    self.scroll_to = Some(vp.min.y - row_h);
                    ui.ctx().request_repaint();
                }
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                self.drag_from = None;
            }
            if body.secondary_clicked()
                && let Some(i) = body.interact_pointer_pos().and_then(line_at)
                && is_changed(i)
                && !self.sel.contains(i)
            {
                self.sel.single(i);
            }
            body.context_menu(|ui| {
                let n = self.sel.lines.len();
                let ctx = ui.ctx().clone();
                let item = |text: String, a: KAct| -> egui::Button<'static> {
                    let sc = crate::keymap::text(&ctx, a);
                    let b = egui::Button::new(text);
                    if sc.is_empty() {
                        b
                    } else {
                        b.shortcut_text(RichText::new(sc).weak())
                    }
                };
                if editable && n > 0 {
                    match mode {
                        DiffMode::Unstaged => {
                            if ui
                                .add_enabled(
                                    !self.ignore_ws,
                                    item(format!("Stage {n} selected line(s)"), KAct::DiffStage),
                                )
                                .clicked()
                            {
                                acts.push(Act::StageSel);
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    !self.ignore_ws,
                                    item(
                                        format!("Discard {n} selected line(s)…"),
                                        KAct::DiffDiscard,
                                    ),
                                )
                                .clicked()
                            {
                                acts.push(Act::DiscardSel);
                                ui.close();
                            }
                        }
                        DiffMode::Staged => {
                            if ui
                                .add_enabled(
                                    !self.ignore_ws,
                                    item(
                                        format!("Unstage {n} selected line(s)"),
                                        KAct::DiffUnstage,
                                    ),
                                )
                                .clicked()
                            {
                                acts.push(Act::UnstageSel);
                                ui.close();
                            }
                        }
                        DiffMode::ReadOnly => {}
                    }
                    ui.separator();
                }
                if ui
                    .add_enabled(n > 0, egui::Button::new("Copy selected lines"))
                    .clicked()
                {
                    acts.push(Act::CopySel);
                    ui.close();
                }
                if ui.button("Copy patch").clicked() {
                    acts.push(Act::CopyPatch);
                    ui.close();
                }
                if ui.button("Open in external diff tool").clicked() {
                    acts.push(Act::External);
                    ui.close();
                }
                if ui
                    .add_enabled(n > 0, egui::Button::new("Clear selection"))
                    .clicked()
                {
                    self.sel.clear();
                    ui.close();
                }
            });

            // ---- keyboard (when focused)
            if self.focus_requested {
                body.request_focus();
                self.focus_requested = false;
            }
            if body.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        body_id,
                        egui::EventFilter {
                            tab: false,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
                // Keymap actions first: they take Shift+Down / Shift+Up
                // before the scroll keys below see them.
                for a in crate::keymap::pressed(ui.ctx(), &DIFF_ACTS) {
                    self.key_action(a, shown, mode, editable, acts);
                }
                use egui::Key;
                let (up, down, pgup, pgdn, home, end, esc, copy, all) = ui.input_mut(|i| {
                    (
                        i.key_pressed(Key::ArrowUp),
                        i.key_pressed(Key::ArrowDown),
                        i.key_pressed(Key::PageUp),
                        i.key_pressed(Key::PageDown),
                        i.key_pressed(Key::Home),
                        i.key_pressed(Key::End),
                        i.key_pressed(Key::Escape),
                        i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                        i.consume_key(egui::Modifiers::COMMAND, Key::A),
                    )
                });
                let y = vp.min.y;
                let page = (vp.height() - row_h).max(row_h);
                let max = (rows.len() as f32 * row_h - vp.height()).max(0.0);
                let to = if up {
                    Some(y - row_h)
                } else if down {
                    Some(y + row_h)
                } else if pgup {
                    Some(y - page)
                } else if pgdn {
                    Some(y + page)
                } else if home {
                    Some(0.0)
                } else if end {
                    Some(max)
                } else {
                    None
                };
                if let Some(t) = to {
                    self.scroll_to = Some(t.clamp(0.0, max));
                    ui.ctx().request_repaint();
                }
                if copy && !self.sel.is_empty() {
                    acts.push(Act::CopySel);
                }
                if all {
                    self.sel
                        .range(0, d.lines.len().saturating_sub(1) as u32, &d.lines, false);
                }
                if esc {
                    body.surrender_focus();
                }
                ui.painter().rect_stroke(
                    vis.shrink(1.0),
                    0.0,
                    egui::Stroke::new(1.0, p.selection.gamma_multiply(2.0)),
                    egui::StrokeKind::Inside,
                );
            }

            // ---- paint visible rows
            // With keyboard focus, the hunk S / U / Delete act on is outlined.
            let key_hunk = body
                .has_focus()
                .then(|| self.current_hunk(&shown.layout))
                .flatten();
            let painter = ui.painter().clone();
            for (r, row) in rows.iter().enumerate().take(last).skip(first) {
                let y = origin.y + r as f32 * row_h;
                let rect = Rect::from_min_max(pos2(vis.left(), y), pos2(vis.right(), y + row_h));
                match *row {
                    Row::Hunk(h) => {
                        let h = h as usize;
                        painter.rect_filled(rect, 0.0, p.hunk_bg);
                        if key_hunk == Some(h) {
                            painter.rect_stroke(
                                rect.shrink(1.0),
                                0.0,
                                (1.0, p.branch),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if let Some(hk) = d.hunks.get(h) {
                            painter.text(
                                pos2(rect.left() + 8.0, rect.center().y),
                                Align2::LEFT_CENTER,
                                hk.header(),
                                small.clone(),
                                p.muted,
                            );
                        }
                        if editable {
                            self.hunk_buttons(ui, rect, h, d, mode, &small, acts);
                        }
                    }
                    Row::Line(i) => {
                        let Some(l) = d.lines.get(i as usize) else {
                            continue;
                        };
                        let bg = line_bg(&p, l.kind);
                        if bg != Color32::TRANSPARENT {
                            painter.rect_filled(rect, 0.0, bg);
                        }
                        let selected = self.sel.contains(i);
                        if selected {
                            painter.rect_filled(rect, 0.0, p.selection);
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, vec2(3.0, row_h)),
                                0.0,
                                p.branch,
                            );
                        }
                        let cy = rect.center().y;
                        let x0 = rect.left();
                        if l.old_no != 0 {
                            number(
                                &painter,
                                pos2(x0 + num_w - 6.0, cy),
                                l.old_no,
                                &font,
                                p.muted,
                            );
                        }
                        if l.new_no != 0 {
                            number(
                                &painter,
                                pos2(x0 + 2.0 * num_w - 6.0, cy),
                                l.new_no,
                                &font,
                                p.muted,
                            );
                        }
                        let (sign, sc) = sign_of(&p, l.kind);
                        painter.text(
                            pos2(x0 + 2.0 * num_w + 2.0, cy),
                            Align2::LEFT_CENTER,
                            sign,
                            font.clone(),
                            sc,
                        );
                        let text_rect = Rect::from_min_max(pos2(x0 + gutter, rect.top()), rect.max);
                        let hls = self.highlights(d, &shown.layout, i);
                        let job = line_job(
                            &line_text(d, l, false),
                            &hls,
                            spans_of(i, l.kind, false),
                            hl(l.kind),
                            &font,
                            text_color,
                            p.muted,
                            tab,
                            l.no_eol(),
                        );
                        let galley = painter.layout_job(job);
                        let gy = cy - galley.size().y / 2.0;
                        painter.with_clip_rect(text_rect.intersect(vis)).galley(
                            pos2(text_rect.left() + 4.0 - scroll_x, gy),
                            galley,
                            text_color,
                        );
                    }
                    Row::Pair(a, b) => {
                        let mid = vis.center().x;
                        for (idx, left) in [(a, true), (b, false)] {
                            let half = if left {
                                Rect::from_min_max(rect.min, pos2(mid, rect.bottom()))
                            } else {
                                Rect::from_min_max(pos2(mid, rect.top()), rect.max)
                            };
                            let Some(l) =
                                (idx != NONE).then(|| d.lines.get(idx as usize)).flatten()
                            else {
                                painter.rect_filled(half, 0.0, faint);
                                continue;
                            };
                            let bg = line_bg(&p, l.kind);
                            if bg != Color32::TRANSPARENT {
                                painter.rect_filled(half, 0.0, bg);
                            }
                            if l.kind != LineKind::Context && self.sel.contains(idx) {
                                painter.rect_filled(half, 0.0, p.selection);
                                painter.rect_filled(
                                    Rect::from_min_size(half.min, vec2(3.0, row_h)),
                                    0.0,
                                    p.branch,
                                );
                            }
                            let cy = half.center().y;
                            let no = if left { l.old_no } else { l.new_no };
                            if no != 0 {
                                number(
                                    &painter,
                                    pos2(half.left() + num_w - 6.0, cy),
                                    no,
                                    &font,
                                    p.muted,
                                );
                            }
                            let (sign, sc) = sign_of(&p, l.kind);
                            painter.text(
                                pos2(half.left() + num_w + 2.0, cy),
                                Align2::LEFT_CENTER,
                                sign,
                                font.clone(),
                                sc,
                            );
                            let text_rect = Rect::from_min_max(
                                pos2(half.left() + gutter, half.top()),
                                half.max,
                            );
                            let hls = self.highlights(d, &shown.layout, idx);
                            let job = line_job(
                                &line_text(d, l, left),
                                &hls,
                                spans_of(idx, l.kind, left),
                                hl(l.kind),
                                &font,
                                text_color,
                                p.muted,
                                tab,
                                l.no_eol(),
                            );
                            let galley = painter.layout_job(job);
                            let gy = cy - galley.size().y / 2.0;
                            painter.with_clip_rect(text_rect.intersect(vis)).galley(
                                pos2(text_rect.left() + 4.0 - scroll_x, gy),
                                galley,
                                text_color,
                            );
                        }
                    }
                }
            }
            if sbs {
                let x = vis.center().x;
                painter.vline(
                    x,
                    vis.y_range(),
                    egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );
            }
            // Gutter separator.
            let gx = if sbs {
                vis.left() + num_w + sign_w
            } else {
                vis.left() + 2.0 * num_w + sign_w
            };
            painter.vline(gx, vis.y_range(), egui::Stroke::new(1.0, faint));
        });
        self.offset = out.state.offset.y;
        self.view_h = out.inner_rect.height();
    }

    /// A diff shortcut (keymap scope `Diff`) while the body has focus.
    /// Stage / unstage / discard act on the selected lines when there are
    /// any, else on the current hunk.
    fn key_action(
        &mut self,
        a: KAct,
        shown: &Shown,
        mode: DiffMode,
        editable: bool,
        acts: &mut Vec<Act>,
    ) {
        let d = &*shown.diff;
        let layout = &shown.layout;
        let can_edit = editable && !self.ignore_ws;
        let lines = !self.sel.is_empty();
        let hunk = self.current_hunk(layout);
        match a {
            KAct::DiffNextHunk => self.jump_in(layout, true),
            KAct::DiffPrevHunk => self.jump_in(layout, false),
            KAct::DiffStage if can_edit && mode == DiffMode::Unstaged => {
                acts.extend(if lines {
                    Some(Act::StageSel)
                } else {
                    hunk.map(Act::StageHunk)
                });
            }
            KAct::DiffUnstage if can_edit && mode == DiffMode::Staged => {
                acts.extend(if lines {
                    Some(Act::UnstageSel)
                } else {
                    hunk.map(Act::UnstageHunk)
                });
            }
            KAct::DiffDiscard if can_edit && mode == DiffMode::Unstaged => {
                acts.extend(if lines {
                    Some(Act::DiscardSel)
                } else {
                    hunk.map(Act::DiscardHunk)
                });
            }
            KAct::DiffSelectDown | KAct::DiffSelectUp => {
                let down = a == KAct::DiffSelectDown;
                // Continue from the keyboard cursor, else from the mouse
                // selection's end, else start in the current hunk.
                let from = self.line_cursor.or_else(|| {
                    if down {
                        self.sel.lines.last().copied()
                    } else {
                        self.sel.lines.first().copied()
                    }
                });
                let Some(j) = model::step_changed(d, from, down, hunk) else {
                    return;
                };
                match (from, self.sel.anchor.or(from)) {
                    (Some(_), Some(anchor)) => {
                        self.sel.range(anchor, j, &d.lines, false);
                        self.sel.anchor = Some(anchor);
                    }
                    _ => self.sel.single(j),
                }
                self.line_cursor = Some(j);
                self.hunk_cursor = model::hunk_of_line(d, j).or(self.hunk_cursor);
                // Keep the moving end on screen.
                if let Some(r) = model::row_of_line(layout.rows(self.side_by_side), j)
                    && self.row_h > 0.0
                {
                    let (y0, y1) = (r as f32 * self.row_h, (r + 1) as f32 * self.row_h);
                    if y0 < self.offset {
                        self.scroll_to = Some(y0);
                    } else if y1 > self.offset + self.view_h {
                        self.scroll_to = Some(y1 - self.view_h);
                    }
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn hunk_buttons(
        &self,
        ui: &mut Ui,
        rect: Rect,
        h: usize,
        d: &FileDiff,
        mode: DiffMode,
        font: &FontId,
        acts: &mut Vec<Act>,
    ) {
        let n_sel = self.sel.lines.len();
        let in_hunk = self.sel.count_in_hunk(d, h) > 0;
        let lines = in_hunk && n_sel > 0;
        let buttons: Vec<(String, Act, KAct)> = match mode {
            DiffMode::Unstaged if lines => vec![
                (
                    format!("Stage lines ({n_sel})"),
                    Act::StageSel,
                    KAct::DiffStage,
                ),
                (
                    format!("Discard lines ({n_sel})…"),
                    Act::DiscardSel,
                    KAct::DiffDiscard,
                ),
            ],
            DiffMode::Unstaged => vec![
                ("Stage hunk".into(), Act::StageHunk(h), KAct::DiffStage),
                (
                    "Discard hunk…".into(),
                    Act::DiscardHunk(h),
                    KAct::DiffDiscard,
                ),
            ],
            DiffMode::Staged if lines => vec![(
                format!("Unstage lines ({n_sel})"),
                Act::UnstageSel,
                KAct::DiffUnstage,
            )],
            DiffMode::Staged => vec![(
                "Unstage hunk".into(),
                Act::UnstageHunk(h),
                KAct::DiffUnstage,
            )],
            DiffMode::ReadOnly => Vec::new(),
        };
        let enabled = !self.ignore_ws;
        let mut x = rect.right() - 6.0;
        let color = ui.visuals().text_color();
        for (label, act, key) in buttons.into_iter().rev() {
            let sc = crate::keymap::text(ui.ctx(), key);
            let w = ui
                .painter()
                .layout_no_wrap(label.clone(), font.clone(), color)
                .size()
                .x
                + 14.0;
            let r = Rect::from_min_max(pos2(x - w, rect.top() + 1.0), pos2(x, rect.bottom() - 1.0));
            x -= w + 4.0;
            let builder = egui::UiBuilder::new()
                .max_rect(r)
                .layout(egui::Layout::left_to_right(egui::Align::Center));
            let resp = ui
                .scope_builder(builder, |ui| {
                    let b = egui::Button::new(RichText::new(label).font(font.clone()))
                        .small()
                        .min_size(r.size());
                    ui.add_enabled(enabled, b)
                })
                .inner
                .on_disabled_hover_text(
                    "Turn off \"Ignore WS\" to stage or discard parts of a file",
                );
            let resp = if sc.is_empty() {
                resp
            } else {
                resp.on_hover_text(format!("Shortcut: {sc} (diff focused)"))
            };
            if resp.clicked() {
                acts.push(act);
            }
        }
    }

    /// Word highlights of line `i` (only when it is half of a removed/added pair).
    fn highlights(&mut self, d: &FileDiff, layout: &Layout, i: u32) -> Vec<Range<usize>> {
        if !self.word_diff {
            return Vec::new();
        }
        let Some(&other) = layout.partner.get(i as usize) else {
            return Vec::new();
        };
        if other == NONE {
            return Vec::new();
        }
        let (removed, added) = if d.lines[i as usize].kind == LineKind::Removed {
            (i, other)
        } else {
            (other, i)
        };
        if self.inline.len() > INLINE_CACHE_MAX {
            self.inline.clear();
        }
        let e = self.inline.entry(removed).or_insert_with(|| {
            let a = d.line_text(&d.lines[removed as usize]);
            let b = d.line_text(&d.lines[added as usize]);
            Arc::new(gk_diff::inline_changes(&a, &b))
        });
        if i == removed {
            e.0.clone()
        } else {
            e.1.clone()
        }
    }

    fn apply(&mut self, act: Act, ui: &Ui, session: &Session, shared: &Shared) {
        let Some(s) = &self.shown else { return };
        let path = s.target.path().to_string();
        match act {
            Act::CopySel => ui.ctx().copy_text(self.sel.text(&s.diff)),
            Act::External => open_external(session, &s.target, shared),
            Act::CopyPatch => {
                let old = match &s.target {
                    DiffTarget::Staged {
                        old_path: Some(o), ..
                    } => o.clone(),
                    _ => path.clone(),
                };
                ui.ctx()
                    .copy_text(gk_diff::to_unified(&old, &path, &s.diff));
            }
            Act::StageHunk(h) => stage_lines(
                session,
                path,
                model::hunk_keys(&s.diff, h),
                false,
                "Stage hunk",
            ),
            Act::UnstageHunk(h) => stage_lines(
                session,
                path,
                model::hunk_keys(&s.diff, h),
                true,
                "Unstage hunk",
            ),
            Act::StageSel => {
                stage_lines(session, path, self.sel.keys(&s.diff), false, "Stage lines");
                self.sel.clear();
            }
            Act::UnstageSel => {
                stage_lines(session, path, self.sel.keys(&s.diff), true, "Unstage lines");
                self.sel.clear();
            }
            Act::DiscardHunk(h) => {
                let keys = model::hunk_keys(&s.diff, h);
                self.ask_discard(session, path, keys, "this hunk".into(), shared);
            }
            Act::DiscardSel => {
                let keys = self.sel.keys(&s.diff);
                let what = format!("{} selected line(s)", keys.len());
                self.ask_discard(session, path, keys, what, shared);
            }
        }
    }

    fn ask_discard(
        &mut self,
        session: &Session,
        path: String,
        keys: Vec<LineKey>,
        what: String,
        shared: &Shared,
    ) {
        if keys.is_empty() {
            return;
        }
        if shared.settings.confirmations.discard {
            self.confirm = Some(ConfirmDiscard { path, keys, what });
        } else {
            discard_lines(session, path, keys);
            self.sel.clear();
        }
    }

    fn confirm_modal(&mut self, ui: &Ui, session: &Session, shared: &Shared) {
        let Some(c) = &self.confirm else { return };
        let mut decision: Option<bool> = None;
        let resp = egui::Modal::new(Id::new("gk-diff-discard-confirm")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.heading("Discard changes");
            ui.add_space(4.0);
            ui.label(format!("Discard {} in {}?", c.what, c.path));
            ui.label(
                RichText::new("The working copy file is modified (a copy is saved: \"Discarded changes…\" restores it).")
                    .color(shared.palette.removed_fg),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Discard").clicked() {
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
        match decision {
            Some(true) => {
                if let Some(c) = self.confirm.take() {
                    discard_lines(session, c.path, c.keys);
                    self.sel.clear();
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

fn stage_lines(session: &Session, path: String, keys: Vec<LineKey>, unstage: bool, label: &str) {
    if keys.is_empty() {
        return;
    }
    session.run_index_op(format!("{label}: {path}"), move |repo| {
        repo.stage_lines(&path, unstage, &model::picker(&keys))
    });
}

fn discard_lines(session: &Session, path: String, keys: Vec<LineKey>) {
    if keys.is_empty() {
        return;
    }
    session.run_index_op(format!("Discard lines: {path}"), move |repo| {
        repo.discard_lines(&path, &model::picker(&keys))
    });
}

fn line_bg(p: &crate::theme::Palette, kind: LineKind) -> Color32 {
    match kind {
        LineKind::Added => p.added_bg,
        LineKind::Removed => p.removed_bg,
        LineKind::Context => Color32::TRANSPARENT,
    }
}

fn sign_of(p: &crate::theme::Palette, kind: LineKind) -> (&'static str, Color32) {
    match kind {
        LineKind::Added => ("+", p.added_fg),
        LineKind::Removed => ("-", p.removed_fg),
        LineKind::Context => (" ", p.muted),
    }
}

fn number(painter: &egui::Painter, right_center: Pos2, n: u32, font: &FontId, color: Color32) {
    painter.text(
        right_center,
        Align2::RIGHT_CENTER,
        n.to_string(),
        font.clone(),
        color,
    );
}

/// Display text of a line; `old_side` uses the old bytes of context lines
/// (they differ when whitespace is ignored).
fn line_text<'a>(d: &'a FileDiff, l: &Line, old_side: bool) -> Cow<'a, str> {
    if old_side && l.kind == LineKind::Context && l.old_text != l.text {
        let b = d.old_line_bytes(l);
        String::from_utf8_lossy(b.strip_suffix(b"\r").unwrap_or(b))
    } else {
        d.line_text(l)
    }
}

#[allow(clippy::too_many_arguments)]
fn line_job(
    text: &str,
    hl: &[Range<usize>],
    syn: &[Span],
    hl_color: Color32,
    font: &FontId,
    color: Color32,
    muted: Color32,
    tab: usize,
    no_eol: bool,
) -> LayoutJob {
    let (cut, was_cut) = model::display_cut(text);
    let mut job = LayoutJob::default();
    let mut col = 0usize;
    let mut buf = String::new();
    // Word-highlight segments (background), split further at token
    // boundaries (foreground). Both span lists are sorted and disjoint.
    let mut si = 0usize;
    for (r, h) in model::segments(cut.len(), hl) {
        let mut pos = r.start;
        while pos < r.end {
            while si < syn.len() && (syn[si].end as usize) <= pos {
                si += 1;
            }
            let (end, fg) = match syn.get(si) {
                Some(s) if (s.start as usize) <= pos => ((s.end as usize).min(r.end), s.color),
                Some(s) => ((s.start as usize).min(r.end), color),
                None => (r.end, color),
            };
            let piece = pos..end;
            pos = end;
            buf.clear();
            let Some(part) = cut.get(piece) else { continue };
            model::expand_tabs(part, &mut col, tab, &mut buf);
            let format = TextFormat {
                font_id: font.clone(),
                color: fg,
                background: if h { hl_color } else { Color32::TRANSPARENT },
                ..Default::default()
            };
            job.append(&buf, 0.0, format);
        }
    }
    let note = TextFormat {
        font_id: font.clone(),
        color: muted,
        ..Default::default()
    };
    if was_cut {
        job.append(" …", 0.0, note.clone());
    }
    if no_eol {
        job.append("  ⏎ no newline at end of file", 0.0, note);
    }
    job
}

/// `("dir/sub/", "file.rs")`
/// Opens `target` (unstaged, staged or commit diff) in the configured
/// external diff tool (`settings.external_diff_tool`, `$LOCAL` / `$REMOTE`):
/// the sides are written to temp files on a worker thread. Errors become
/// messages of the tab. Usable from any view (e.g. commit details menus).
/// Ctrl + mouse wheel over a diff: code font size (Settings › Code font size; diffs, blame,
/// console, commit ids), saved like any other setting.
fn zoom_code_font(ui: &egui::Ui, shared: &mut Shared) {
    if !ui.rect_contains_pointer(ui.max_rect()) {
        return;
    }
    let zoom = ui.input(|i| i.zoom_delta());
    if (zoom - 1.0).abs() < 1e-3 {
        return;
    }
    let s = &mut shared.settings.mono_font_size;
    let next = ((*s * zoom) * 2.0).round() / 2.0;
    // At least half a point per wheel step, so slow wheels still move it.
    let next = if zoom > 1.0 {
        next.max(*s + 0.5)
    } else {
        next.min(*s - 0.5)
    }
    .clamp(8.0, 32.0);
    if next != *s {
        *s = next;
        shared.actions.push(crate::app::Action::ApplySettings);
    }
}

pub fn open_external(session: &Session, target: &DiffTarget, shared: &Shared) {
    external::open(session, target, shared.settings.external_diff_tool.as_ref());
}

/// An LFS pointer diff: object ids and sizes instead of the pointer text.
fn lfs_info(
    ui: &mut Ui,
    old: Option<guard::LfsPointer>,
    new: Option<guard::LfsPointer>,
    shared: &Shared,
) {
    let p = shared.palette;
    ui.add_space(8.0);
    ui.label(RichText::new("Git LFS object").strong());
    let row = |ui: &mut Ui, label: &str, ptr: &Option<guard::LfsPointer>, color: Color32| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(p.muted));
            match ptr {
                Some(ptr) => {
                    let size = ptr.size.map(human_size).unwrap_or_else(|| "?".into());
                    let short = &ptr.oid[..12.min(ptr.oid.len())];
                    ui.label(
                        RichText::new(format!("LFS object {short} ({size})"))
                            .monospace()
                            .color(color),
                    )
                    .on_hover_text(format!("sha256:{}", ptr.oid));
                }
                None => {
                    ui.label(RichText::new("none").color(p.muted));
                }
            }
        });
    };
    row(ui, "Before:", &old, p.removed_fg);
    row(ui, "After: ", &new, p.added_fg);
    ui.label(
        RichText::new("The file is stored with Git LFS; the repository only tracks this pointer.")
            .small()
            .color(p.muted),
    );
}

pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

pub fn human_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / (K * K))
    } else {
        format!("{:.1} GB", f / (K * K * K))
    }
}

#[cfg(test)]
impl DiffView {
    /// Lines and display rows of the shown diff (for smoke tests).
    pub(crate) fn shown_stats(&self) -> Option<(usize, usize, bool)> {
        self.shown.as_ref().map(|s| {
            (
                s.diff.lines.len(),
                s.layout.rows(self.side_by_side).len(),
                s.diff.binary,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_sizes() {
        assert_eq!(split_path("a/b/c.rs"), ("a/b/", "c.rs"));
        assert_eq!(split_path("c.rs"), ("", "c.rs"));
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    #[test]
    fn job_marks_cut_and_eol() {
        let f = FontId::monospace(12.0);
        let j = line_job(
            "a\tb",
            std::slice::from_ref(&(2..3)),
            &[],
            Color32::RED,
            &f,
            Color32::WHITE,
            Color32::GRAY,
            4,
            true,
        );
        assert!(j.text.starts_with("a   b"));
        assert!(j.text.contains("no newline"));
        // highlighted segment "b" carries the background
        assert!(
            j.sections
                .iter()
                .any(|s| s.format.background == Color32::RED
                    && &j.text[s.byte_range.start.0..s.byte_range.end.0] == "b")
        );
    }

    #[test]
    fn job_combines_tokens_and_word_highlights() {
        let f = FontId::monospace(12.0);
        // "let x = 1": token "let" blue, "1" green; word highlight over "x = 1"
        let syn = [
            Span {
                start: 0,
                end: 3,
                color: Color32::BLUE,
            },
            Span {
                start: 8,
                end: 9,
                color: Color32::GREEN,
            },
        ];
        let j = line_job(
            "let x = 1",
            std::slice::from_ref(&(4..9)),
            &syn,
            Color32::RED,
            &f,
            Color32::WHITE,
            Color32::GRAY,
            4,
            false,
        );
        let parts: Vec<(&str, Color32, Color32)> = j
            .sections
            .iter()
            .map(|s| {
                (
                    &j.text[s.byte_range.start.0..s.byte_range.end.0],
                    s.format.color,
                    s.format.background,
                )
            })
            .collect();
        assert_eq!(
            parts,
            vec![
                ("let", Color32::BLUE, Color32::TRANSPARENT),
                (" ", Color32::WHITE, Color32::TRANSPARENT),
                ("x = ", Color32::WHITE, Color32::RED),
                ("1", Color32::GREEN, Color32::RED),
            ]
        );
    }

    /// The hunk keys taken from a context-3 display diff stage / unstage /
    /// discard exactly that hunk through the core's full-context rebuild.
    #[test]
    fn hunk_actions_on_a_real_repository() {
        use gk_diff::Options;
        let dir = std::env::temp_dir().join(format!("gk-diff-hunks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(o.status.success(), "git {args:?}");
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        let old: String = (0..40).map(|i| format!("{i}\r\n")).collect();
        std::fs::write(dir.join("f.txt"), &old).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let new = old
            .replace("\r\n3\r\n", "\r\nthree\r\n")
            .replace("\r\n30\r\n", "\r\nthirty\r\n");
        std::fs::write(dir.join("f.txt"), &new).unwrap();

        let repo = gk_core::Repo::open(&dir, gk_cli::CommandLog::default()).unwrap();
        let shown = |t: &DiffTarget| {
            let (o, n) = gk_core::content::load_sides(&repo, t).unwrap();
            gk_diff::diff(
                o.as_deref().unwrap_or_default(),
                n.as_deref().unwrap_or_default(),
                Options::default(),
            )
        };
        let unstaged = DiffTarget::Unstaged {
            path: "f.txt".into(),
        };
        let staged = DiffTarget::Staged {
            path: "f.txt".into(),
            old_path: None,
        };
        let d = shown(&unstaged);
        assert_eq!(d.hunks.len(), 2);

        // Stage the second hunk only.
        repo.stage_lines("f.txt", false, &model::picker(&model::hunk_keys(&d, 1)))
            .unwrap();
        let cached = git(&["diff", "--cached"]);
        assert!(
            cached.contains("+thirty") && !cached.contains("+three"),
            "{cached}"
        );

        // Unstage it again via the staged diff.
        let s = shown(&staged);
        assert_eq!(s.hunks.len(), 1);
        repo.stage_lines("f.txt", true, &model::picker(&model::hunk_keys(&s, 0)))
            .unwrap();
        assert!(git(&["diff", "--cached"]).trim().is_empty());

        // Discard the first hunk from the worktree; CRLF stays intact.
        let d = shown(&unstaged);
        repo.discard_lines("f.txt", &model::picker(&model::hunk_keys(&d, 0)))
            .unwrap();
        let now = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        assert!(now.contains("\r\n3\r\n") && now.contains("\r\nthirty\r\n"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
