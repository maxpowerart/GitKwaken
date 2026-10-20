//! Text contrast: WCAG luminance and contrast ratio, [`readable`] to fix a text color against
//! the background it is painted on, and [`check`], which finds hard-to-read text in a painted
//! frame (the tests run every view through it in both themes and every built-in style).

use egui::epaint::{ClippedShape, Shape};
use egui::{Color32, Pos2, Rect};

/// Normal text: WCAG AA.
pub const TEXT: f32 = 4.5;
/// Secondary text (hints, dates, counters, disabled items) and status letters: WCAG AA for
/// large text and UI components. Nothing on screen may go below it.
pub const SECONDARY: f32 = 3.0;
/// Head room the helpers keep above a minimum: the renderer shifts glyph colors slightly.
const MARGIN: f32 = 0.1;

/// WCAG relative luminance of an opaque color.
pub fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio (1 to 21) of two opaque colors.
pub fn ratio(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `top` (premultiplied, possibly translucent) painted over the opaque `under`.
pub fn over(top: Color32, under: Color32) -> Color32 {
    let k = (255 - top.a()) as u32;
    let mix = |t: u8, u: u8| (t as u32 + (u as u32 * k + 127) / 255).min(255) as u8;
    Color32::from_rgb(
        mix(top.r(), under.r()),
        mix(top.g(), under.g()),
        mix(top.b(), under.b()),
    )
}

/// `fg` painted on `bg` (opaque), moved toward black or white, whichever reaches it sooner
/// while keeping the hue, until the contrast is at least `min`; unchanged when it already is.
/// The result is opaque.
pub fn readable(fg: Color32, bg: Color32, min: f32) -> Color32 {
    let fg = over(fg, bg);
    if ratio(fg, bg) >= min {
        return fg;
    }
    let toward = |target: f32| {
        let mut t = 0.0;
        loop {
            t += 0.05_f32;
            let mix = |v: u8| (v as f32 + (target - v as f32) * t.min(1.0)).round() as u8;
            let c = Color32::from_rgb(mix(fg.r()), mix(fg.g()), mix(fg.b()));
            if ratio(c, bg) >= min || t >= 1.0 {
                return (t, c);
            }
        }
    };
    let (dark, light) = (toward(0.0), toward(255.0));
    let ok = |c: Color32| ratio(c, bg) >= min;
    match (ok(dark.1), ok(light.1)) {
        (true, true) => {
            if dark.0 <= light.0 {
                dark.1
            } else {
                light.1
            }
        }
        (true, false) => dark.1,
        (false, true) => light.1,
        (false, false) => {
            if ratio(dark.1, bg) >= ratio(light.1, bg) {
                dark.1
            } else {
                light.1
            }
        }
    }
}

/// Near-black or white, whichever reads better on `bg`: the text of a filled badge or chip.
pub fn ink_on(bg: Color32) -> Color32 {
    let dark = Color32::from_rgb(0x16, 0x18, 0x1C);
    if ratio(Color32::WHITE, bg) >= ratio(dark, bg) {
        Color32::WHITE
    } else {
        dark
    }
}

/// `fill` moved away from `ink` (darker under light text, lighter under dark text), keeping
/// its hue, until `ink` on it has a contrast of at least `min`: selection bars, chips and
/// badges keep their color and stay readable.
pub fn fill_for(fill: Color32, ink: Color32, min: f32) -> Color32 {
    let fill = over(fill, Color32::BLACK);
    let target = if luminance(ink) > 0.5 { 0.0 } else { 255.0 };
    let mut t = 0.0_f32;
    let mut c = fill;
    while ratio(ink, c) < min && t < 1.0 {
        t += 0.02;
        let mix = |v: u8| (v as f32 + (target - v as f32) * t.min(1.0)).round() as u8;
        c = Color32::from_rgb(mix(fill.r()), mix(fill.g()), mix(fill.b()));
    }
    c
}

/// [`readable`] for normal text.
pub fn text_on(fg: Color32, bg: Color32) -> Color32 {
    readable(fg, bg, TEXT + MARGIN)
}

/// [`readable`] for secondary text and status colors.
pub fn secondary_on(fg: Color32, bg: Color32) -> Color32 {
    readable(fg, bg, SECONDARY + MARGIN)
}

/// Text in a painted frame whose contrast with what is under it is below the minimum.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Violation {
    pub text: String,
    pub fg: Color32,
    pub bg: Color32,
    pub ratio: f32,
    pub at: Pos2,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = |c: Color32| format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
        write!(
            f,
            "{:?} {} on {} = {:.2}:1 at ({:.0}, {:.0})",
            self.text,
            hex(self.fg),
            hex(self.bg),
            self.ratio,
            self.at.x,
            self.at.y
        )
    }
}

/// Every piece of text in `shapes` (paint order, as egui hands them to the renderer) with a
/// contrast below `min` against the filled rectangles painted under it, on top of `base`.
/// Text is checked per color section, at the center of the section's first glyph. Text
/// painted fully transparent or clipped away is skipped.
#[cfg_attr(not(test), allow(dead_code))]
pub fn check(shapes: &[ClippedShape], base: Color32, min: f32) -> Vec<Violation> {
    let mut fills: Vec<(Rect, Rect, Color32)> = Vec::new();
    let mut out = Vec::new();
    let mut visit = |clip: Rect, shape: &Shape, out: &mut Vec<Violation>| {
        walk(clip, shape, &mut fills, base, min, out);
    };
    for s in shapes {
        visit(s.clip_rect, &s.shape, &mut out);
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
fn walk(
    clip: Rect,
    shape: &Shape,
    fills: &mut Vec<(Rect, Rect, Color32)>,
    base: Color32,
    min: f32,
    out: &mut Vec<Violation>,
) {
    match shape {
        Shape::Vec(v) => {
            for s in v {
                walk(clip, s, fills, base, min, out);
            }
        }
        // Additive fills (alpha 0, color above 0: egui's stripes) count too.
        Shape::Rect(r) if r.fill != Color32::TRANSPARENT => fills.push((r.rect, clip, r.fill)),
        Shape::Text(t) => {
            let bg_at = |p: Pos2| {
                fills
                    .iter()
                    .filter(|(r, c, _)| r.contains(p) && c.contains(p))
                    .fold(base, |under, (_, _, f)| over(*f, under))
            };
            for row in &t.galley.rows {
                // One report per color and background in a row.
                let mut seen = Vec::new();
                for glyph in &row.glyphs {
                    if glyph.chr.is_whitespace() {
                        continue;
                    }
                    let Some(v) = row.visuals.mesh.vertices.get(glyph.first_vertex as usize) else {
                        continue;
                    };
                    let at = t.pos + row.pos.to_vec2() + glyph.pos.to_vec2();
                    let at = Pos2::new(
                        at.x + glyph.advance_width / 2.0,
                        at.y - glyph.font_ascent / 3.0,
                    );
                    if !clip.contains(at) {
                        continue;
                    }
                    let mut fg = t.override_text_color.unwrap_or(v.color);
                    if fg == Color32::PLACEHOLDER {
                        fg = t.fallback_color;
                    }
                    if t.opacity_factor < 1.0 {
                        fg = fg.gamma_multiply(t.opacity_factor);
                    }
                    if fg.a() == 0 {
                        continue;
                    }
                    let bg = bg_at(at);
                    if seen.contains(&(fg, bg)) {
                        continue;
                    }
                    seen.push((fg, bg));
                    let shown = over(fg, bg);
                    let r = ratio(shown, bg);
                    if r < min {
                        out.push(Violation {
                            text: row.glyphs.iter().map(|g| g.chr).collect(),
                            fg: shown,
                            bg,
                            ratio: r,
                            at,
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_and_fixes() {
        assert!((ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
        // Half-transparent white over black: mid gray.
        let g = over(Color32::from_white_alpha(128), Color32::BLACK);
        assert!((g.r() as i32 - 128).abs() <= 1);

        let teal = Color32::from_rgb(0x1F, 0x98, 0x8D);
        let green = Color32::from_rgb(0x4E, 0xC9, 0x6A);
        assert!(ratio(green, teal) < 2.0);
        let fixed = text_on(green, teal);
        assert!(ratio(fixed, teal) >= TEXT, "{fixed:?}");
        // Already fine: unchanged.
        assert_eq!(text_on(Color32::WHITE, Color32::BLACK), Color32::WHITE);
        // Light background: goes dark.
        let on_light = text_on(Color32::from_rgb(0x9B, 0xD8, 0x9B), Color32::WHITE);
        assert!(luminance(on_light) < 0.2);
    }
}
