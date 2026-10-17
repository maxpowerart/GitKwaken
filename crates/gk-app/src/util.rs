//! Small formatting and widget helpers shared by the views.

use egui::{Color32, RichText, Ui};

/// "2026-10-17 14:03" in the commit's own time zone.
pub fn format_time(secs: i64, offset: i32, fmt_relative: bool) -> String {
    if fmt_relative {
        return relative(secs);
    }
    let t = secs + offset as i64;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

pub fn relative(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let d = (now - secs).max(0);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86_400 => format!("{} h ago", d / 3600),
        86_400..2_592_000 => format!("{} days ago", d / 86_400),
        2_592_000..31_536_000 => format!("{} months ago", d / 2_592_000),
        _ => format!("{} years ago", d / 31_536_000),
    }
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Small rounded label, as used for branch/tag decorations.
pub fn chip(ui: &mut Ui, text: &str, color: Color32, bold: bool) -> egui::Response {
    let mut rt = RichText::new(text)
        .color(crate::views::history::table::chip_text_color(color))
        .size(ui.style().text_styles[&egui::TextStyle::Small].size);
    if bold {
        rt = rt.strong();
    }
    egui::Frame::new()
        .fill(color)
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(4, 0))
        .show(ui, |ui| ui.label(rt))
        .response
}

pub fn status_color(p: &crate::theme::Palette, kind: gk_status::Kind) -> Color32 {
    use gk_status::Kind::*;
    match kind {
        Added | IntentToAdd => p.added_fg,
        Deleted => p.removed_fg,
        Untracked => p.untracked,
        Conflict(_) => p.conflict,
        _ => p.modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_time(0, 0, false), "1970-01-01 00:00");
        assert_eq!(
            format_time(1_760_659_200, 3 * 3600, false),
            "2025-10-17 03:00"
        );
    }
}
