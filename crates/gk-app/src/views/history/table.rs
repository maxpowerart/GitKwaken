//! Commit table building blocks shared by the history and search views:
//! resizable column header, a virtualized click/keyboard-aware row list and
//! painter-based cell rendering (only visible rows are formatted).

use crate::theme::Palette;
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Modifiers, Painter, Rangef, Rect, Response, Sense,
    Stroke, Ui, pos2, vec2,
};
use gk_core::Label;
use gk_core::ObjectId;
use gk_core::history::CommitMeta;
use std::ops::Range;

/// Resizable widths of the fixed columns (the description takes the rest).
#[derive(Debug, Clone, Copy)]
pub struct Columns {
    pub date: f32,
    pub author: f32,
    pub commit: f32,
}

impl Default for Columns {
    fn default() -> Self {
        Self {
            date: 130.0,
            author: 150.0,
            commit: 72.0,
        }
    }
}

/// Absolute x ranges of the columns for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ColRects {
    pub graph: Rangef,
    pub desc: Rangef,
    pub date: Rangef,
    pub author: Rangef,
    pub commit: Rangef,
}

const MIN_COL: f32 = 40.0;
const MIN_DESC: f32 = 120.0;
const CELL_PAD: f32 = 6.0;

/// Splits `[left, right]` into columns. Fixed columns shrink proportionally
/// when the description would get narrower than [`MIN_DESC`].
pub fn layout_columns(left: f32, right: f32, graph_w: f32, cols: &Columns) -> ColRects {
    let total = (right - left).max(0.0);
    let fixed = cols.date + cols.author + cols.commit;
    let room = (total - graph_w - MIN_DESC).max(0.0);
    let scale = if fixed > room && fixed > 0.0 {
        room / fixed
    } else {
        1.0
    };
    let (d, a, c) = (cols.date * scale, cols.author * scale, cols.commit * scale);
    let commit = Rangef::new(right - c, right);
    let author = Rangef::new(commit.min - a, commit.min);
    let date = Rangef::new(author.min - d, author.min);
    let graph = Rangef::new(left, (left + graph_w).min(date.min));
    let desc = Rangef::new(graph.max, date.min.max(graph.max));
    ColRects {
        graph,
        desc,
        date,
        author,
        commit,
    }
}

/// Paints the column header and handles separator dragging. Returns the
/// header's height.
pub fn header(ui: &mut Ui, id: Id, cols: &mut Columns, rects: &ColRects, with_graph: bool) -> f32 {
    let h = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter_at(rect);
    let v = ui.visuals();
    p.rect_filled(rect, 0.0, v.faint_bg_color);
    p.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        v.widgets.noninteractive.bg_stroke,
    );
    let font = egui::TextStyle::Small.resolve(ui.style());
    let color = v.weak_text_color();
    let title = |r: Rangef, t: &str| {
        if r.span() > 8.0 {
            p.with_clip_rect(Rect::from_x_y_ranges(r, rect.y_range()))
                .text(
                    pos2(r.min + CELL_PAD, rect.center().y),
                    Align2::LEFT_CENTER,
                    t,
                    font.clone(),
                    color,
                );
        }
    };
    if with_graph {
        title(rects.graph, "Graph");
    }
    title(rects.desc, "Description");
    title(rects.date, "Date");
    title(rects.author, "Author");
    title(rects.commit, "Commit");

    // Draggable separators: (x, which).
    let seps = [
        (rects.date.min, 0),
        (rects.author.min, 1),
        (rects.commit.min, 2),
    ];
    for (x, which) in seps {
        let r = Rect::from_x_y_ranges(Rangef::new(x - 3.0, x + 3.0), rect.y_range());
        let resp = ui.interact(r, id.with(("colsep", which)), Sense::drag());
        let active = resp.hovered() || resp.dragged();
        if active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        p.vline(
            x,
            rect.y_range().shrink(3.0),
            if active {
                v.widgets.active.fg_stroke
            } else {
                v.widgets.noninteractive.bg_stroke
            },
        );
        let dx = resp.drag_delta().x;
        if dx != 0.0 {
            match which {
                0 => cols.date = (cols.date - dx).max(MIN_COL),
                1 => {
                    let nd = (cols.date + dx).max(MIN_COL);
                    let na = (cols.author - (nd - cols.date)).max(MIN_COL);
                    cols.date = nd;
                    cols.author = na;
                }
                _ => {
                    let na = (cols.author + dx).max(MIN_COL);
                    let nc = (cols.commit - (na - cols.author)).max(MIN_COL);
                    cols.author = na;
                    cols.commit = nc;
                }
            }
        }
    }
    h
}

/// What happened in a [`virtual_list`] this frame.
pub struct ListOutput {
    pub response: Response,
    pub clicked: Option<(usize, Modifiers)>,
    pub double_clicked: Option<usize>,
    pub secondary: Option<usize>,
    /// Rows fully or partially on screen.
    pub visible: Range<usize>,
}

/// Geometry handed to the row painter.
pub struct ListFrame<'a> {
    pub painter: &'a Painter,
    pub top: f32,
    pub row_h: f32,
    pub x: Rangef,
    pub hovered: Option<usize>,
    /// On-screen y range of the list.
    pub clip_y: Rangef,
}

impl ListFrame<'_> {
    pub fn row_rect(&self, row: usize) -> Rect {
        Rect::from_x_y_ranges(
            self.x,
            Rangef::new(
                self.top + row as f32 * self.row_h,
                self.top + (row + 1) as f32 * self.row_h,
            ),
        )
    }
}

/// Rows `[first, last)` intersecting the viewport `[y0, y1)` (content coordinates).
pub fn visible_rows(y0: f32, y1: f32, row_h: f32, n: usize) -> Range<usize> {
    if n == 0 || row_h <= 0.0 {
        return 0..0;
    }
    let first = ((y0.max(0.0) / row_h).floor() as usize).min(n);
    let last = ((y1.max(0.0) / row_h).ceil() as usize).min(n);
    first..last.max(first)
}

/// A virtualized list of `n` equally tall rows. Only the visible range is
/// passed to `paint`. `scroll_to` brings a row into view. The returned
/// response is focusable (click it to focus) and carries the context menu.
pub fn virtual_list(
    ui: &mut Ui,
    id: Id,
    n: usize,
    row_h: f32,
    scroll_to: Option<usize>,
    paint: impl FnOnce(&ListFrame, Range<usize>),
) -> ListOutput {
    egui::ScrollArea::vertical()
        .id_salt(id.with("scroll"))
        .auto_shrink(false)
        .content_margin(0.0)
        .show_viewport(ui, |ui, viewport| {
            let max = ui.max_rect();
            let width = ui.available_width().max(max.width());
            let total_h = n as f32 * row_h;
            ui.set_min_size(vec2(width, total_h));
            ui.set_max_width(width);
            let top = max.top();
            let visible = visible_rows(viewport.min.y, viewport.max.y, row_h, n);
            let screen = Rect::from_x_y_ranges(
                Rangef::new(max.left(), max.left() + width),
                Rangef::new(top + viewport.min.y, top + viewport.max.y),
            );
            let response = ui.interact(screen, id, Sense::click());
            let row_at = |p: egui::Pos2| -> Option<usize> {
                let r = ((p.y - top) / row_h).floor();
                (r >= 0.0 && (r as usize) < n).then_some(r as usize)
            };
            let hovered = response.hover_pos().and_then(row_at);
            let mods = ui.input(|i| i.modifiers);
            let pointer = response.interact_pointer_pos();
            let clicked = if response.clicked() {
                pointer.and_then(row_at).map(|r| (r, mods))
            } else {
                None
            };
            let double_clicked = if response.double_clicked() {
                pointer.and_then(row_at)
            } else {
                None
            };
            let secondary = if response.secondary_clicked() {
                pointer.and_then(row_at)
            } else {
                None
            };
            if response.clicked() || response.secondary_clicked() {
                response.request_focus();
            }
            if let Some(r) = scroll_to.filter(|r| *r < n) {
                let rr = Rect::from_x_y_ranges(
                    screen.x_range(),
                    Rangef::new(top + r as f32 * row_h, top + (r + 1) as f32 * row_h),
                );
                // Only scroll when needed, keeping a little context.
                if rr.top() < screen.top() || rr.bottom() > screen.bottom() {
                    ui.scroll_to_rect(rr, None);
                }
            }
            let painter = ui.painter().clone();
            let frame = ListFrame {
                painter: &painter,
                top,
                row_h,
                x: screen.x_range(),
                hovered,
                clip_y: screen.y_range(),
            };
            paint(&frame, visible.clone());
            if response.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        id,
                        egui::EventFilter {
                            vertical_arrows: true,
                            ..Default::default()
                        },
                    )
                });
            }
            ListOutput {
                response,
                clicked,
                double_clicked,
                secondary,
                visible,
            }
        })
        .inner
}

/// Navigation keys pressed this frame (with shift state).
pub fn nav_keys(ui: &Ui) -> Vec<(egui::Key, bool)> {
    use egui::Key;
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if matches!(
                    key,
                    Key::ArrowUp
                        | Key::ArrowDown
                        | Key::PageUp
                        | Key::PageDown
                        | Key::Home
                        | Key::End
                ) =>
                {
                    Some((*key, modifiers.shift))
                }
                _ => None,
            })
            .collect()
    })
}

/// Background of a row: stripes, hover and selection.
pub fn paint_row_bg(
    f: &ListFrame,
    row: usize,
    rect: Rect,
    selected: bool,
    focused: bool,
    palette: &Palette,
    v: &egui::Visuals,
) {
    if selected {
        let c = if focused {
            palette.selection
        } else {
            palette.selection.gamma_multiply(0.6)
        };
        f.painter.rect_filled(rect, 0.0, c);
    } else if f.hovered == Some(row) {
        f.painter.rect_filled(
            rect,
            0.0,
            v.widgets.hovered.weak_bg_fill.gamma_multiply(0.5),
        );
    } else if row % 2 == 1 {
        f.painter.rect_filled(rect, 0.0, v.faint_bg_color);
    }
}

/// Single-line text truncated with an ellipsis at `max_w`.
pub fn text_trunc(
    p: &Painter,
    text: String,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text, font, color);
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    p.layout_job(job)
}

/// Paints `text` left-aligned and vertically centered in a cell.
pub fn cell_text(p: &Painter, cell: Rect, text: String, font: &FontId, color: Color32) {
    let w = cell.width() - CELL_PAD * 2.0;
    if w <= 4.0 {
        return;
    }
    let g = text_trunc(p, text, font.clone(), color, w);
    let pos = pos2(cell.left() + CELL_PAD, cell.center().y - g.size().y / 2.0);
    p.with_clip_rect(cell.intersect(p.clip_rect()))
        .galley(pos, g, color);
}

/// Display text and color of a ref label, `None` when hidden by settings.
pub fn label_style(
    l: &Label,
    palette: &Palette,
    s: &gk_settings::HistorySettings,
) -> Option<(String, Color32, bool)> {
    Some(match l {
        Label::Head => ("HEAD".to_string(), palette.head, true),
        Label::Branch { name, is_head } => (
            name.clone(),
            if *is_head {
                palette.head
            } else {
                palette.branch
            },
            *is_head,
        ),
        Label::Remote(n) if s.show_remote_branches => (n.clone(), palette.remote, false),
        Label::Tag(n) if s.show_tags => (n.clone(), palette.tag, false),
        Label::Stash if s.show_stashes => ("stash".to_string(), palette.stash, false),
        _ => return None,
    })
}

/// Paints a ref chip at `x`; returns the x after it.
#[allow(clippy::too_many_arguments)]
pub fn paint_chip(
    p: &Painter,
    x: f32,
    cy: f32,
    max_x: f32,
    text: String,
    color: Color32,
    bold: bool,
    font: &FontId,
) -> f32 {
    let room = max_x - x - 8.0;
    if room < 16.0 {
        return x;
    }
    let g = text_trunc(p, text, font.clone(), Color32::WHITE, room.min(220.0));
    let size = g.size();
    let rect = Rect::from_min_size(
        pos2(x, cy - size.y / 2.0 - 1.0),
        vec2(size.x + 8.0, size.y + 2.0),
    );
    p.rect_filled(rect, CornerRadius::same(3), color);
    if bold {
        p.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, Color32::WHITE),
            egui::StrokeKind::Inside,
        );
        p.galley(
            pos2(rect.left() + 4.5, rect.top() + 1.0),
            g.clone(),
            Color32::WHITE,
        );
    }
    p.galley(pos2(rect.left() + 4.0, rect.top() + 1.0), g, Color32::WHITE);
    rect.right() + 4.0
}

/// Style shared by all cells of a frame.
pub struct CellStyle<'a> {
    pub font: FontId,
    pub small: FontId,
    pub mono: FontId,
    pub text: Color32,
    pub muted: Color32,
    pub palette: &'a Palette,
    pub settings: &'a gk_settings::HistorySettings,
}

/// Paints description (chips + summary), date, author and short id of a commit row.
#[allow(clippy::too_many_arguments)]
pub fn paint_commit_cells(
    p: &Painter,
    rect: Rect,
    cols: &ColRects,
    st: &CellStyle,
    id: &ObjectId,
    meta: Option<&CommitMeta>,
    labels: Option<&[Label]>,
    bold: bool,
) {
    let y = rect.y_range();
    let desc = Rect::from_x_y_ranges(cols.desc, y);
    let dp = p.with_clip_rect(desc.intersect(p.clip_rect()));
    let mut x = desc.left() + CELL_PAD;
    if let Some(ls) = labels {
        for l in ls {
            if let Some((t, c, b)) = label_style(l, st.palette, st.settings) {
                x = paint_chip(&dp, x, rect.center().y, desc.right(), t, c, b, &st.small);
            }
        }
    }
    let summary_cell = Rect::from_x_y_ranges(Rangef::new(x - CELL_PAD, desc.right()), y);
    match meta {
        Some(m) => {
            cell_text(&dp, summary_cell, m.summary.clone(), &st.font, st.text);
            if bold {
                // Faux bold for the HEAD row.
                cell_text(
                    &dp,
                    summary_cell.translate(vec2(0.5, 0.0)),
                    m.summary.clone(),
                    &st.font,
                    st.text,
                );
            }
            let date = crate::util::format_time(m.time, m.offset, st.settings.relative_dates);
            cell_text(
                p,
                Rect::from_x_y_ranges(cols.date, y),
                date,
                &st.font,
                st.muted,
            );
            cell_text(
                p,
                Rect::from_x_y_ranges(cols.author, y),
                m.author.clone(),
                &st.font,
                st.text,
            );
        }
        None => cell_text(&dp, summary_cell, "…".to_string(), &st.font, st.muted),
    }
    let mut hex = [0u8; 40];
    let full = id.hex_to_buf(&mut hex);
    let short = &full[..8.min(full.len())];
    cell_text(
        p,
        Rect::from_x_y_ranges(cols.commit, y),
        short.to_string(),
        &st.mono,
        st.muted,
    );
}

/// Looks up the ref labels of `id` without allocating.
pub fn labels_of<'a>(
    labels: &'a std::collections::BTreeMap<String, Vec<Label>>,
    id: &ObjectId,
) -> Option<&'a [Label]> {
    if labels.is_empty() {
        return None;
    }
    let mut hex = [0u8; 40];
    let s = id.hex_to_buf(&mut hex);
    labels.get(&*s).map(|v| v.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_range() {
        assert_eq!(visible_rows(0.0, 100.0, 20.0, 1000), 0..5);
        assert_eq!(visible_rows(10.0, 110.0, 20.0, 1000), 0..6);
        assert_eq!(visible_rows(19_990.0, 20_100.0, 20.0, 1000), 999..1000);
        assert_eq!(visible_rows(0.0, 100.0, 20.0, 0), 0..0);
        assert_eq!(visible_rows(500.0, 400.0, 20.0, 10), 10..10);
    }

    #[test]
    fn columns_fill_width() {
        let c = Columns {
            date: 100.0,
            author: 100.0,
            commit: 50.0,
        };
        let r = layout_columns(0.0, 1000.0, 60.0, &c);
        assert_eq!(r.graph, Rangef::new(0.0, 60.0));
        assert_eq!(r.desc, Rangef::new(60.0, 750.0));
        assert_eq!(r.date, Rangef::new(750.0, 850.0));
        assert_eq!(r.commit, Rangef::new(950.0, 1000.0));
        // Narrow: fixed columns shrink so the description keeps its minimum.
        let r = layout_columns(0.0, 400.0, 30.0, &c);
        assert!((r.desc.span() - MIN_DESC).abs() < 0.01);
        assert!((r.commit.max - 400.0).abs() < 0.01);
        // Too narrow for anything: ranges stay ordered.
        let r = layout_columns(0.0, 50.0, 30.0, &c);
        assert!(r.desc.min <= r.desc.max && r.graph.min <= r.graph.max);
    }
}
