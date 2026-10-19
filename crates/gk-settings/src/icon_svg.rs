//! Custom icons for styles: a small SVG reader for line icons.
//!
//! Supported: `<svg>` with `viewBox` (or `width` / `height`), `<g>`, `<path>` (all commands,
//! arcs included), `<line>`, `<polyline>`, `<polygon>`, `<circle>`, `<ellipse>`, `<rect>`
//! (with `rx`), and the presentation attributes `fill`, `stroke` and `stroke-width` (inherited
//! from parents, as in Lucide, Tabler and Feather icons). Colors are not taken from the file:
//! strokes and fills get the style's icon color, and elements with `class="node"` (or inside a
//! group with it) the node color. Fills are drawn as convex shapes (circles and simple
//! polygons look right; concave filled outlines do not). `transform` is not supported.

use anyhow::{Context, Result, anyhow, bail};

/// One drawn piece of an icon, in view-box coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// A polyline (curves flattened); `closed` joins the last point to the first.
    Poly {
        points: Vec<[f32; 2]>,
        closed: bool,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
    },
}

/// How a [`Part`] is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub part: Part,
    /// Stroke width in view-box units; `None` when not stroked.
    pub stroke: Option<f32>,
    pub fill: bool,
    /// In the node color (`class="node"`).
    pub node: bool,
}

/// A parsed icon.
#[derive(Debug, Clone, PartialEq)]
pub struct IconShape {
    /// min x, min y, width, height.
    pub view_box: [f32; 4],
    pub items: Vec<Item>,
}

#[derive(Clone)]
struct Paint {
    stroke: bool,
    fill: bool,
    width: f32,
    node: bool,
}

fn num(n: roxmltree::Node, name: &str) -> f32 {
    n.attribute(name)
        .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        .unwrap_or(0.0)
}

fn points_attr(s: &str) -> Vec<[f32; 2]> {
    let v: Vec<f32> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect();
    v.as_chunks::<2>().0.to_vec()
}

fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<[f32; 2]> {
    (0..48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            [cx + rx * a.cos(), cy + ry * a.sin()]
        })
        .collect()
}

fn rect_points(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Vec<[f32; 2]> {
    let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
    if rx <= 0.0 || ry <= 0.0 {
        return vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
    }
    let mut pts = Vec::new();
    // Corners clockwise from the top right, each a quarter ellipse.
    let corners = [
        (x + w - rx, y + ry, -90.0f32),
        (x + w - rx, y + h - ry, 0.0),
        (x + rx, y + h - ry, 90.0),
        (x + rx, y + ry, 180.0),
    ];
    for (cx, cy, start) in corners {
        for i in 0..=6 {
            let a = (start + 90.0 * i as f32 / 6.0).to_radians();
            pts.push([cx + rx * a.cos(), cy + ry * a.sin()]);
        }
    }
    pts
}

/// Path data to polylines (each subpath one, curves flattened).
fn path_polys(d: &str) -> Result<Vec<(Vec<[f32; 2]>, bool)>> {
    use svgtypes::{SimplePathSegment as S, SimplifyingPathParser};
    let mut out: Vec<(Vec<[f32; 2]>, bool)> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut last = [0.0f32, 0.0];
    let flush = |cur: &mut Vec<[f32; 2]>, out: &mut Vec<(Vec<[f32; 2]>, bool)>, closed: bool| {
        if cur.len() >= 2 || (closed && !cur.is_empty()) {
            out.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };
    for seg in SimplifyingPathParser::from(d) {
        match seg.map_err(|e| anyhow!("path data: {e}"))? {
            S::MoveTo { x, y } => {
                flush(&mut cur, &mut out, false);
                last = [x as f32, y as f32];
                cur.push(last);
            }
            S::LineTo { x, y } => {
                last = [x as f32, y as f32];
                cur.push(last);
            }
            S::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let (p0, p1, p2, p3) = (
                    last,
                    [x1 as f32, y1 as f32],
                    [x2 as f32, y2 as f32],
                    [x as f32, y as f32],
                );
                for i in 1..=16 {
                    let t = i as f32 / 16.0;
                    let u = 1.0 - t;
                    let c = |k: usize| {
                        u * u * u * p0[k]
                            + 3.0 * u * u * t * p1[k]
                            + 3.0 * u * t * t * p2[k]
                            + t * t * t * p3[k]
                    };
                    cur.push([c(0), c(1)]);
                }
                last = p3;
            }
            S::Quadratic { x1, y1, x, y } => {
                let (p0, p1, p2) = (last, [x1 as f32, y1 as f32], [x as f32, y as f32]);
                for i in 1..=12 {
                    let t = i as f32 / 12.0;
                    let u = 1.0 - t;
                    let c = |k: usize| u * u * p0[k] + 2.0 * u * t * p1[k] + t * t * p2[k];
                    cur.push([c(0), c(1)]);
                }
                last = p2;
            }
            S::ClosePath => {
                let start = cur.first().copied();
                flush(&mut cur, &mut out, true);
                if let Some(s) = start {
                    last = s;
                }
            }
        }
    }
    flush(&mut cur, &mut out, false);
    Ok(out)
}

fn walk(n: roxmltree::Node, inherited: &Paint, items: &mut Vec<Item>) -> Result<()> {
    if !n.is_element() {
        return Ok(());
    }
    if n.attribute("transform").is_some() {
        bail!("<{}>: `transform` is not supported", n.tag_name().name());
    }
    let mut paint = inherited.clone();
    if let Some(v) = n.attribute("stroke") {
        paint.stroke = v.trim() != "none";
    }
    if let Some(v) = n.attribute("fill") {
        paint.fill = v.trim() != "none";
    }
    if let Some(v) = n.attribute("stroke-width") {
        paint.width = v
            .trim()
            .trim_end_matches("px")
            .parse()
            .unwrap_or(paint.width);
    }
    if n.attribute("class")
        .is_some_and(|c| c.split_whitespace().any(|c| c == "node"))
    {
        paint.node = true;
    }
    let item = |part: Part, closed_shape: bool| Item {
        part,
        stroke: paint.stroke.then_some(paint.width),
        fill: paint.fill && closed_shape,
        node: paint.node,
    };
    match n.tag_name().name() {
        "svg" | "g" => {
            for c in n.children() {
                walk(c, &paint, items)?;
            }
        }
        "path" => {
            let d = n.attribute("d").unwrap_or("");
            for (points, closed) in path_polys(d)? {
                items.push(item(Part::Poly { points, closed }, closed));
            }
        }
        "line" => items.push(item(
            Part::Poly {
                points: vec![[num(n, "x1"), num(n, "y1")], [num(n, "x2"), num(n, "y2")]],
                closed: false,
            },
            false,
        )),
        "polyline" | "polygon" => {
            let closed = n.tag_name().name() == "polygon";
            let points = points_attr(n.attribute("points").unwrap_or(""));
            items.push(item(Part::Poly { points, closed }, closed));
        }
        "circle" => items.push(item(
            Part::Circle {
                center: [num(n, "cx"), num(n, "cy")],
                radius: num(n, "r"),
            },
            true,
        )),
        "ellipse" => items.push(item(
            Part::Poly {
                points: ellipse_points(num(n, "cx"), num(n, "cy"), num(n, "rx"), num(n, "ry")),
                closed: true,
            },
            true,
        )),
        "rect" => {
            let (rx, ry) = match (n.attribute("rx"), n.attribute("ry")) {
                (Some(_), None) => (num(n, "rx"), num(n, "rx")),
                (None, Some(_)) => (num(n, "ry"), num(n, "ry")),
                _ => (num(n, "rx"), num(n, "ry")),
            };
            let points = rect_points(
                num(n, "x"),
                num(n, "y"),
                num(n, "width"),
                num(n, "height"),
                rx,
                ry,
            );
            items.push(item(
                Part::Poly {
                    points,
                    closed: true,
                },
                true,
            ));
        }
        // Metadata, titles, definitions: nothing to draw.
        _ => {}
    }
    Ok(())
}

/// Parses an SVG document into an icon.
pub fn parse(src: &str) -> Result<IconShape> {
    let doc = roxmltree::Document::parse(src).map_err(|e| anyhow!("not valid SVG/XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        bail!(
            "the root element is <{}>, not <svg>",
            root.tag_name().name()
        );
    }
    let view_box = match root.attribute("viewBox") {
        Some(v) => {
            let n = points_attr(v);
            match n.as_slice() {
                [[x, y], [w, h]] if *w > 0.0 && *h > 0.0 => [*x, *y, *w, *h],
                _ => bail!("invalid viewBox {v:?}"),
            }
        }
        None => {
            let (w, h) = (num(root, "width"), num(root, "height"));
            if w > 0.0 && h > 0.0 {
                [0.0, 0.0, w, h]
            } else {
                [0.0, 0.0, 24.0, 24.0]
            }
        }
    };
    // SVG defaults: filled black, no stroke, width 1.
    let base = Paint {
        stroke: false,
        fill: true,
        width: 1.0,
        node: false,
    };
    let mut items = Vec::new();
    walk(root, &base, &mut items)?;
    if items.is_empty() {
        bail!("nothing to draw");
    }
    Ok(IconShape { view_box, items })
}

/// Reads and parses an SVG file.
pub fn load(path: &std::path::Path) -> Result<IconShape> {
    let src = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&src).with_context(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lucide's "git-commit-horizontal" icon.
    const LUCIDE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><line x1="3" x2="9" y1="12" y2="12"/><line x1="15" x2="21" y1="12" y2="12"/></svg>"#;

    #[test]
    fn lucide_style_icons_parse() {
        let s = parse(LUCIDE).unwrap();
        assert_eq!(s.view_box, [0.0, 0.0, 24.0, 24.0]);
        assert_eq!(s.items.len(), 3);
        assert!(s.items.iter().all(|i| i.stroke == Some(2.0) && !i.fill));
        assert_eq!(
            s.items[0].part,
            Part::Circle {
                center: [12.0, 12.0],
                radius: 3.0
            }
        );
    }

    #[test]
    fn paths_nodes_and_errors() {
        let s = parse(
            r#"<svg viewBox="0 0 24 24" stroke="currentColor" fill="none" stroke-width="1.5">
                 <path d="M4 12 L 9 17 L 20 6"/>
                 <path d="M5 5 h 4 v 4 z"/>
                 <path d="M3 20 A 4 4 0 0 1 11 20"/>
                 <circle class="node" cx="20" cy="6" r="2.5" fill="currentColor" stroke="none"/>
               </svg>"#,
        )
        .unwrap();
        assert_eq!(s.items.len(), 4);
        match &s.items[0].part {
            Part::Poly { points, closed } => {
                assert_eq!(points, &vec![[4.0, 12.0], [9.0, 17.0], [20.0, 6.0]]);
                assert!(!closed);
            }
            p => panic!("{p:?}"),
        }
        assert!(
            matches!(&s.items[1].part, Part::Poly { closed: true, points } if points.len() == 3)
        );
        // The arc is flattened into many points ending at (11, 20).
        match &s.items[2].part {
            Part::Poly { points, .. } => {
                assert!(points.len() > 5);
                let end = points.last().unwrap();
                assert!((end[0] - 11.0).abs() < 0.01 && (end[1] - 20.0).abs() < 0.01);
            }
            p => panic!("{p:?}"),
        }
        let node = &s.items[3];
        assert!(node.node && node.fill && node.stroke.is_none());
        assert_eq!(s.items[0].stroke, Some(1.5));

        assert!(parse("<html/>").is_err());
        assert!(parse("<svg><g transform=\"scale(2)\"/></svg>").is_err());
        assert!(parse("<svg viewBox=\"0 0 24 24\"></svg>").is_err(), "empty");
        assert!(parse("not xml").is_err());
    }
}
