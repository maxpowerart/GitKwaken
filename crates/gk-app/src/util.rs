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

/// Tab captions: the repository folder name, plus, for names open more than once, the shortest
/// tail of the parent path that tells them apart (`repo — work/a` / `repo — work/b`).
pub fn tab_labels(paths: &[&std::path::Path]) -> Vec<String> {
    let name = |p: &std::path::Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    // Parent folders, innermost first.
    let parents = |p: &std::path::Path| -> Vec<String> {
        p.parent()
            .map(|d| {
                d.components()
                    .rev()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                        std::path::Component::Prefix(s) => {
                            Some(s.as_os_str().to_string_lossy().into_owned())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let names: Vec<String> = paths.iter().map(|p| name(p)).collect();
    let mut labels = names.clone();
    for (i, n) in names.iter().enumerate() {
        let same: Vec<usize> = (0..paths.len())
            .filter(|&j| names[j].eq_ignore_ascii_case(n))
            .collect();
        if same.len() < 2 {
            continue;
        }
        let tails: Vec<Vec<String>> = same.iter().map(|&j| parents(paths[j])).collect();
        let longest = tails.iter().map(Vec::len).max().unwrap_or(0);
        let tail = |t: &[String], k: usize| -> String {
            let mut v: Vec<&str> = t.iter().take(k).map(String::as_str).collect();
            v.reverse();
            v.join("/")
        };
        let me = same.iter().position(|&j| j == i).unwrap_or(0);
        // Fewest parent folders that make every caption of this name unique.
        let k = (1..=longest.max(1))
            .find(|&k| {
                let all: Vec<String> = tails.iter().map(|t| tail(t, k).to_lowercase()).collect();
                all.iter()
                    .enumerate()
                    .all(|(a, x)| all.iter().skip(a + 1).all(|y| x != y))
            })
            .unwrap_or(longest.max(1));
        let suffix = tail(&tails[me], k);
        if !suffix.is_empty() {
            labels[i] = format!("{n} — {suffix}");
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_tab_names_get_the_differing_part_of_the_path() {
        use std::path::Path;
        let labels = |ps: &[&str]| {
            let v: Vec<&Path> = ps.iter().map(Path::new).collect();
            tab_labels(&v)
        };
        assert_eq!(labels(&["/w/a/repo", "/w/b/other"]), ["repo", "other"]);
        assert_eq!(
            labels(&["/w/a/repo", "/w/b/repo", "/w/c/x"]),
            ["repo — a", "repo — b", "x"]
        );
        // Same direct parent: one more level.
        assert_eq!(
            labels(&["/one/src/repo", "/two/src/repo"]),
            ["repo — one/src", "repo — two/src"]
        );
        // Different depths.
        assert_eq!(
            labels(&["/x/repo", "/y/x/repo"]),
            ["repo — x", "repo — y/x"]
        );
    }

    #[test]
    fn dates() {
        assert_eq!(format_time(0, 0, false), "1970-01-01 00:00");
        assert_eq!(
            format_time(1_760_659_200, 3 * 3600, false),
            "2025-10-17 03:00"
        );
    }
}
