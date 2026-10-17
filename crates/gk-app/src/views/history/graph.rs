//! Commit graph geometry and painting, one row at a time.

use egui::epaint::CubicBezierShape;
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};
use gk_graph::{Edge, EdgeKind, Layout};

/// Lanes beyond this are clipped from the graph column.
pub const MAX_LANES: u16 = 20;

/// Horizontal padding left of the first lane.
const PAD: f32 = 4.0;

/// Center x of `lane` in a graph column starting at `left`.
pub fn lane_x(left: f32, lane: u16, lane_w: f32) -> f32 {
    left + PAD + lane_w * (lane as f32 + 0.5)
}

/// Width of the graph column for rows `[first, last)`.
pub fn column_width(
    layout: &Layout,
    first: usize,
    last: usize,
    lane_w: f32,
    extra_lane: Option<u16>,
) -> f32 {
    let last = last.min(layout.len());
    let mut lanes = layout
        .rows
        .get(first..last)
        .map(|rs| rs.iter().map(|r| r.width).max().unwrap_or(0))
        .unwrap_or(0);
    if let Some(l) = extra_lane {
        lanes = lanes.max(l + 1);
    }
    lanes.clamp(1, MAX_LANES) as f32 * lane_w + PAD * 2.0
}

/// A painted piece of an edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Line([Pos2; 2]),
    Curve([Pos2; 4]),
}

/// S-curve (or straight line when vertical) between two points.
fn connect(a: Pos2, b: Pos2) -> Seg {
    if (a.x - b.x).abs() < 0.5 {
        Seg::Line([a, b])
    } else {
        let dy = (b.y - a.y) * 0.65;
        Seg::Curve([a, pos2(a.x, a.y + dy), pos2(b.x, b.y - dy), b])
    }
}

/// Geometry of `e` within a row occupying `rect`, the node being on `node_lane`.
pub fn edge_seg(e: &Edge, node_lane: u16, rect: Rect, left: f32, lane_w: f32) -> Seg {
    let cy = rect.center().y;
    let nx = lane_x(left, node_lane, lane_w);
    match e.kind {
        EdgeKind::Up => connect(pos2(lane_x(left, e.from, lane_w), rect.top()), pos2(nx, cy)),
        EdgeKind::Through => {
            let x = lane_x(left, e.from, lane_w);
            Seg::Line([pos2(x, rect.top()), pos2(x, rect.bottom())])
        }
        EdgeKind::Down => connect(
            pos2(nx, cy),
            pos2(lane_x(left, e.to, lane_w), rect.bottom()),
        ),
    }
}

/// Everything needed to paint one graph row.
pub struct RowGraph<'a> {
    pub layout: &'a Layout,
    pub row: usize,
    pub rect: Rect,
    pub left: f32,
    pub lane_w: f32,
    pub is_head: bool,
    pub bg: Color32,
    pub head_color: Color32,
}

pub fn paint_row(p: &Painter, g: &RowGraph) {
    let Some(r) = g.layout.rows.get(g.row) else {
        return;
    };
    let width = 1.6;
    let clip_right = g.left + PAD + MAX_LANES as f32 * g.lane_w;
    // `.iter()` works whether `row_edges` returns a slice or a Vec.
    #[allow(clippy::unnecessary_to_owned, clippy::explicit_iter_loop)]
    for e in g.layout.row_edges(g.row).iter() {
        if e.from.max(e.to) >= MAX_LANES && e.from.min(e.to) >= MAX_LANES {
            continue;
        }
        let stroke = Stroke::new(width, crate::theme::lane_color(e.color));
        match edge_seg(e, r.lane, g.rect, g.left, g.lane_w) {
            Seg::Line(pts) => {
                p.line_segment(pts, stroke);
            }
            Seg::Curve(pts) => {
                p.add(CubicBezierShape::from_points_stroke(
                    pts,
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                ));
            }
        }
    }
    let cx = lane_x(g.left, r.lane, g.lane_w);
    if cx > clip_right {
        return;
    }
    let c = pos2(cx, g.rect.center().y);
    let color = crate::theme::lane_color(r.color);
    let radius = (g.lane_w * 0.3).clamp(3.0, g.rect.height() * 0.3);
    if g.is_head {
        p.circle_stroke(c, radius + 2.5, Stroke::new(1.5, g.head_color));
    }
    if r.is_merge {
        p.circle_filled(c, radius, g.bg);
        p.circle_stroke(c, radius - 0.5, Stroke::new(2.0, color));
    } else {
        p.circle_filled(c, radius, color);
    }
}

/// When row `row` needs more lanes than fit in the (capped) graph column
/// `cell`, fades its right edge into the row background: `bg` is the panel,
/// `row_fill` the translucent stripe/selection fill painted over it.
pub fn fade_overflow(
    p: &Painter,
    layout: &Layout,
    row: usize,
    cell: Rect,
    lane_w: f32,
    bg: Color32,
    row_fill: Option<Color32>,
) {
    let Some(r) = layout.rows.get(row) else {
        return;
    };
    let needed = r.width.clamp(1, MAX_LANES) as f32 * lane_w + PAD * 2.0;
    if needed <= cell.width() + 0.5 {
        return;
    }
    let w = (lane_w * 2.0).min(cell.width() / 3.0);
    let fade = Rect::from_min_max(pos2(cell.right() - w, cell.top()), cell.max);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(fade.left_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(fade.right_top(), bg);
    mesh.colored_vertex(fade.right_bottom(), bg);
    mesh.colored_vertex(fade.left_bottom(), Color32::TRANSPARENT);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
    if let Some(c) = row_fill {
        p.rect_filled(fade, 0.0, c);
    }
    // The commit's own node is cut off: point at it from the edge.
    if lane_x(cell.left(), r.lane, lane_w) > cell.right() - w {
        let color = crate::theme::lane_color(r.color);
        let cy = cell.center().y;
        let s = (cell.height() * 0.2).clamp(3.0, 5.0);
        let tip = cell.right() - 2.0;
        p.add(Shape::convex_polygon(
            vec![
                pos2(tip - s * 1.4, cy - s),
                pos2(tip, cy),
                pos2(tip - s * 1.4, cy + s),
            ],
            color,
            Stroke::NONE,
        ));
    }
}

/// The synthetic "uncommitted changes" node, connected by a dashed line to
/// the HEAD commit's node. `from_row_rect` is the synthetic row, `head_center`
/// the HEAD node (possibly far outside the viewport), `visible` the
/// on-screen y range used to clip the dashes.
pub fn paint_worktree_node(
    p: &Painter,
    center: Pos2,
    head_center: Option<Pos2>,
    visible: (f32, f32),
    radius: f32,
    color: Color32,
    bg: Color32,
) {
    if let Some(h) = head_center {
        let y0 = center.y.max(visible.0);
        let y1 = h.y.min(visible.1);
        if y1 > y0 {
            let stroke = Stroke::new(1.5, color);
            p.extend(Shape::dashed_line(
                &[pos2(center.x, y0), pos2(center.x, y1)],
                stroke,
                3.0,
                3.0,
            ));
        }
    }
    p.circle_filled(center, radius, bg);
    p.circle_stroke(center, radius - 0.5, Stroke::new(1.5, color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn layout(parents: &[&[u32]]) -> Layout {
        let mut l = Layout::new();
        for (i, p) in parents.iter().enumerate() {
            l.push(i as u32, p);
        }
        l
    }

    #[test]
    fn lanes_are_spaced() {
        assert_eq!(lane_x(10.0, 0, 14.0), 10.0 + PAD + 7.0);
        assert_eq!(lane_x(10.0, 2, 14.0) - lane_x(10.0, 1, 14.0), 14.0);
    }

    #[test]
    fn edge_geometry() {
        let rect = Rect::from_min_size(pos2(0.0, 100.0), vec2(200.0, 20.0));
        let through = Edge {
            kind: EdgeKind::Through,
            from: 1,
            to: 1,
            color: 0,
        };
        let x1 = lane_x(0.0, 1, 10.0);
        assert_eq!(
            edge_seg(&through, 0, rect, 0.0, 10.0),
            Seg::Line([pos2(x1, 100.0), pos2(x1, 120.0)])
        );

        // Up from the same lane is a straight line into the node center.
        let up = Edge {
            kind: EdgeKind::Up,
            from: 0,
            to: 0,
            color: 0,
        };
        let x0 = lane_x(0.0, 0, 10.0);
        assert_eq!(
            edge_seg(&up, 0, rect, 0.0, 10.0),
            Seg::Line([pos2(x0, 100.0), pos2(x0, 110.0)])
        );

        // Down to another lane curves from the node to the row bottom.
        let down = Edge {
            kind: EdgeKind::Down,
            from: 0,
            to: 2,
            color: 0,
        };
        match edge_seg(&down, 0, rect, 0.0, 10.0) {
            Seg::Curve([a, c1, c2, b]) => {
                assert_eq!(a, pos2(x0, 110.0));
                assert_eq!(b, pos2(lane_x(0.0, 2, 10.0), 120.0));
                assert_eq!(c1.x, a.x);
                assert_eq!(c2.x, b.x);
                assert!(c1.y > a.y && c2.y < b.y);
            }
            s => panic!("expected curve, got {s:?}"),
        }
    }

    #[test]
    fn width_is_clamped() {
        let l = layout(&[&[2], &[2], &[]]);
        assert_eq!(column_width(&l, 0, 3, 10.0, None), 2.0 * 10.0 + PAD * 2.0);
        assert_eq!(column_width(&l, 2, 3, 10.0, None), 10.0 + PAD * 2.0);
        assert_eq!(
            column_width(&l, 2, 3, 10.0, Some(3)),
            4.0 * 10.0 + PAD * 2.0
        );
        assert_eq!(
            column_width(&l, 0, 3, 10.0, Some(100)),
            MAX_LANES as f32 * 10.0 + PAD * 2.0
        );
        assert_eq!(column_width(&l, 5, 9, 10.0, None), 10.0 + PAD * 2.0);
    }
}
