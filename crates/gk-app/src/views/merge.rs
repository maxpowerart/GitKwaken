//! Built-in three-way merge editor for one conflicted file.
//!
//! Top: "ours" and "theirs" side by side, aligned chunk by chunk, with
//! per-conflict buttons. Bottom: the editable result, which starts as the
//! base with every non-conflicting change applied and git-style conflict
//! markers for the rest. Open it with [`open_merge`].

pub mod diff3;

use crate::app::Shared;
use crate::tab::{RepoTab, View};
use diff3::{ChunkKind, Merge3, Pick, Rendered};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Pos2, Rect, Sense, Ui, UiBuilder, vec2};
use gk_core::{Repo, Snapshot};
use parking_lot::Mutex;
use std::sync::Arc;

const NONE: u32 = u32::MAX;
/// Above this the result editor skips marker highlighting (layout cost).
const HIGHLIGHT_LIMIT: usize = 512 * 1024;

#[derive(Clone, Copy)]
enum Row {
    /// Toolbar row above a conflict chunk.
    Header(u32),
    /// Line indices into ours / theirs, `NONE` for padding.
    Line { chunk: u32, ours: u32, theirs: u32 },
}

struct Loaded {
    m: Merge3,
    /// Write the result with CRLF line endings.
    crlf: bool,
    ours_title: String,
    theirs_title: String,
    /// Labels after `<<<<<<<` / `>>>>>>>`.
    labels: (String, String),
    /// Non-mergeable content (binary, not UTF-8): only whole-file choices.
    unmergeable: Option<String>,
    rows: Vec<Row>,
    /// Chunk indices of conflicts, and the row of each one's header.
    conflicts: Vec<usize>,
    header_rows: Vec<usize>,
    digits: usize,
}

type Slot = Arc<Mutex<Option<Result<Loaded, String>>>>;

pub struct MergeState {
    pub path: String,
    slot: Slot,
    data: Option<Arc<Loaded>>,
    error: Option<String>,
    picks: Vec<Option<Pick>>,
    result: String,
    rendered: Rendered,
    /// The result was edited by hand; conflict buttons then patch the text.
    dirty: bool,
    unresolved: usize,
    current: usize,
    scroll_top: Option<f32>,
    scroll_result: Option<f32>,
    notice: Option<String>,
    confirm_save: bool,
    row_h: f32,
    result_row_h: f32,
}

enum Act {
    Cancel,
    Save(Vec<u8>),
    Whole(gk_core::ops::Side),
}

/// Opens the merge editor for the conflicted `path` (repository-relative,
/// `/`-separated). The index stages and worktree file load in the background.
pub fn open_merge(tab: &mut RepoTab, path: String) {
    let repo = tab.session.repo().clone();
    let head = tab
        .session
        .snapshot()
        .refs
        .head
        .branch()
        .map(str::to_string);
    let slot: Slot = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    let p = path.clone();
    let spawned = std::thread::Builder::new()
        .name("gk-merge-load".into())
        .spawn(move || {
            let r = load(&repo, &p, head).map_err(|e| format!("{e:#}"));
            *s2.lock() = Some(r);
        });
    if let Err(e) = spawned {
        *slot.lock() = Some(Err(e.to_string()));
    }
    tab.merge = Some(Box::new(MergeState {
        path,
        slot,
        data: None,
        error: None,
        picks: Vec::new(),
        result: String::new(),
        rendered: Rendered {
            text: String::new(),
            offsets: Vec::new(),
            lines: Vec::new(),
        },
        dirty: false,
        unresolved: 0,
        current: 0,
        scroll_top: None,
        scroll_result: None,
        notice: None,
        confirm_save: false,
        row_h: 18.0,
        result_row_h: 16.0,
    }));
    tab.view = View::Merge;
}

fn load(repo: &Repo, path: &str, head: Option<String>) -> anyhow::Result<Loaded> {
    let g = repo.gix();
    let stage = |n| gk_core::content::index_blob(&g, path, n);
    let (base, ours, theirs) = (stage(1)?, stage(2)?, stage(3)?);
    if ours.is_none() && theirs.is_none() {
        anyhow::bail!("{path} has no conflict in the index (already resolved?)");
    }
    let work = std::fs::read(repo.abs(path)).ok();
    let crlf = [work.as_deref(), ours.as_deref(), theirs.as_deref()]
        .into_iter()
        .flatten()
        .find(|d| d.contains(&b'\n'))
        .is_some_and(uses_crlf);
    let (base, ours, theirs) = (
        base.unwrap_or_default(),
        ours.unwrap_or_default(),
        theirs.unwrap_or_default(),
    );
    let unmergeable = if [&base, &ours, &theirs]
        .iter()
        .any(|d| gk_diff::is_binary(d))
    {
        Some("Binary file: choose a whole side.".to_string())
    } else if [&base, &ours, &theirs]
        .iter()
        .any(|d| std::str::from_utf8(d).is_err())
    {
        Some("Not UTF-8 text: choose a whole side or use an external merge tool.".to_string())
    } else {
        None
    };
    let m = if unmergeable.is_some() {
        Merge3::default()
    } else {
        Merge3::new(&base, &ours, &theirs)
    };
    let (theirs_name, theirs_kind) = incoming(repo);
    let head = head.or_else(|| {
        repo.git()
            .read(&["symbolic-ref", "--short", "-q", "HEAD"])
            .ok()
            .filter(|h| !h.is_empty())
    });
    let ours_label = head
        .clone()
        .map(|h| format!("HEAD ({h})"))
        .unwrap_or_else(|| "HEAD".into());
    let mut d = Loaded {
        crlf,
        ours_title: format!("Ours · {ours_label}"),
        theirs_title: format!("Theirs · {theirs_name}{theirs_kind}"),
        labels: ("HEAD".into(), theirs_name),
        unmergeable,
        rows: Vec::new(),
        conflicts: Vec::new(),
        header_rows: Vec::new(),
        digits: m
            .ours
            .lines
            .len()
            .max(m.theirs.lines.len())
            .max(1)
            .to_string()
            .len(),
        m,
    };
    build_rows(&mut d);
    Ok(d)
}

fn uses_crlf(d: &[u8]) -> bool {
    let lf = d.iter().filter(|&&b| b == b'\n').count();
    let crlf = d.windows(2).filter(|w| w == b"\r\n").count();
    crlf * 2 > lf
}

/// Name of the commit being merged / picked / rebased, and what is going on.
fn incoming(repo: &Repo) -> (String, &'static str) {
    for (file, what) in [
        ("MERGE_HEAD", ""),
        ("CHERRY_PICK_HEAD", " (cherry-pick)"),
        ("REVERT_HEAD", " (revert)"),
        ("REBASE_HEAD", " (commit being rebased)"),
    ] {
        let Ok(s) = std::fs::read_to_string(repo.git_dir().join(file)) else {
            continue;
        };
        let id = s.lines().next().unwrap_or_default().trim().to_string();
        if id.is_empty() {
            continue;
        }
        let name = repo
            .git()
            .read(&[
                "name-rev",
                "--name-only",
                "--no-undefined",
                "--exclude=refs/tags/*",
                &id,
            ])
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| crate::util::short_id(&id).to_string());
        return (name, what);
    }
    ("incoming".into(), "")
}

fn build_rows(d: &mut Loaded) {
    for (ci, c) in d.m.chunks.iter().enumerate() {
        if c.kind == ChunkKind::Conflict {
            d.conflicts.push(ci);
            d.header_rows.push(d.rows.len());
            d.rows.push(Row::Header(ci as u32));
        }
        let n = c.ours.len().max(c.theirs.len());
        for k in 0..n {
            let at = |r: &std::ops::Range<usize>| {
                if k < r.len() {
                    (r.start + k) as u32
                } else {
                    NONE
                }
            };
            d.rows.push(Row::Line {
                chunk: ci as u32,
                ours: at(&c.ours),
                theirs: at(&c.theirs),
            });
        }
    }
}

impl MergeState {
    fn poll(&mut self) {
        if self.data.is_some() || self.error.is_some() {
            return;
        }
        let Some(r) = self.slot.lock().take() else {
            return;
        };
        match r {
            Ok(d) => {
                self.picks = vec![None; d.m.chunks.len()];
                self.data = Some(Arc::new(d));
                self.rerender();
                if !self.data.as_ref().is_some_and(|d| d.conflicts.is_empty()) {
                    self.goto(0);
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn labels(d: &Loaded) -> (&str, &str) {
        (&d.labels.0, &d.labels.1)
    }

    fn rerender(&mut self) {
        let Some(d) = self.data.clone() else { return };
        self.rendered = d.m.render(&self.picks, Self::labels(&d));
        self.result = self.rendered.text.clone();
        self.dirty = false;
        self.notice = None;
        self.recount();
    }

    fn recount(&mut self) {
        self.unresolved = self
            .result
            .lines()
            .filter(|l| l.starts_with("<<<<<<<"))
            .count();
    }

    fn goto(&mut self, k: usize) {
        let Some(d) = &self.data else { return };
        if d.conflicts.is_empty() {
            return;
        }
        self.current = k.min(d.conflicts.len() - 1);
        let row = d.header_rows[self.current];
        self.scroll_top = Some((row as f32 - 3.0).max(0.0) * self.row_h);
        let line = self
            .rendered
            .lines
            .get(d.conflicts[self.current])
            .copied()
            .unwrap_or(0);
        self.scroll_result = Some((line as f32 - 3.0).max(0.0) * self.result_row_h);
    }

    fn set_pick(&mut self, chunk: usize, pick: Option<Pick>) {
        let Some(d) = self.data.clone() else { return };
        let old = self.picks[chunk];
        if old == pick {
            return;
        }
        if !self.dirty {
            self.picks[chunk] = pick;
            self.rerender();
            return;
        }
        // Hand-edited result: replace this conflict's current text in place.
        let labels = Self::labels(&d);
        let old_text = d.m.chunk_text(chunk, old, labels);
        let new_text = d.m.chunk_text(chunk, pick, labels);
        let expected = self.rendered.offsets.get(chunk).copied().unwrap_or(0);
        match locate(&self.result, &old_text, expected) {
            Some((range, trimmed)) => {
                let new_text = if trimmed {
                    new_text.strip_suffix('\n').unwrap_or(&new_text)
                } else {
                    &new_text
                };
                self.result.replace_range(range, new_text);
                self.picks[chunk] = pick;
                self.rendered = d.m.render(&self.picks, labels);
                self.notice = None;
                self.recount();
            }
            None => {
                self.notice = Some(
                    "This conflict was edited by hand — edit the result directly, or Reset result."
                        .into(),
                );
            }
        }
    }

    fn output(&self, crlf: bool) -> Vec<u8> {
        let t = self.result.replace("\r\n", "\n");
        if crlf {
            t.replace('\n', "\r\n").into_bytes()
        } else {
            t.into_bytes()
        }
    }
}

/// Finds `needle` in `text` nearest to byte `expected`. A needle at the very
/// end may lack its final newline (file without one); `.1` reports that.
fn locate(text: &str, needle: &str, expected: usize) -> Option<(std::ops::Range<usize>, bool)> {
    if needle.is_empty() {
        return None;
    }
    let best = text
        .match_indices(needle)
        .map(|(i, _)| i)
        .min_by_key(|&i| i.abs_diff(expected));
    if let Some(i) = best {
        return Some((i..i + needle.len(), false));
    }
    let trimmed = needle.strip_suffix('\n')?;
    (!trimmed.is_empty() && text.ends_with(trimmed))
        .then(|| (text.len() - trimmed.len()..text.len(), true))
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, _snap: &Snapshot, shared: &mut Shared) {
    let Some(st) = tab.merge.as_deref_mut() else {
        tab.view = View::FileStatus;
        return;
    };
    st.poll();
    let p = shared.palette;
    let mut act: Option<Act> = None;
    let id = egui::Id::new(("merge", &tab.path));

    egui::Panel::top(id.with("head")).show(ui, |ui| header(ui, st, &mut act, p));

    if let Some(e) = &st.error {
        ui.add_space(12.0);
        ui.colored_label(p.removed_fg, e);
    } else if let Some(d) = st.data.clone() {
        if let Some(msg) = &d.unmergeable {
            ui.add_space(12.0);
            ui.label(msg);
        } else {
            egui::Panel::bottom(id.with("result"))
                .resizable(true)
                .default_size((ui.available_height() * 0.45).max(120.0))
                .size_range(80.0..=(ui.available_height() - 80.0).max(120.0))
                .show(ui, |ui| result_editor(ui, st, &d, p));
            egui::CentralPanel::default().show(ui, |ui| panes(ui, st, &d, p));
        }
    } else {
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Loading conflict…");
        });
    }

    let path = st.path.clone();
    match act {
        Some(Act::Cancel) => {
            tab.merge = None;
            tab.view = View::FileStatus;
        }
        Some(Act::Save(bytes)) => {
            tab.session
                .run_index_op(format!("Resolve {path}"), move |r| {
                    r.save_resolution(&path, &bytes)
                });
            tab.merge = None;
            tab.view = View::FileStatus;
        }
        Some(Act::Whole(side)) => {
            tab.session
                .run_index_op(format!("Resolve {path}"), move |r| {
                    r.resolve_with(&[path], side)
                });
            tab.merge = None;
            tab.view = View::FileStatus;
        }
        None => {}
    }
}

fn header(ui: &mut Ui, st: &mut MergeState, act: &mut Option<Act>, p: crate::theme::Palette) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Merge").strong());
        ui.label(egui::RichText::new(&st.path).strong());
        ui.separator();
        let total = st.data.as_ref().map_or(0, |d| d.conflicts.len());
        let mergeable = st.data.as_ref().is_some_and(|d| d.unmergeable.is_none());
        if mergeable {
            if total > 0 {
                ui.label(format!("Conflict {} of {total}", st.current + 1));
            }
            let color = if st.unresolved > 0 {
                p.conflict
            } else {
                p.added_fg
            };
            ui.label(
                egui::RichText::new(format!("{} unresolved", st.unresolved))
                    .color(color)
                    .strong(),
            );
            if ui
                .add_enabled(st.current > 0, egui::Button::new("◀ Previous"))
                .clicked()
            {
                st.goto(st.current - 1);
            }
            if ui
                .add_enabled(st.current + 1 < total, egui::Button::new("Next ▶"))
                .clicked()
            {
                st.goto(st.current + 1);
            }
            ui.separator();
        }
        let loaded = st.data.is_some();
        if ui
            .add_enabled(loaded, egui::Button::new("Use whole file: ours"))
            .clicked()
        {
            *act = Some(Act::Whole(gk_core::ops::Side::Ours));
        }
        if ui
            .add_enabled(loaded, egui::Button::new("Use whole file: theirs"))
            .clicked()
        {
            *act = Some(Act::Whole(gk_core::ops::Side::Theirs));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Cancel")
                .on_hover_text("Back to File status without saving")
                .clicked()
            {
                *act = Some(Act::Cancel);
            }
            if mergeable {
                let save = ui.add(egui::Button::new(
                    egui::RichText::new("Save & mark resolved").strong(),
                ));
                if save.clicked() {
                    if st.unresolved > 0 {
                        st.confirm_save = true;
                    } else if let Some(d) = &st.data {
                        *act = Some(Act::Save(st.output(d.crlf)));
                    }
                }
            }
        });
    });
    if st.confirm_save {
        ui.horizontal(|ui| {
            ui.colored_label(
                p.conflict,
                format!(
                    "The result still contains {} conflict marker(s).",
                    st.unresolved
                ),
            );
            if ui.button("Save anyway").clicked() {
                if let Some(d) = &st.data {
                    *act = Some(Act::Save(st.output(d.crlf)));
                }
                st.confirm_save = false;
            }
            if ui.button("Keep editing").clicked() {
                st.confirm_save = false;
            }
        });
    }
    if let Some(n) = &st.notice {
        ui.colored_label(p.modified, n);
    }
    ui.add_space(2.0);
}

fn chunk_colors(
    st: &MergeState,
    d: &Loaded,
    chunk: usize,
    p: &crate::theme::Palette,
) -> (Option<Color32>, Option<Color32>) {
    let c = &d.m.chunks[chunk];
    let unresolved =
        Color32::from_rgba_unmultiplied(p.conflict.r(), p.conflict.g(), p.conflict.b(), 45);
    match c.kind {
        ChunkKind::Stable => (None, None),
        ChunkKind::Ours => (Some(p.added_bg), None),
        ChunkKind::Theirs => (None, Some(p.added_bg)),
        ChunkKind::Same => (Some(p.hunk_bg), Some(p.hunk_bg)),
        ChunkKind::Conflict => match st.picks[chunk] {
            None => (Some(unresolved), Some(unresolved)),
            Some(Pick::Ours) => (Some(p.added_bg), Some(p.removed_bg)),
            Some(Pick::Theirs) => (Some(p.removed_bg), Some(p.added_bg)),
            Some(_) => (Some(p.added_bg), Some(p.added_bg)),
        },
    }
}

fn panes(ui: &mut Ui, st: &mut MergeState, d: &Loaded, p: crate::theme::Palette) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let (font_h, char_w) = ui
        .ctx()
        .fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
    let small = egui::TextStyle::Small.resolve(ui.style());
    let row_h = (font_h + 6.0).round().max(20.0);
    st.row_h = row_h;
    let text_color = ui.visuals().text_color();
    let faint = ui.visuals().faint_bg_color;
    let num_w = d.digits as f32 * char_w + 14.0;

    // Column titles.
    let (title, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
    let half = title.width() / 2.0;
    for (i, t) in [&d.ours_title, &d.theirs_title].into_iter().enumerate() {
        let r = Rect::from_min_size(title.min + vec2(half * i as f32, 0.0), vec2(half, row_h));
        ui.painter().rect_filled(r.shrink(1.0), 2.0, faint);
        ui.painter().with_clip_rect(r).text(
            r.left_center() + vec2(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            t,
            egui::TextStyle::Body.resolve(ui.style()),
            text_color,
        );
    }

    let mut sa = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("gk-merge-panes");
    if let Some(y) = st.scroll_top.take() {
        sa = sa.vertical_scroll_offset(y);
    }
    let mut picked: Option<(usize, Option<Pick>)> = None;
    let mut goto: Option<usize> = None;
    sa.show_viewport(ui, |ui, vp| {
        let rows = &d.rows;
        ui.set_min_height(rows.len() as f32 * row_h);
        let origin = ui.max_rect().min;
        let w = ui.max_rect().width();
        let half = w / 2.0;
        let first = ((vp.min.y / row_h).floor().max(0.0) as usize).min(rows.len());
        let last = (((vp.max.y / row_h).ceil() as usize) + 1).min(rows.len());
        let painter = ui.painter().clone();
        let cur_chunk = d.conflicts.get(st.current).copied();
        for (ri, row) in rows.iter().enumerate().take(last).skip(first) {
            let rect = Rect::from_min_size(
                Pos2::new(origin.x, origin.y + ri as f32 * row_h),
                vec2(w, row_h),
            );
            match *row {
                Row::Header(ci) => {
                    let ci = ci as usize;
                    let k = d.conflicts.iter().position(|&c| c == ci).unwrap_or(0);
                    painter.rect_filled(rect, 0.0, p.hunk_bg);
                    if cur_chunk == Some(ci) {
                        painter.rect_filled(
                            Rect::from_min_size(rect.min, vec2(3.0, row_h)),
                            0.0,
                            p.branch,
                        );
                    }
                    let pick = st.picks[ci];
                    let mut child = ui.new_child(
                        UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(8.0, 1.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let state = match pick {
                        None => egui::RichText::new("unresolved").color(p.conflict),
                        Some(_) => egui::RichText::new("resolved").color(p.added_fg),
                    };
                    if child
                        .add(
                            egui::Label::new(
                                egui::RichText::new(format!("Conflict {}", k + 1)).strong(),
                            )
                            .sense(Sense::click()),
                        )
                        .clicked()
                    {
                        goto = Some(k);
                    }
                    child.label(state.font(small.clone()));
                    child.add_space(8.0);
                    for (label, pk) in [
                        ("Use ours", Pick::Ours),
                        ("Use theirs", Pick::Theirs),
                        ("Use both (ours first)", Pick::OursThenTheirs),
                        ("Use both (theirs first)", Pick::TheirsThenOurs),
                    ] {
                        let on = pick == Some(pk);
                        let b = egui::Button::new(egui::RichText::new(label).font(small.clone()))
                            .selected(on);
                        let r =
                            child
                                .add(b)
                                .on_hover_text(if on { "Click again to undo" } else { "" });
                        if r.clicked() {
                            picked = Some((ci, if on { None } else { Some(pk) }));
                            goto = Some(k);
                        }
                    }
                }
                Row::Line {
                    chunk,
                    ours,
                    theirs,
                } => {
                    let chunk = chunk as usize;
                    let (lc, rc) = chunk_colors(st, d, chunk, &p);
                    for (side, (line, color)) in [(ours, lc), (theirs, rc)].into_iter().enumerate()
                    {
                        let r = Rect::from_min_size(
                            rect.min + vec2(half * side as f32, 0.0),
                            vec2(half, row_h),
                        );
                        let text = if side == 0 { &d.m.ours } else { &d.m.theirs };
                        if line == NONE {
                            if color.is_some() {
                                painter.rect_filled(r, 0.0, faint);
                            }
                            continue;
                        }
                        if let Some(c) = color {
                            painter.rect_filled(r, 0.0, c);
                        }
                        let clip = painter.with_clip_rect(r.intersect(painter.clip_rect()));
                        clip.text(
                            Pos2::new(r.min.x + num_w - 6.0, r.center().y),
                            egui::Align2::RIGHT_CENTER,
                            (line + 1).to_string(),
                            font.clone(),
                            p.muted,
                        );
                        let s = &text.lines[line as usize];
                        let shown: std::borrow::Cow<str> = if s.contains('\t') {
                            s.replace('\t', "    ").into()
                        } else {
                            s.as_str().into()
                        };
                        clip.text(
                            Pos2::new(r.min.x + num_w + 4.0, r.center().y),
                            egui::Align2::LEFT_CENTER,
                            shown,
                            font.clone(),
                            text_color,
                        );
                    }
                    if cur_chunk == Some(chunk) {
                        painter.rect_filled(
                            Rect::from_min_size(rect.min, vec2(3.0, row_h)),
                            0.0,
                            p.branch,
                        );
                    }
                }
            }
        }
        let x = origin.x + half;
        painter.line_segment(
            [
                Pos2::new(x, vp.min.y + origin.y),
                Pos2::new(x, vp.max.y + origin.y),
            ],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    });
    if let Some((ci, pk)) = picked {
        st.set_pick(ci, pk);
    }
    if let Some(k) = goto {
        // Keep the panes where they are; only follow in the result.
        st.current = k;
        let line = st.rendered.lines.get(d.conflicts[k]).copied().unwrap_or(0);
        st.scroll_result = Some((line as f32 - 3.0).max(0.0) * st.result_row_h);
    }
}

fn result_editor(ui: &mut Ui, st: &mut MergeState, d: &Loaded, p: crate::theme::Palette) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    st.result_row_h = ui.ctx().fonts_mut(|f| f.row_height(&font));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Result").strong());
        let hint = if d.crlf {
            "editable · saved with CRLF line endings"
        } else {
            "editable · saved with LF line endings"
        };
        ui.label(egui::RichText::new(hint).color(p.muted));
        if st.dirty {
            ui.label(egui::RichText::new("edited").color(p.modified));
        }
        if ui
            .add_enabled(st.dirty, egui::Button::new("Reset result"))
            .on_hover_text("Discard hand edits")
            .clicked()
        {
            st.rerender();
        }
    });
    let mut sa = egui::ScrollArea::both()
        .auto_shrink([false, false])
        .id_salt("gk-merge-result");
    if let Some(y) = st.scroll_result.take() {
        sa = sa.vertical_scroll_offset(y);
    }
    let colors = MarkerColors {
        text: ui.visuals().text_color(),
        marker: p.conflict,
        ours: p.hunk_bg,
        theirs: Color32::from_rgba_unmultiplied(p.conflict.r(), p.conflict.g(), p.conflict.b(), 30),
    };
    let fill_h = (ui.available_height() - 12.0).max(0.0);
    let fill_rows = ((fill_h / st.result_row_h.max(1.0)).floor() as usize).max(8);
    sa.show(ui, |ui| {
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = if buf.as_str().len() <= HIGHLIGHT_LIMIT {
                marker_job(buf.as_str(), &font, &colors)
            } else {
                LayoutJob::simple(
                    buf.as_str().to_string(),
                    font.clone(),
                    colors.text,
                    f32::INFINITY,
                )
            };
            job.wrap.max_width = wrap;
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };
        let r = ui.add(
            egui::TextEdit::multiline(&mut st.result)
                .code_editor()
                .desired_width(f32::INFINITY)
                // Fill the pane (clicks below the text still focus it); TextEdit
                // ignores a minimum height, so ask for enough rows.
                .desired_rows(fill_rows)
                .id_salt("gk-merge-result-text")
                .layouter(&mut layouter),
        );
        if r.changed() {
            st.dirty = true;
            st.confirm_save = false;
            st.recount();
        }
    });
}

struct MarkerColors {
    text: Color32,
    marker: Color32,
    ours: Color32,
    theirs: Color32,
}

/// Colors conflict marker lines and tints the two sides between them.
fn marker_job(text: &str, font: &FontId, c: &MarkerColors) -> LayoutJob {
    let mut job = LayoutJob {
        break_on_newline: true,
        ..Default::default()
    };
    #[derive(PartialEq, Clone, Copy)]
    enum St {
        Plain,
        Ours,
        Theirs,
        Marker,
    }
    let fmt = |s: St| TextFormat {
        font_id: font.clone(),
        color: if s == St::Marker { c.marker } else { c.text },
        background: match s {
            St::Ours => c.ours,
            St::Theirs => c.theirs,
            _ => Color32::TRANSPARENT,
        },
        ..Default::default()
    };
    let mut start = 0usize;
    let mut cur = St::Plain;
    let mut inside = St::Plain;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let (kind, next) = if line.starts_with("<<<<<<<") {
            (St::Marker, St::Ours)
        } else if line.starts_with("=======") && inside != St::Plain {
            (St::Marker, St::Theirs)
        } else if line.starts_with(">>>>>>>") && inside != St::Plain {
            (St::Marker, St::Plain)
        } else {
            (inside, inside)
        };
        if kind != cur {
            if pos > start {
                job.append(&text[start..pos], 0.0, fmt(cur));
            }
            start = pos;
            cur = kind;
        }
        inside = next;
        pos += line.len();
    }
    if pos > start || job.sections.is_empty() {
        job.append(&text[start..pos], 0.0, fmt(cur));
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_prefers_nearest_and_handles_missing_eol() {
        let t = "x\nA\ny\nA\n";
        assert_eq!(locate(t, "A\n", 0), Some((2..4, false)));
        assert_eq!(locate(t, "A\n", 7), Some((6..8, false)));
        assert_eq!(locate("a\nB", "B\n", 2), Some((2..3, true)));
        assert_eq!(locate("abc", "", 0), None);
        assert_eq!(locate("abc", "z\n", 0), None);
    }

    #[test]
    fn crlf_detection() {
        assert!(uses_crlf(b"a\r\nb\r\n"));
        assert!(!uses_crlf(b"a\nb\n"));
        assert!(!uses_crlf(b"a\nb\nc\r\n"));
    }

    #[test]
    fn marker_job_sections_cover_text() {
        let font = FontId::monospace(12.0);
        let c = MarkerColors {
            text: Color32::WHITE,
            marker: Color32::RED,
            ours: Color32::BLUE,
            theirs: Color32::GREEN,
        };
        let t = "a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\nz";
        let j = marker_job(t, &font, &c);
        assert_eq!(j.sections.first().unwrap().byte_range.start.0, 0);
        assert_eq!(j.sections.last().unwrap().byte_range.end.0, t.len());
        for w in j.sections.windows(2) {
            assert_eq!(w[0].byte_range.end, w[1].byte_range.start);
        }
        assert_eq!(j.sections.len(), 7);
        assert!(marker_job("", &font, &c).sections.len() == 1);
    }
}
