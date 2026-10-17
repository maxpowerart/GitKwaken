//! Blame view: who last changed each line of a file. Open it with
//! [`open_blame`]; `git blame` runs on a background thread.

use crate::app::Shared;
use crate::tab::{RepoTab, View};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Pos2, Rect, Sense, Ui, vec2};
use gk_core::Snapshot;
use gk_core::advanced::Blame;
use parking_lot::Mutex;
use std::sync::Arc;

const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

struct Prepared {
    blame: Blame,
    /// Per line: first line of a run of lines from the same commit.
    run_start: Vec<bool>,
    /// Per line: run parity, for alternating backgrounds.
    odd_run: Vec<bool>,
    /// Per commit: 0 (oldest) ..= 1 (newest).
    heat: Vec<f32>,
    digits: usize,
    max_cols: usize,
}

type Slot = Arc<Mutex<Option<Result<Prepared, String>>>>;

pub struct BlameState {
    pub path: String,
    pub rev: Option<String>,
    /// View to return to.
    back: View,
    /// Earlier (path, rev) pairs, for "Back" after "Blame previous revision".
    stack: Vec<(String, Option<String>)>,
    slot: Slot,
    data: Option<Arc<Prepared>>,
    error: Option<String>,
    query: String,
    matches: Vec<usize>,
    current: usize,
    focus_search: bool,
    scroll_to: Option<usize>,
    selected: Option<usize>,
}

enum Act {
    Back,
    Jump(String),
    Previous(String, String),
}

/// Opens the blame of `path` (repository-relative) at `rev` (`None`: the
/// working copy).
pub fn open_blame(tab: &mut RepoTab, path: String, rev: Option<String>) {
    let (back, mut stack) = match (tab.view, tab.blame.take()) {
        (View::Blame, Some(b)) => (b.back, b.stack),
        (View::Blame, None) => (View::History, Vec::new()),
        (v, _) => (v, Vec::new()),
    };
    if tab.view != View::Blame {
        stack.clear();
    }
    let slot = spawn_load(tab, &path, rev.clone());
    tab.blame = Some(Box::new(BlameState {
        path,
        rev,
        back,
        stack,
        slot,
        data: None,
        error: None,
        query: String::new(),
        matches: Vec::new(),
        current: 0,
        focus_search: false,
        scroll_to: None,
        selected: None,
    }));
    tab.view = View::Blame;
}

fn spawn_load(tab: &RepoTab, path: &str, rev: Option<String>) -> Slot {
    let repo = tab.session.repo().clone();
    let slot: Slot = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    let p = path.to_string();
    let spawned = std::thread::Builder::new().name("gk-blame".into()).spawn(move || {
        let r = repo.blame(&p, rev.as_deref()).map(prepare).map_err(|e| format!("{e:#}"));
        *s2.lock() = Some(r);
    });
    if let Err(e) = spawned {
        *slot.lock() = Some(Err(e.to_string()));
    }
    slot
}

fn prepare(blame: Blame) -> Prepared {
    let n = blame.lines.len();
    let mut run_start = Vec::with_capacity(n);
    let mut odd_run = Vec::with_capacity(n);
    let mut odd = true;
    for (i, l) in blame.lines.iter().enumerate() {
        let start = i == 0 || blame.lines[i - 1].commit != l.commit;
        if start {
            odd = !odd;
        }
        run_start.push(start);
        odd_run.push(odd);
    }
    let times = blame.commits.iter().filter(|c| c.id != UNCOMMITTED).map(|c| c.author_time);
    let (lo, hi) = times.fold((i64::MAX, i64::MIN), |(a, b), t| (a.min(t), b.max(t)));
    let heat = blame
        .commits
        .iter()
        .map(|c| {
            if c.id == UNCOMMITTED || hi <= lo {
                1.0
            } else {
                (c.author_time - lo) as f32 / (hi - lo) as f32
            }
        })
        .collect();
    let max_cols = blame.text.iter().map(|t| t.chars().count() + 3 * t.matches('\t').count()).max().unwrap_or(0);
    Prepared { digits: n.max(1).to_string().len(), run_start, odd_run, heat, max_cols, blame }
}

/// ASCII-case-insensitive substring search; byte offsets match `hay`.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (from..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

impl BlameState {
    fn poll(&mut self) {
        if self.data.is_some() || self.error.is_some() {
            return;
        }
        let Some(r) = self.slot.lock().take() else { return };
        match r {
            Ok(d) => {
                self.data = Some(Arc::new(d));
                self.update_matches();
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn update_matches(&mut self) {
        self.matches.clear();
        self.current = 0;
        let Some(d) = &self.data else { return };
        if self.query.is_empty() {
            return;
        }
        self.matches = d
            .blame
            .text
            .iter()
            .enumerate()
            .filter(|(_, t)| find_ci(t, &self.query, 0).is_some())
            .map(|(i, _)| i)
            .collect();
        // Start at the first match below the selection, if any.
        let from = self.selected.unwrap_or(0);
        self.current = self.matches.iter().position(|&l| l >= from).unwrap_or(0);
        if let Some(&l) = self.matches.get(self.current) {
            self.scroll_to = Some(l);
        }
    }

    fn step(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.current = (self.current as isize + delta).rem_euclid(n) as usize;
        self.scroll_to = Some(self.matches[self.current]);
    }
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, _snap: &Snapshot, shared: &mut Shared) {
    let Some(st) = tab.blame.as_deref_mut() else {
        tab.view = View::History;
        return;
    };
    st.poll();
    let p = shared.palette;
    let relative = shared.settings.history.relative_dates;
    let mut act: Option<Act> = None;

    if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
        st.focus_search = true;
    }

    egui::Panel::top(egui::Id::new(("blame-head", &tab.path))).show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let back_hint = if st.stack.is_empty() { "Back to the previous view" } else { "Back to the previous blame" };
            if ui.button("◀ Back").on_hover_text(back_hint).clicked() {
                act = Some(Act::Back);
            }
            ui.label(egui::RichText::new("Blame").strong());
            ui.label(egui::RichText::new(&st.path).monospace());
            let rev = st.rev.as_deref().map(|r| if r.len() == 40 { crate::util::short_id(r) } else { r });
            ui.label(egui::RichText::new(format!("@ {}", rev.unwrap_or("working copy"))).color(p.muted));
            if let Some(d) = &st.data {
                ui.label(
                    egui::RichText::new(format!("{} lines · {} commits", d.blame.lines.len(), d.blame.commits.len()))
                        .color(p.muted),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let total = st.matches.len();
                if ui.add_enabled(total > 0, egui::Button::new("▼")).on_hover_text("Next match (Enter)").clicked() {
                    st.step(1);
                }
                if ui.add_enabled(total > 0, egui::Button::new("▲")).on_hover_text("Previous match (Shift+Enter)").clicked() {
                    st.step(-1);
                }
                if !st.query.is_empty() {
                    let t = if total == 0 { "no matches".to_string() } else { format!("{} of {total}", st.current + 1) };
                    ui.label(egui::RichText::new(t).color(p.muted));
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut st.query).hint_text("Find (Ctrl+F)").desired_width(200.0),
                );
                if st.focus_search {
                    r.request_focus();
                    st.focus_search = false;
                }
                if r.changed() {
                    st.update_matches();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let back = ui.input(|i| i.modifiers.shift);
                    st.step(if back { -1 } else { 1 });
                    r.request_focus();
                }
                if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    st.query.clear();
                    st.update_matches();
                }
            });
        });
        ui.add_space(2.0);
    });

    if let Some(e) = &st.error {
        ui.add_space(12.0);
        ui.colored_label(p.removed_fg, e);
    } else if let Some(d) = st.data.clone() {
        body(ui, st, &d, p, relative, &mut act);
    } else {
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Running git blame…");
        });
    }

    match act {
        Some(Act::Back) => {
            if let Some((path, rev)) = st.stack.pop() {
                let stack = std::mem::take(&mut st.stack);
                open_blame(tab, path, rev);
                if let Some(b) = tab.blame.as_deref_mut() {
                    b.stack = stack;
                }
            } else {
                let back = st.back;
                tab.blame = None;
                tab.view = back;
            }
        }
        Some(Act::Jump(id)) => {
            tab.sidebar.jump_to = Some(id);
            tab.view = View::History;
        }
        Some(Act::Previous(id, path)) => {
            let here = (st.path.clone(), st.rev.clone());
            let mut stack = std::mem::take(&mut st.stack);
            stack.push(here);
            open_blame(tab, path, Some(id));
            if let Some(b) = tab.blame.as_deref_mut() {
                b.stack = stack;
            }
        }
        None => {}
    }
}

fn heat_color(p: &crate::theme::Palette, heat: f32) -> Color32 {
    // Old: dim gray; new: bright orange.
    let (lo, hi) = if p.dark {
        (Color32::from_rgb(0x45, 0x48, 0x50), Color32::from_rgb(0xFF, 0x9A, 0x3C))
    } else {
        (Color32::from_rgb(0xD8, 0xD8, 0xDC), Color32::from_rgb(0xF0, 0x7A, 0x1A))
    };
    let t = heat.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(mix(lo.r(), hi.r()), mix(lo.g(), hi.g()), mix(lo.b(), hi.b()))
}

fn body(
    ui: &mut Ui,
    st: &mut BlameState,
    d: &Prepared,
    p: crate::theme::Palette,
    relative: bool,
    act: &mut Option<Act>,
) {
    let b = &d.blame;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let (font_h, char_w) = ui.ctx().fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
    let row_h = (font_h + 4.0).round().max(16.0);
    let text_color = ui.visuals().text_color();
    let faint = ui.visuals().faint_bg_color;
    let date_w = if relative { 14.0 } else { 10.0 } * char_w;
    let id_w = 8.0 * char_w + 10.0;
    let author_w = 16.0 * char_w;
    let gutter_w = 6.0 + id_w + author_w + 8.0 + date_w + 10.0;
    let num_w = d.digits as f32 * char_w + 16.0;
    let code_x = gutter_w + num_w + 6.0;
    let content_w = code_x + d.max_cols as f32 * char_w + 40.0;
    let match_bg = Color32::from_rgba_unmultiplied(0xE8, 0xC1, 0x3D, 50);
    let match_cur = Color32::from_rgba_unmultiplied(0xE8, 0xC1, 0x3D, 120);
    let current_line = st.matches.get(st.current).copied();
    let is_match = |i: usize| st.matches.binary_search(&i).is_ok();

    let mut sa = egui::ScrollArea::both().auto_shrink([false, false]).id_salt("gk-blame-scroll");
    if let Some(line) = st.scroll_to.take() {
        let view_h = ui.available_height();
        sa = sa.vertical_scroll_offset((line as f32 * row_h - view_h / 2.0).max(0.0));
        st.selected = Some(line);
    }
    let query = st.query.clone();
    let mut clicked_line: Option<usize> = None;
    sa.show_viewport(ui, |ui, vp| {
        let n = b.lines.len();
        ui.set_min_width(content_w.max(vp.width()));
        ui.set_min_height(n as f32 * row_h);
        let origin = ui.max_rect().min;
        let full_w = ui.max_rect().width().max(vp.width());
        let first = ((vp.min.y / row_h).floor().max(0.0) as usize).min(n);
        let last = (((vp.max.y / row_h).ceil() as usize) + 1).min(n);
        let painter = ui.painter().clone();
        // The gutter sticks to the left edge while scrolling horizontally.
        let gx = origin.x + vp.min.x;
        for i in first..last {
            let y = origin.y + i as f32 * row_h;
            let line = &b.lines[i];
            let c = &b.commits[line.commit];
            let row = Rect::from_min_size(Pos2::new(origin.x, y), vec2(full_w, row_h));
            if d.odd_run[i] {
                painter.rect_filled(row, 0.0, faint);
            }
            if st.selected == Some(i) {
                painter.rect_filled(row, 0.0, p.selection);
            }
            // Code.
            let code_pos = Pos2::new(gx + code_x, y + row_h / 2.0);
            let raw = &b.text[i];
            let shown: std::borrow::Cow<str> =
                if raw.contains('\t') { raw.replace('\t', "    ").into() } else { raw.as_str().into() };
            if Some(i) == current_line || is_match(i) {
                let bg = if Some(i) == current_line { match_cur } else { match_bg };
                let job = highlight_job(&shown, &query, &font, text_color, bg);
                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                painter.galley(Pos2::new(code_pos.x - vp.min.x, y + (row_h - galley.size().y) / 2.0), galley, text_color);
            } else {
                painter.text(
                    Pos2::new(code_pos.x - vp.min.x, code_pos.y),
                    egui::Align2::LEFT_CENTER,
                    shown,
                    font.clone(),
                    text_color,
                );
            }
            // Gutter (painted over the code when scrolled horizontally).
            let gutter = Rect::from_min_size(Pos2::new(gx, y), vec2(gutter_w + num_w, row_h));
            let gutter_bg = if d.odd_run[i] { faint } else { ui.visuals().panel_fill };
            painter.rect_filled(gutter, 0.0, gutter_bg);
            if st.selected == Some(i) {
                painter.rect_filled(gutter, 0.0, p.selection);
            }
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(gx, y), vec2(4.0, row_h)),
                0.0,
                heat_color(&p, d.heat[line.commit]),
            );
            let uncommitted = c.id == UNCOMMITTED;
            if d.run_start[i] {
                let clip = painter.with_clip_rect(gutter.intersect(painter.clip_rect()));
                let mid = y + row_h / 2.0;
                let (id_text, id_color) = if uncommitted {
                    ("--------".to_string(), p.muted)
                } else {
                    (crate::util::short_id(&c.id).to_string(), p.branch)
                };
                clip.text(Pos2::new(gx + 10.0, mid), egui::Align2::LEFT_CENTER, id_text, font.clone(), id_color);
                let author = if uncommitted { "Not committed yet" } else { c.author.as_str() };
                let ar = Rect::from_min_size(Pos2::new(gx + 10.0 + id_w, y), vec2(author_w, row_h));
                painter.with_clip_rect(ar.intersect(painter.clip_rect())).text(
                    ar.left_center(),
                    egui::Align2::LEFT_CENTER,
                    author,
                    small.clone(),
                    text_color,
                );
                if !uncommitted {
                    let date = crate::util::format_time(c.author_time, 0, relative);
                    let date = if relative { date } else { date.chars().take(10).collect() };
                    clip.text(
                        Pos2::new(gx + 10.0 + id_w + author_w + 8.0, mid),
                        egui::Align2::LEFT_CENTER,
                        date,
                        small.clone(),
                        p.muted,
                    );
                }
            }
            // Line number.
            painter.text(
                Pos2::new(gx + gutter_w + num_w - 8.0, y + row_h / 2.0),
                egui::Align2::RIGHT_CENTER,
                (i + 1).to_string(),
                font.clone(),
                p.muted,
            );

            // Gutter interaction: tooltip, click, context menu.
            let gr = Rect::from_min_size(Pos2::new(gx, y), vec2(gutter_w, row_h));
            let resp = ui.interact(gr, ui.id().with(("blame-gutter", i)), Sense::click());
            if resp.hovered() && !uncommitted {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let resp = resp.on_hover_ui(|ui| {
                if uncommitted {
                    ui.label("Not committed yet");
                    return;
                }
                ui.label(egui::RichText::new(&c.summary).strong());
                ui.label(format!("{} · {}", c.author, crate::util::format_time(c.author_time, 0, false)));
                ui.label(egui::RichText::new(&c.id).monospace().color(p.muted));
                if let Some((_, prev_path)) = &c.previous
                    && prev_path != &st.path
                {
                    ui.label(egui::RichText::new(format!("previously {prev_path}")).color(p.muted));
                }
                ui.label(egui::RichText::new("Click to show in History").color(p.muted));
            });
            if resp.clicked() {
                clicked_line = Some(i);
                if !uncommitted {
                    *act = Some(Act::Jump(c.id.clone()));
                }
            }
            resp.context_menu(|ui| {
                let prev = c.previous.clone();
                if ui
                    .add_enabled(prev.is_some(), egui::Button::new("Blame previous revision"))
                    .on_disabled_hover_text("This commit added the lines")
                    .clicked()
                {
                    if let Some((id, path)) = prev {
                        *act = Some(Act::Previous(id, path));
                    }
                    ui.close();
                }
                if ui.add_enabled(!uncommitted, egui::Button::new("Show in History")).clicked() {
                    *act = Some(Act::Jump(c.id.clone()));
                    ui.close();
                }
                if ui.add_enabled(!uncommitted, egui::Button::new("Copy commit id")).clicked() {
                    ui.ctx().copy_text(c.id.clone());
                    ui.close();
                }
            });
        }
        // Select a line by clicking the code area.
        let code_rect = Rect::from_min_max(
            Pos2::new(gx + gutter_w, origin.y + vp.min.y),
            Pos2::new(origin.x + vp.max.x, origin.y + vp.max.y),
        );
        let r = ui.interact(code_rect, ui.id().with("blame-code"), Sense::click());
        if r.clicked()
            && let Some(pos) = r.interact_pointer_pos()
        {
            let l = ((pos.y - origin.y) / row_h).floor();
            if l >= 0.0 && (l as usize) < n {
                clicked_line = Some(l as usize);
            }
        }
    });
    if let Some(l) = clicked_line {
        st.selected = Some(l);
    }
}

/// Code line with every case-insensitive occurrence of `query` highlighted.
fn highlight_job(text: &str, query: &str, font: &egui::FontId, color: Color32, bg: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let hit = TextFormat { background: bg, ..plain.clone() };
    let mut pos = 0;
    while let Some(i) = find_ci(text, query, pos) {
        job.append(&text[pos..i], 0.0, plain.clone());
        job.append(&text[i..i + query.len()], 0.0, hit.clone());
        pos = i + query.len();
    }
    job.append(&text[pos..], 0.0, plain);
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_core::advanced::parse_blame_porcelain;

    #[test]
    fn runs_and_heat() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let data = format!(
            "{a} 1 1 2\nauthor A\nauthor-time 100\nsummary one\nfilename f\n\tx\n{a} 2 2\n\ty\n\
             {b} 1 3 1\nauthor B\nauthor-time 200\nsummary two\nprevious {a} old\nfilename f\n\tz\n\
             {a} 3 4 1\n\tw\n"
        );
        let p = prepare(parse_blame_porcelain(&data));
        assert_eq!(p.run_start, [true, false, true, true]);
        assert_eq!(p.odd_run, [false, false, true, false]);
        assert_eq!(p.heat, [0.0, 1.0]);
        assert_eq!(p.blame.commits[1].previous, Some((a.clone(), "old".to_string())));
    }

    #[test]
    fn case_insensitive_find() {
        assert_eq!(find_ci("Hello World", "world", 0), Some(6));
        assert_eq!(find_ci("abcabc", "ABC", 1), Some(3));
        assert_eq!(find_ci("ab", "abc", 0), None);
        assert_eq!(find_ci("ab", "", 0), None);
        assert_eq!(find_ci("ÄbcX", "x", 0), Some(4));
    }

    #[test]
    fn highlight_sections() {
        let j = highlight_job("foo bar FOO", "foo", &egui::FontId::monospace(12.0), Color32::WHITE, Color32::RED);
        assert_eq!(j.text, "foo bar FOO");
        assert_eq!(j.sections.iter().filter(|s| s.format.background == Color32::RED).count(), 2);
    }
}
