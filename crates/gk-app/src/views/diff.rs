//! Reusable diff viewer: unified / side-by-side, word highlights, line
//! numbers, hunk actions and line selection for partial staging.
//! Used by the file status view (editable) and commit details (read-only).

use crate::app::Shared;
use gk_core::Session;
use gk_core::content::DiffTarget;

/// What actions the viewer offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// Commit history: no staging actions.
    ReadOnly,
    /// Index vs worktree: stage / discard hunks and lines.
    Unstaged,
    /// HEAD vs index: unstage hunks and lines.
    Staged,
}

/// Per-view state (scroll, selected lines, options). Keep one per place it is shown.
#[derive(Default)]
pub struct DiffView {}

impl DiffView {
    /// Renders the diff of `target`. `status_gen` invalidates worktree diffs
    /// when files change on disk.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        _session: &Session,
        target: &DiffTarget,
        _mode: DiffMode,
        _status_gen: u64,
        _shared: &mut Shared,
    ) {
        ui.label(format!("diff of {}", target.path()));
    }
}
