//! File status badges (M, A, D, R, ?, U…) in the File Status lists: a filled rounded square
//! in the status color with the letter centered on its outline. The letter is dark in the
//! dark theme and white in the light one, on every status; fills too dark or too light for it
//! are adjusted so it always reads.

use crate::contrast::luminance;
use egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, vec2};
use gk_status::Kind;

/// Letter color in the dark theme (white in the light one).
const LETTER: Color32 = Color32::from_rgb(0x16, 0x18, 0x1C);

/// Fill and letter colors of a badge in `color`: a dark letter in the dark theme, a white one
/// in the light theme. The fill is moved toward white (dark theme) or black (light theme),
/// keeping its hue, until the letter has a contrast of at least about 6:1; colors that already
/// allow it stay as they are.
pub fn badge_colors(color: Color32, dark: bool) -> (Color32, Color32) {
    let (target, letter): (f32, Color32) = if dark {
        (255.0, LETTER)
    } else {
        (0.0, Color32::WHITE)
    };
    let ok = |c: Color32| {
        if dark {
            luminance(c) >= 0.32
        } else {
            luminance(c) <= 0.12
        }
    };
    let mut c = color;
    let mut t = 0.0;
    while !ok(c) && t < 1.0 {
        t += 0.04;
        let mix = |v: u8| (v as f32 + (target - v as f32) * t).round() as u8;
        c = Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()));
    }
    (c, letter)
}

/// Paints the badge of `kind` in `color` with its left edge at `left_center` on a row whose
/// (opaque) background is `bg`; `k` is the text scale. A dimmed color (a pending file) fades
/// the badge into the row, its letter stays readable.
pub fn paint(
    p: &Painter,
    left_center: Pos2,
    kind: Kind,
    color: Color32,
    bg: Color32,
    dark: bool,
    k: f32,
) {
    let side = 14.0 * k;
    let r = Rect::from_center_size(left_center + vec2(side / 2.0, 0.0), vec2(side, side));
    // A dimmed color: work on the opaque one, dim the result alike.
    let a = color.a() as f32 / 255.0;
    let opaque = if a > 0.0 && a < 1.0 {
        let un = |v: u8| ((v as f32 / a).round().min(255.0)) as u8;
        Color32::from_rgb(un(color.r()), un(color.g()), un(color.b()))
    } else {
        color
    };
    let (fill, letter) = badge_colors(opaque, dark);
    let (fill, letter) = if a < 1.0 {
        let fill = crate::contrast::over(fill.gamma_multiply(a), bg);
        (fill, crate::contrast::text_on(letter, fill))
    } else {
        (fill, letter)
    };
    p.rect_filled(r, 3.5 * k, fill);
    // Centered on the letter's visible outline, not its line box; drawn twice for weight.
    let font = FontId::new(side * 0.72, FontFamily::Proportional);
    let g = p.layout_no_wrap(kind.code().to_string(), font, letter);
    let pos = r.center() - g.mesh_bounds.center().to_vec2();
    p.galley(pos + vec2(0.5, 0.0), g.clone(), letter);
    p.galley(pos, g, letter);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_letter_reads_on_every_status_color() {
        let contrast = crate::contrast::ratio;
        for style in ["Kwak", "Classic"] {
            let (s, _) = gk_settings::style::load_in(std::path::Path::new(""), style);
            for (v, dark) in [(&s.dark, true), (&s.light, false)] {
                let p = crate::theme::Palette::of(v, dark);
                for c in [
                    p.modified,
                    p.added_fg,
                    p.removed_fg,
                    p.conflict,
                    p.untracked,
                ] {
                    let (fill, letter) = badge_colors(c, dark);
                    assert!(contrast(fill, letter) >= 6.0, "{style} {c:?} -> {fill:?}");
                }
            }
        }
        // Light enough already: left alone.
        let orange = Color32::from_rgb(0xE8, 0xA7, 0x3D);
        assert_eq!(badge_colors(orange, true), (orange, LETTER));
    }
}
