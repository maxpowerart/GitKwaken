//! Resizable panels that keep the size the user gave them.
//!
//! egui clamps a panel to the room it has and stores the clamped size, so a window that is
//! briefly small (minimized, restored from maximized, a narrow monitor) shrinks the file list,
//! sidebar or commit box for good. Here the size the user chose is kept separately (in
//! `RepoState::layout` where it should survive restarts) and put back once it fits again.

use egui::containers::panel::PanelState;

/// Which dimension of the panel is resizable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Left / right panels.
    Width,
    /// Top / bottom panels.
    Height,
}

fn along(r: egui::Rect, axis: Axis) -> f32 {
    match axis {
        Axis::Width => r.width(),
        Axis::Height => r.height(),
    }
}

/// Before showing the panel `id`: back to `desired` when it fits within `limit` (the most the
/// panel may take right now: its range maximum and the room available) and egui's stored size
/// was clamped below it. `desired <= 0` means "never set": the panel's default applies.
pub fn restore(ctx: &egui::Context, id: egui::Id, axis: Axis, desired: f32, limit: f32) {
    if desired <= 0.0 || desired > limit {
        return;
    }
    let Some(mut st) = PanelState::load(ctx, id) else {
        return;
    };
    if (along(st.outer_rect, axis) - desired).abs() <= 0.5 {
        return;
    }
    match axis {
        Axis::Width => st.outer_rect.set_width(desired),
        Axis::Height => st.outer_rect.set_height(desired),
    }
    ctx.data_mut(|d| d.insert_persisted(id, st));
}

/// After showing it: a size below `limit` is the user's choice (a drag, or the default on the
/// first frame) and becomes the new `desired`; a size pressed against `limit` is only the
/// window being small and is not remembered.
pub fn remember(shown: egui::Rect, axis: Axis, limit: f32, desired: &mut f32) {
    let size = along(shown, axis);
    if size > 0.0 && size < limit - 1.0 {
        *desired = size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_sizes_are_not_remembered() {
        let r = |w: f32| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 100.0));
        let mut desired = 300.0;
        // Window too small: the panel is pressed against the room it has.
        remember(r(180.0), Axis::Width, 180.0, &mut desired);
        assert_eq!(desired, 300.0);
        // The user dragged it with room to spare.
        remember(r(250.0), Axis::Width, 900.0, &mut desired);
        assert_eq!(desired, 250.0);
    }

    #[test]
    fn restore_puts_the_chosen_size_back_once_it_fits() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("panel");
        let st = PanelState {
            outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(180.0, 100.0)),
        };
        ctx.data_mut(|d| d.insert_persisted(id, st));
        // Not enough room yet: unchanged.
        restore(&ctx, id, Axis::Width, 300.0, 250.0);
        assert_eq!(PanelState::load(&ctx, id).unwrap().size().x, 180.0);
        restore(&ctx, id, Axis::Width, 300.0, 900.0);
        assert_eq!(PanelState::load(&ctx, id).unwrap().size().x, 300.0);
    }
}
