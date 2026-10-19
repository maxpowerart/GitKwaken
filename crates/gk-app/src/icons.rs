//! GitKwaken's icon set ("Kwak"), drawn as vectors.
//!
//! The style follows the logo, a frog whose tentacles form a git graph: soft, round-capped
//! teal strokes, and one orange "commit node" per icon where it means something (the commit a
//! check mark lands on, the node an arrow pulls into, the tips of a branch). Every icon is
//! designed on a 24 × 24 grid and drawn at any size, so it is sharp at every font size and
//! looks the same on every platform (no font has to provide a glyph).

use egui::epaint::PathShape;
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Commit,
    Pull,
    Push,
    Fetch,
    Branch,
    Merge,
    Stash,
    Discard,
    Tag,
    Flow,
    Remote,
    Terminal,
    Explorer,
    Folder,
    Settings,
    FileStatus,
    History,
    Search,
    Reflog,
    Blame,
    Submodule,
    Worktree,
    Add,
    Close,
    Undo,
    Up,
    Down,
    Back,
    Forward,
    Refresh,
    Copy,
    Update,
    Warning,
    Ok,
    Error,
    List,
    Tree,
    Pin,
    PinOff,
    Filter,
    FilterOff,
    Bisect,
}

impl Icon {
    /// Name in style files (`[icons.files]`), see `gk_settings::style::ICON_NAMES`.
    pub fn name(self) -> &'static str {
        use Icon::*;
        match self {
            Commit => "commit",
            Pull => "pull",
            Push => "push",
            Fetch => "fetch",
            Branch => "branch",
            Merge => "merge",
            Stash => "stash",
            Discard => "discard",
            Tag => "tag",
            Flow => "flow",
            Remote => "remote",
            Terminal => "terminal",
            Explorer => "explorer",
            Folder => "folder",
            Settings => "settings",
            FileStatus => "file_status",
            History => "history",
            Search => "search",
            Reflog => "reflog",
            Blame => "blame",
            Submodule => "submodule",
            Worktree => "worktree",
            Add => "add",
            Close => "close",
            Undo => "undo",
            Up => "up",
            Down => "down",
            Back => "back",
            Forward => "forward",
            Refresh => "refresh",
            Copy => "copy",
            Update => "update",
            Warning => "warning",
            Ok => "ok",
            Error => "error",
            List => "list",
            Tree => "tree",
            Pin => "pin",
            PinOff => "pin_off",
            Filter => "filter",
            FilterOff => "filter_off",
            Bisect => "bisect",
        }
    }
}

/// Stroke and node colors of one icon.
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub stroke: Color32,
    pub node: Color32,
}

impl Colors {
    /// The theme's icon colors ([`crate::theme::icon_colors`]).
    pub fn theme() -> Self {
        let (stroke, node) = crate::theme::icon_colors();
        Self { stroke, node }
    }

    /// One color for everything (selected rows, buttons on a colored fill).
    pub fn mono(c: Color32) -> Self {
        Self { stroke: c, node: c }
    }

    /// Grayed out: disabled widgets.
    pub fn disabled(weak: Color32) -> Self {
        Self {
            stroke: weak,
            node: weak,
        }
    }
}

/// Where an icon is drawn: the screen, or an SVG document (export). Coordinates are on the
/// 24-unit design grid; `accent` parts use the node color.
trait Sink {
    /// A polyline with round caps and joins.
    fn stroke_path(&self, pts: &[(f32, f32)], closed: bool, accent: bool);
    fn stroke_circle(&self, c: (f32, f32), r: f32);
    fn fill_circle(&self, c: (f32, f32), r: f32, accent: bool);
    /// A filled convex polygon.
    fn fill_convex(&self, pts: &[(f32, f32)]);
    fn stroke_rrect(&self, min: (f32, f32), max: (f32, f32), r: f32);
}

/// Draws on screen: the grid mapped onto a square at `origin`.
struct Screen<'a> {
    p: &'a Painter,
    origin: Pos2,
    unit: f32,
    stroke: Stroke,
    node: Color32,
}

impl Screen<'_> {
    fn at(&self, (x, y): (f32, f32)) -> Pos2 {
        self.origin + vec2(x, y) * self.unit
    }
}

impl Sink for Screen<'_> {
    fn stroke_path(&self, pts: &[(f32, f32)], closed: bool, accent: bool) {
        let stroke = if accent {
            Stroke::new(self.stroke.width, self.node)
        } else {
            self.stroke
        };
        let pts: Vec<Pos2> = pts.iter().map(|&q| self.at(q)).collect();
        stroke_screen(self.p, pts, closed, stroke);
    }

    fn stroke_circle(&self, c: (f32, f32), r: f32) {
        self.p.circle_stroke(self.at(c), r * self.unit, self.stroke);
    }

    fn fill_circle(&self, c: (f32, f32), r: f32, accent: bool) {
        let color = if accent { self.node } else { self.stroke.color };
        self.p.circle_filled(self.at(c), r * self.unit, color);
    }

    fn fill_convex(&self, pts: &[(f32, f32)]) {
        let pts = pts.iter().map(|&q| self.at(q)).collect();
        self.p.add(PathShape::convex_polygon(
            pts,
            self.stroke.color,
            Stroke::NONE,
        ));
    }

    fn stroke_rrect(&self, min: (f32, f32), max: (f32, f32), r: f32) {
        let rect = Rect::from_min_max(self.at(min), self.at(max));
        self.p
            .rect_stroke(rect, r * self.unit, self.stroke, egui::StrokeKind::Middle);
    }
}

/// A screen polyline with round caps and joins.
fn stroke_screen(p: &Painter, pts: Vec<Pos2>, closed: bool, stroke: Stroke) {
    for q in &pts {
        p.circle_filled(*q, stroke.width * 0.5, stroke.color);
    }
    p.add(if closed {
        PathShape::closed_line(pts, stroke)
    } else {
        PathShape::line(pts, stroke)
    });
}

/// Writes SVG elements (the export of the built-in icons).
struct Svg {
    out: std::cell::RefCell<String>,
}

impl Svg {
    fn points(pts: &[(f32, f32)]) -> String {
        pts.iter()
            .map(|(x, y)| format!("{} {}", fmt(*x), fmt(*y)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn push(&self, s: String) {
        let mut o = self.out.borrow_mut();
        o.push_str("  ");
        o.push_str(&s);
        o.push('\n');
    }
}

/// A coordinate with at most two decimals.
fn fmt(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Sink for Svg {
    fn stroke_path(&self, pts: &[(f32, f32)], closed: bool, accent: bool) {
        let tag = if closed { "polygon" } else { "polyline" };
        let class = if accent { r#" class="node""# } else { "" };
        self.push(format!(r#"<{tag}{class} points="{}"/>"#, Svg::points(pts)));
    }

    fn stroke_circle(&self, (x, y): (f32, f32), r: f32) {
        self.push(format!(
            r#"<circle cx="{}" cy="{}" r="{}"/>"#,
            fmt(x),
            fmt(y),
            fmt(r)
        ));
    }

    fn fill_circle(&self, (x, y): (f32, f32), r: f32, accent: bool) {
        let class = if accent { r#" class="node""# } else { "" };
        self.push(format!(
            r#"<circle{class} cx="{}" cy="{}" r="{}" fill="currentColor" stroke="none"/>"#,
            fmt(x),
            fmt(y),
            fmt(r)
        ));
    }

    fn fill_convex(&self, pts: &[(f32, f32)]) {
        self.push(format!(
            r#"<polygon points="{}" fill="currentColor" stroke="none"/>"#,
            Svg::points(pts)
        ));
    }

    fn stroke_rrect(&self, min: (f32, f32), max: (f32, f32), r: f32) {
        self.push(format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}"/>"#,
            fmt(min.0),
            fmt(min.1),
            fmt(max.0 - min.0),
            fmt(max.1 - min.1),
            fmt(r)
        ));
    }
}

/// The drawing vocabulary of the icon set, on the 24-unit grid.
struct Pen<'a> {
    sink: &'a dyn Sink,
}

impl Pen<'_> {
    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.sink.stroke_path(&[a, b], false, false);
    }

    fn poly(&self, pts: &[(f32, f32)], closed: bool) {
        self.sink.stroke_path(pts, closed, false);
    }

    /// Arc around `c` from `a0` to `a1` degrees (0 = right, 90 = down).
    fn arc(&self, c: (f32, f32), r: f32, a0: f32, a1: f32) {
        let n = (((a1 - a0).abs() / 12.0).ceil() as usize).max(2);
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
                (c.0 + r * a.cos(), c.1 + r * a.sin())
            })
            .collect();
        self.sink.stroke_path(&pts, false, false);
    }

    fn circle(&self, c: (f32, f32), r: f32) {
        self.sink.stroke_circle(c, r);
    }

    fn quad(&self, a: (f32, f32), ctrl: (f32, f32), b: (f32, f32)) {
        let pts: Vec<(f32, f32)> = (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0;
                let u = 1.0 - t;
                (
                    u * u * a.0 + 2.0 * u * t * ctrl.0 + t * t * b.0,
                    u * u * a.1 + 2.0 * u * t * ctrl.1 + t * t * b.1,
                )
            })
            .collect();
        self.sink.stroke_path(&pts, false, false);
    }

    fn cubic(&self, a: (f32, f32), c1: (f32, f32), c2: (f32, f32), b: (f32, f32)) {
        let pts: Vec<(f32, f32)> = (0..=20)
            .map(|i| {
                let t = i as f32 / 20.0;
                let u = 1.0 - t;
                let c = |p0: f32, p1: f32, p2: f32, p3: f32| {
                    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
                };
                (c(a.0, c1.0, c2.0, b.0), c(a.1, c1.1, c2.1, b.1))
            })
            .collect();
        self.sink.stroke_path(&pts, false, false);
    }

    fn rrect(&self, min: (f32, f32), max: (f32, f32), r: f32) {
        self.sink.stroke_rrect(min, max, r);
    }

    /// The orange commit node.
    fn node(&self, c: (f32, f32), r: f32) {
        self.sink.fill_circle(c, r, true);
    }

    /// A stroke in the node color (terminal cursor, warning mark).
    fn accent_line(&self, a: (f32, f32), b: (f32, f32)) {
        self.sink.stroke_path(&[a, b], false, true);
    }

    fn fill(&self, pts: &[(f32, f32)]) {
        self.sink.fill_convex(pts);
    }

    /// Arrow head at `tip` pointing along `dir` degrees.
    fn head(&self, tip: (f32, f32), dir: f32, len: f32) {
        let back = (dir + 180.0).to_radians();
        let spread = 40f32.to_radians();
        let a = (
            tip.0 + len * (back - spread).cos(),
            tip.1 + len * (back - spread).sin(),
        );
        let b = (
            tip.0 + len * (back + spread).cos(),
            tip.1 + len * (back + spread).sin(),
        );
        self.poly(&[a, tip, b], false);
    }
}

/// Draws `icon` into the square centered in `rect`: the style's SVG for it when the style
/// replaces it, else the built-in drawing.
pub fn paint(p: &Painter, icon: Icon, rect: Rect, colors: Colors) {
    let side = rect.width().min(rect.height());
    let unit = side / 24.0;
    let origin = rect.center() - Vec2::splat(side / 2.0);
    if let Some(shape) = crate::theme::custom_icon(icon.name()) {
        paint_svg(p, &shape, rect, colors);
        return;
    }
    // Thicker relative to the size when small, so tiny icons stay legible.
    let width = (crate::theme::icon_width() * unit).max(1.25);
    let screen = Screen {
        p,
        origin,
        unit,
        stroke: Stroke::new(width, colors.stroke),
        node: colors.node,
    };
    draw(&Pen { sink: &screen }, icon);
}

/// Draws a style's SVG icon into the square centered in `rect`.
fn paint_svg(p: &Painter, shape: &gk_settings::icon_svg::IconShape, rect: Rect, colors: Colors) {
    use gk_settings::icon_svg::Part;
    let [vx, vy, vw, vh] = shape.view_box;
    let side = rect.width().min(rect.height());
    let scale = side / vw.max(vh);
    let origin = rect.center() - vec2(vw, vh) * scale / 2.0;
    let at = |[x, y]: [f32; 2]| origin + vec2(x - vx, y - vy) * scale;
    for item in &shape.items {
        let color = if item.node {
            colors.node
        } else {
            colors.stroke
        };
        if item.fill {
            match &item.part {
                Part::Circle { center, radius } => {
                    p.circle_filled(at(*center), radius * scale, color);
                }
                Part::Poly { points, .. } => {
                    let pts = points.iter().map(|q| at(*q)).collect();
                    p.add(PathShape::convex_polygon(pts, color, Stroke::NONE));
                }
            }
        }
        if let Some(w) = item.stroke {
            let stroke = Stroke::new((w * scale).max(1.0), color);
            match &item.part {
                Part::Circle { center, radius } => {
                    p.circle_stroke(at(*center), radius * scale, stroke);
                }
                Part::Poly { points, closed } => {
                    stroke_screen(p, points.iter().map(|q| at(*q)).collect(), *closed, stroke);
                }
            }
        }
    }
}

/// The built-in drawing of `icon`.
fn draw(pen: &Pen, icon: Icon) {
    let n = 2.7; // node radius
    use Icon::*;
    match icon {
        Commit => {
            pen.poly(&[(3.5, 13.0), (8.5, 18.0), (18.0, 7.5)], false);
            pen.node((19.0, 6.0), n);
        }
        Pull => {
            pen.line((12.0, 3.5), (12.0, 13.5));
            pen.head((12.0, 14.0), 90.0, 6.0);
            pen.node((12.0, 20.0), n);
        }
        Push | Update => {
            pen.line((12.0, 16.0), (12.0, 5.0));
            pen.head((12.0, 4.0), -90.0, 6.0);
            pen.node((12.0, 20.0), n);
        }
        Fetch | Refresh => {
            // Clockwise, the head at the end of the arc (upper left), pointing along it.
            pen.arc((12.0, 12.0), 8.0, -40.0, 235.0);
            let end = 235f32.to_radians();
            pen.head(
                (12.0 + 8.0 * end.cos(), 12.0 + 8.0 * end.sin()),
                235.0 + 90.0,
                4.5,
            );
            if icon == Fetch {
                pen.node((12.0, 12.0), 2.4);
            }
        }
        Branch => {
            pen.line((7.0, 6.5), (7.0, 17.5));
            pen.cubic((17.0, 8.5), (17.0, 13.5), (8.0, 12.5), (7.2, 16.0));
            pen.node((7.0, 4.5), n);
            pen.node((7.0, 19.5), n);
            pen.node((17.0, 6.0), n);
        }
        Merge => {
            pen.quad((6.0, 3.5), (6.0, 11.0), (11.0, 14.5));
            pen.quad((18.0, 3.5), (18.0, 11.0), (13.0, 14.5));
            pen.line((12.0, 15.5), (12.0, 21.0));
            pen.node((12.0, 15.0), n);
        }
        Stash => {
            pen.rrect((3.5, 5.0), (20.5, 9.5), 1.8);
            pen.poly(&[(5.0, 9.5), (5.0, 19.0), (19.0, 19.0), (19.0, 9.5)], false);
            pen.node((12.0, 14.5), 2.4);
        }
        Discard | Close | Error => {
            let d = if icon == Close { 6.5 } else { 6.0 };
            pen.line((12.0 - d, 12.0 - d), (12.0 + d, 12.0 + d));
            pen.line((12.0 + d, 12.0 - d), (12.0 - d, 12.0 + d));
        }
        Tag => {
            pen.poly(
                &[
                    (3.5, 12.0),
                    (11.0, 4.0),
                    (20.0, 4.0),
                    (20.0, 13.0),
                    (12.0, 20.5),
                ],
                true,
            );
            pen.node((15.5, 8.5), 2.2);
        }
        Flow => {
            pen.line((6.0, 20.0), (6.0, 4.5));
            pen.poly(
                &[
                    (6.0, 4.5),
                    (18.5, 4.5),
                    (15.0, 8.75),
                    (18.5, 13.0),
                    (6.0, 13.0),
                ],
                false,
            );
            pen.node((6.0, 20.0), n);
        }
        Remote => {
            pen.arc((8.0, 14.5), 4.0, 90.0, 250.0);
            pen.arc((12.5, 10.5), 5.0, 195.0, 335.0);
            pen.arc((17.0, 14.5), 4.0, -70.0, 90.0);
            pen.line((8.0, 18.5), (17.0, 18.5));
            pen.node((12.5, 14.5), 2.2);
        }
        Terminal => {
            pen.rrect((3.0, 4.5), (21.0, 19.5), 2.5);
            pen.poly(&[(7.0, 9.5), (10.0, 12.0), (7.0, 14.5)], false);
            pen.accent_line((12.5, 15.0), (16.5, 15.0));
        }
        Explorer | Folder => {
            pen.poly(
                &[
                    (3.0, 6.5),
                    (3.0, 18.5),
                    (21.0, 18.5),
                    (21.0, 9.0),
                    (11.5, 9.0),
                    (9.5, 6.0),
                    (3.5, 6.0),
                ],
                true,
            );
            if icon == Explorer {
                pen.node((15.5, 14.0), 2.2);
            }
        }
        Settings => {
            pen.circle((12.0, 12.0), 6.0);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                pen.line(
                    (12.0 + 7.5 * a.cos(), 12.0 + 7.5 * a.sin()),
                    (12.0 + 9.5 * a.cos(), 12.0 + 9.5 * a.sin()),
                );
            }
            pen.node((12.0, 12.0), 2.4);
        }
        FileStatus => {
            pen.poly(
                &[
                    (6.0, 3.5),
                    (13.5, 3.5),
                    (18.5, 8.5),
                    (18.5, 20.5),
                    (6.0, 20.5),
                ],
                true,
            );
            pen.poly(&[(13.5, 3.5), (13.5, 8.5), (18.5, 8.5)], false);
            pen.line((9.0, 12.5), (15.0, 12.5));
            pen.line((9.0, 16.5), (12.5, 16.5));
            pen.node((17.5, 18.5), 2.6);
        }
        History => {
            pen.circle((12.0, 12.0), 8.5);
            pen.poly(&[(12.0, 7.0), (12.0, 12.0), (15.5, 14.0)], false);
            pen.node((12.0, 12.0), 2.0);
        }
        Search => {
            pen.circle((10.5, 10.5), 6.0);
            pen.line((15.0, 15.0), (20.0, 20.0));
        }
        Reflog => {
            // Counter-clockwise (back in time), the head at the end (upper right).
            pen.arc((12.0, 12.0), 8.0, 220.0, -55.0);
            let end = (-55f32).to_radians();
            pen.head(
                (12.0 + 8.0 * end.cos(), 12.0 + 8.0 * end.sin()),
                -55.0 - 90.0,
                4.5,
            );
            pen.node((12.0, 12.0), 2.4);
        }
        Undo => {
            pen.head((4.5, 9.5), 180.0, 5.0);
            pen.poly(&[(4.5, 9.5), (14.0, 9.5)], false);
            pen.arc((14.0, 14.5), 5.0, -90.0, 90.0);
            pen.line((14.0, 19.5), (8.5, 19.5));
        }
        Blame => {
            pen.circle((12.0, 8.0), 4.0);
            pen.arc((12.0, 21.0), 7.5, 180.0, 360.0);
        }
        Submodule => {
            pen.poly(
                &[
                    (12.0, 3.0),
                    (20.0, 7.5),
                    (20.0, 16.5),
                    (12.0, 21.0),
                    (4.0, 16.5),
                    (4.0, 7.5),
                ],
                true,
            );
            pen.poly(&[(4.0, 7.5), (12.0, 12.0), (20.0, 7.5)], false);
            pen.line((12.0, 12.0), (12.0, 21.0));
            pen.node((12.0, 12.0), 2.2);
        }
        Worktree => {
            pen.line((12.0, 20.5), (12.0, 8.0));
            pen.quad((12.0, 15.0), (12.0, 11.5), (7.0, 10.0));
            pen.quad((12.0, 12.5), (12.0, 9.0), (17.0, 7.5));
            pen.node((12.0, 5.5), 2.4);
            pen.node((6.0, 9.5), 2.4);
            pen.node((18.0, 7.0), 2.4);
        }
        Add => {
            pen.line((12.0, 5.0), (12.0, 19.0));
            pen.line((5.0, 12.0), (19.0, 12.0));
        }
        Up => pen.poly(&[(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)], false),
        Down => pen.poly(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], false),
        Back => pen.poly(&[(15.0, 6.0), (9.0, 12.0), (15.0, 18.0)], false),
        Forward => pen.poly(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)], false),
        Copy => {
            pen.rrect((8.0, 8.0), (19.5, 19.5), 2.0);
            pen.poly(&[(15.5, 4.5), (4.5, 4.5), (4.5, 15.5)], false);
        }
        Warning => {
            pen.poly(&[(12.0, 3.5), (21.0, 19.5), (3.0, 19.5)], true);
            pen.accent_line((12.0, 9.5), (12.0, 13.5));
            pen.node((12.0, 16.5), 1.4);
        }
        Ok => pen.poly(&[(4.5, 12.5), (9.5, 17.5), (19.5, 6.5)], false),
        List => {
            pen.line((5.0, 7.0), (19.0, 7.0));
            pen.line((5.0, 12.0), (19.0, 12.0));
            pen.line((5.0, 17.0), (19.0, 17.0));
        }
        Tree => {
            pen.line((5.0, 5.0), (19.0, 5.0));
            pen.poly(&[(7.0, 5.0), (7.0, 17.0), (11.0, 17.0)], false);
            pen.line((7.0, 11.0), (11.0, 11.0));
            pen.line((14.0, 11.0), (19.0, 11.0));
            pen.line((14.0, 17.0), (19.0, 17.0));
        }
        Pin | PinOff => {
            // Upright push pin: cap, body, collar, needle.
            pen.line((8.0, 3.5), (16.0, 3.5));
            if icon == Pin {
                pen.fill(&[(9.5, 3.5), (14.5, 3.5), (14.5, 11.5), (9.5, 11.5)]);
            } else {
                pen.poly(&[(9.5, 3.5), (9.5, 11.5)], false);
                pen.poly(&[(14.5, 3.5), (14.5, 11.5)], false);
            }
            pen.line((6.0, 12.0), (18.0, 12.0));
            pen.line((12.0, 12.0), (12.0, 20.5));
        }
        Filter | FilterOff => {
            let cup = [
                (4.0, 5.0),
                (20.0, 5.0),
                (14.0, 12.0),
                (14.0, 18.5),
                (10.0, 20.5),
                (10.0, 12.0),
            ];
            if icon == Filter {
                pen.fill(&[cup[0], cup[1], cup[2], cup[5]]);
                pen.fill(&[cup[5], cup[2], cup[3], cup[4]]);
            }
            pen.poly(&cup, true);
        }
        Bisect => {
            // A lens over a commit: looking for the one that broke it.
            pen.circle((10.5, 10.5), 6.5);
            pen.line((15.3, 15.3), (20.5, 20.5));
            pen.node((10.5, 10.5), 2.6);
        }
    }
}

/// An icon the height of body text (`scale` times), as a widget in a row.
pub fn show(ui: &mut Ui, icon: Icon, scale: f32, colors: Colors) -> Response {
    let h = ui.text_style_height(&egui::TextStyle::Body) * scale;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(h), Sense::hover());
    paint(ui.painter(), icon, rect, colors);
    resp
}

/// Icon followed by text, in `color` (status lines in dialogs: ✔ ok / ⚠ warning / ✖ error).
pub fn label(
    ui: &mut Ui,
    icon: Icon,
    text: impl Into<egui::WidgetText>,
    color: Color32,
) -> Response {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        show(ui, icon, 1.0, Colors::mono(color));
        ui.label(text.into().color(color))
    })
    .inner
}

/// A button with an icon and optional text, framed like egui's buttons.
pub struct IconButton {
    icon: Icon,
    text: Option<egui::WidgetText>,
    small: bool,
    selected: bool,
    fill: Option<Color32>,
    min_size: Vec2,
}

impl IconButton {
    pub fn new(icon: Icon) -> Self {
        Self {
            icon,
            text: None,
            small: false,
            selected: false,
            fill: None,
            min_size: Vec2::ZERO,
        }
    }

    pub fn text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Like `Button::small`: no minimum height, tighter padding.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Background color (a call to action); text and icon turn black or white, whichever
    /// reads better on it.
    pub fn fill(mut self, fill: Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }
}

impl egui::Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let text_h = ui.text_style_height(&egui::TextStyle::Body);
        // As tall as the text: an icon button is exactly as high as a text button.
        let icon_side = text_h.round();
        let mut pad = ui.spacing().button_padding;
        if self.small {
            pad.y = 0.0;
        }
        let galley = self.text.map(|t| {
            t.into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            )
        });
        let gap = ui.spacing().icon_spacing;
        let text_w = galley.as_ref().map_or(0.0, |g| gap + g.size().x);
        let min_h = if self.small {
            0.0
        } else {
            ui.spacing().interact_size.y
        };
        let size = vec2(
            pad.x * 2.0 + icon_side + text_w,
            (icon_side.max(text_h) + pad.y * 2.0).max(min_h),
        )
        .max(self.min_size);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let v = ui.style().interact_selectable(&resp, self.selected);
            let fill = self.fill.unwrap_or(v.weak_bg_fill);
            ui.painter().rect(
                rect.expand(v.expansion),
                v.corner_radius,
                fill,
                v.bg_stroke,
                egui::StrokeKind::Inside,
            );
            let enabled = ui.is_enabled();
            let colors = if !enabled {
                Colors::disabled(ui.visuals().weak_text_color())
            } else if let Some(f) = self.fill {
                Colors::mono(on_fill(f))
            } else if self.selected {
                Colors::mono(v.fg_stroke.color)
            } else {
                Colors::theme()
            };
            // Icon and text centered when the button is wider than they are.
            let content_w = icon_side + text_w;
            let left = rect.center().x - content_w / 2.0 - pad.x;
            let icon_rect = Rect::from_center_size(
                pos2(left + pad.x + icon_side / 2.0, rect.center().y),
                Vec2::splat(icon_side),
            );
            paint(ui.painter(), self.icon, icon_rect, colors);
            if let Some(g) = galley {
                let text_color = match self.fill {
                    Some(f) if enabled => on_fill(f),
                    _ => v.text_color(),
                };
                let pos = pos2(icon_rect.right() + gap, rect.center().y - g.size().y / 2.0);
                ui.painter().galley(pos, g, text_color);
            }
        }
        resp
    }
}

/// Every icon, for review: `GITKWAKEN_ICON_SHEET=1` shows this instead of the app.
pub const ALL: [Icon; 42] = [
    Icon::Commit,
    Icon::Pull,
    Icon::Push,
    Icon::Fetch,
    Icon::Branch,
    Icon::Merge,
    Icon::Stash,
    Icon::Discard,
    Icon::Tag,
    Icon::Flow,
    Icon::Remote,
    Icon::Terminal,
    Icon::Explorer,
    Icon::Settings,
    Icon::Bisect,
    Icon::FileStatus,
    Icon::History,
    Icon::Search,
    Icon::Reflog,
    Icon::Blame,
    Icon::Folder,
    Icon::Submodule,
    Icon::Worktree,
    Icon::Add,
    Icon::Close,
    Icon::Undo,
    Icon::Up,
    Icon::Down,
    Icon::Back,
    Icon::Forward,
    Icon::Refresh,
    Icon::Copy,
    Icon::Update,
    Icon::Warning,
    Icon::Ok,
    Icon::Error,
    Icon::List,
    Icon::Tree,
    Icon::Pin,
    Icon::PinOff,
    Icon::Filter,
    Icon::FilterOff,
];

/// The icon sheet (see [`ALL`]) in `style`: each icon at 16, 22 and 34 points on the dark
/// panel, the dark title bar and the light panel (`GITKWAKEN_ICON_SHEET=1`, for style authors).
pub fn sheet(ui: &mut Ui, style: &gk_settings::style::Style) {
    let c = |r: gk_settings::style::Rgba| {
        let [r, g, b, a] = r.0;
        Color32::from_rgba_unmultiplied(r, g, b, a)
    };
    let dark_bg = style
        .dark
        .panel
        .map_or(Color32::from_rgb(0x25, 0x27, 0x2B), c);
    let navy = c(style.dark.chrome);
    let light_bg = style
        .light
        .panel
        .map_or(Color32::from_rgb(0xF8, 0xF8, 0xF8), c);
    let colors = |v: &gk_settings::style::Variant| Colors {
        stroke: c(v.icon),
        node: if style.icons.nodes {
            c(v.node)
        } else {
            c(v.icon)
        },
    };
    let (dark, light) = (colors(&style.dark), colors(&style.light));
    let full = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(full, 0.0, dark_bg);
    let cols = 7;
    let cell = vec2(full.width() / cols as f32, 150.0);
    let font = egui::FontId::proportional(13.0);
    for (i, icon) in ALL.iter().enumerate() {
        let (col, row) = (i % cols, i / cols);
        let min = full.min + vec2(col as f32 * cell.x, row as f32 * cell.y);
        let r = Rect::from_min_size(min, cell).shrink(4.0);
        // Three bands: dark panel, navy title bar, light panel.
        let band = r.height() / 3.0 - 8.0;
        let rows = [
            (
                Rect::from_min_size(r.min, vec2(r.width(), band)),
                dark_bg,
                dark,
            ),
            (
                Rect::from_min_size(r.min + vec2(0.0, band), vec2(r.width(), band)),
                navy,
                dark,
            ),
            (
                Rect::from_min_size(r.min + vec2(0.0, band * 2.0), vec2(r.width(), band)),
                light_bg,
                light,
            ),
        ];
        for (rr, bg, colors) in rows {
            p.rect_filled(rr, 0.0, bg);
            let mut x = rr.left() + 8.0;
            for size in [16.0, 22.0, 34.0] {
                let ir =
                    Rect::from_min_size(pos2(x, rr.center().y - size / 2.0), Vec2::splat(size));
                paint(p, *icon, ir, colors);
                x += size + 12.0;
            }
        }
        p.text(
            pos2(r.left() + 4.0, r.bottom() - 6.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{icon:?}"),
            font.clone(),
            Color32::from_gray(200),
        );
    }
}

/// Black or white, whichever reads better on `fill`.
fn on_fill(fill: Color32) -> Color32 {
    let l = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    if l > 150.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// The built-in drawing of `icon` as an SVG document (24 × 24, stroke `width`), in the format
/// style files read: `currentColor` strokes, `class="node"` for the commit nodes.
pub fn builtin_svg(icon: Icon, width: f32) -> String {
    let svg = Svg {
        out: std::cell::RefCell::new(String::new()),
    };
    draw(&Pen { sink: &svg }, icon);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n{}</svg>\n",
        fmt(width),
        svg.out.into_inner()
    )
}

/// Writes every icon of `style` as an SVG file into `dir` (the style's own SVG where it
/// replaces one), plus `icons.toml` with the `[icons.files]` section that uses them.
pub fn export(style: &gk_settings::style::Style, dir: &std::path::Path) -> anyhow::Result<usize> {
    use anyhow::Context;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let mut toml = String::from(
        "# Paste into a style file (Settings > General > Style > New style from this one) and\n\
         # edit the SVG files; paths are relative to the style file's folder.\n[icons.files]\n",
    );
    let folder = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for icon in ALL {
        let name = icon.name();
        let file = dir.join(format!("{name}.svg"));
        match style.icons.custom.get(name) {
            Some((src, _)) => {
                std::fs::copy(src, &file)
                    .with_context(|| format!("copy {} -> {}", src.display(), file.display()))?;
            }
            None => std::fs::write(&file, builtin_svg(icon, style.icons.width))
                .with_context(|| format!("write {}", file.display()))?,
        }
        toml.push_str(&format!("{name} = \"{folder}/{name}.svg\"\n"));
    }
    std::fs::write(dir.join("icons.toml"), toml).context("write icons.toml")?;
    Ok(ALL.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_names_match_the_style_files() {
        let names: Vec<&str> = ALL.iter().map(|i| i.name()).collect();
        assert_eq!(names.len(), gk_settings::style::ICON_NAMES.len());
        for n in gk_settings::style::ICON_NAMES {
            assert!(names.contains(&n), "{n}");
        }
    }

    /// Every built-in icon exported as SVG reads back with the same number of pieces drawn.
    #[test]
    fn exported_icons_read_back() {
        for icon in ALL {
            let svg = builtin_svg(icon, 2.0);
            let shape = gk_settings::icon_svg::parse(&svg)
                .unwrap_or_else(|e| panic!("{icon:?}: {e:#}\n{svg}"));
            assert!(!shape.items.is_empty(), "{icon:?}");
            assert_eq!(shape.view_box, [0.0, 0.0, 24.0, 24.0]);
        }
        let commit = gk_settings::icon_svg::parse(&builtin_svg(Icon::Commit, 2.0)).unwrap();
        assert!(
            commit.items.iter().any(|i| i.node),
            "the commit node is marked"
        );
    }
}
