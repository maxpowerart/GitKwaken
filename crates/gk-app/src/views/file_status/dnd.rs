//! Drag and drop between the staged and unstaged lists.
//!
//! A drag carries only paths ([`DragFiles`]), never row widgets, so the
//! dragged row may scroll out of the virtualized viewport while the drag is
//! in flight. Dropping on the other list emits the same `Toggle` event the
//! Stage / Unstage buttons use.

use super::list::Side;
use egui::{Id, LayerId, Order, Rect, Ui, vec2};

/// Drag-and-drop payload: files dragged out of one list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragFiles {
    /// Identifies the repository tab the drag started in.
    pub scope: Id,
    pub from: Side,
    pub paths: Vec<String>,
}

/// Whether the list `target` of `scope` accepts `payload`: only drops onto
/// the other list of the same tab count.
pub fn accepts(payload: &DragFiles, scope: Id, target: Side) -> bool {
    payload.scope == scope && payload.from != target && !payload.paths.is_empty()
}

/// Paths to toggle (stage or unstage, per `payload.from`) when `payload` is
/// dropped onto `target`. Paths that left the source list meanwhile
/// (`source_paths` is sorted) are dropped; `None` when nothing remains.
pub fn drop_paths(
    payload: &DragFiles,
    scope: Id,
    target: Side,
    source_paths: &[String],
) -> Option<Vec<String>> {
    if !accepts(payload, scope, target) {
        return None;
    }
    let paths: Vec<String> = payload
        .paths
        .iter()
        .filter(|p| source_paths.binary_search(p).is_ok())
        .cloned()
        .collect();
    (!paths.is_empty()).then_some(paths)
}

/// Text of the drag ghost.
pub fn ghost_label(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// What a drop onto `target` does.
pub fn drop_verb(target: Side) -> &'static str {
    match target {
        Side::Staged => "Stage",
        Side::Unstaged => "Unstage",
    }
}

/// Paints the drag ghost next to the pointer, above everything else.
pub fn paint_ghost(
    ui: &Ui,
    payload: &DragFiles,
    over: Option<Side>,
    palette: crate::theme::Palette,
) {
    let Some(pos) = ui.ctx().pointer_latest_pos() else {
        return;
    };
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, Id::new("gk-fs-dnd-ghost")));
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut text = ghost_label(payload.paths.len());
    if let Some(t) = over {
        text = format!("{} {text}", drop_verb(t));
    }
    let text_color = ui.visuals().strong_text_color();
    let g = painter.layout_no_wrap(text, font, text_color);
    let pad = vec2(8.0, 4.0);
    let rect = Rect::from_min_size(pos + vec2(14.0, 10.0), g.size() + pad * 2.0);
    let (fill, stroke) = if over.is_some() {
        (ui.visuals().window_fill, palette.branch)
    } else {
        (
            ui.visuals().window_fill.gamma_multiply(0.9),
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        )
    };
    painter.rect(rect, 4.0, fill, (1.0, stroke), egui::StrokeKind::Inside);
    painter.galley(rect.min + pad, g, text_color);
}

/// Highlights a list that accepts the hovering payload.
pub fn paint_target(ui: &Ui, zone: Rect, palette: crate::theme::Palette) {
    let p = ui.painter();
    p.rect(
        zone.shrink(1.0),
        4.0,
        palette.selection.gamma_multiply(0.18),
        (2.0, palette.branch),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn payload(from: Side, paths: &[&str]) -> DragFiles {
        DragFiles {
            scope: Id::new("repo-a"),
            from,
            paths: strs(paths),
        }
    }

    #[test]
    fn drops_only_onto_the_other_list_of_the_same_tab() {
        let scope = Id::new("repo-a");
        let pl = payload(Side::Unstaged, &["a", "b"]);
        assert!(accepts(&pl, scope, Side::Staged));
        assert!(!accepts(&pl, scope, Side::Unstaged), "same list");
        assert!(!accepts(&pl, Id::new("repo-b"), Side::Staged), "other tab");
        assert!(!accepts(&payload(Side::Unstaged, &[]), scope, Side::Staged));
        let back = payload(Side::Staged, &["a"]);
        assert!(accepts(&back, scope, Side::Unstaged));
        assert!(!accepts(&back, scope, Side::Staged));
    }

    #[test]
    fn drop_keeps_paths_still_in_the_source_list() {
        let scope = Id::new("repo-a");
        let pl = payload(Side::Unstaged, &["a", "gone", "c"]);
        let source = strs(&["a", "b", "c"]);
        assert_eq!(
            drop_paths(&pl, scope, Side::Staged, &source),
            Some(strs(&["a", "c"]))
        );
        assert_eq!(drop_paths(&pl, scope, Side::Unstaged, &source), None);
        assert_eq!(drop_paths(&pl, scope, Side::Staged, &strs(&["x"])), None);
    }

    #[test]
    fn ghost_text() {
        assert_eq!(ghost_label(1), "1 file");
        assert_eq!(ghost_label(3), "3 files");
        assert_eq!(drop_verb(Side::Staged), "Stage");
        assert_eq!(drop_verb(Side::Unstaged), "Unstage");
    }
}
