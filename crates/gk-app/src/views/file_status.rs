//! File status view: staged / unstaged lists, diff with hunk and line
//! staging, and the commit box (agent: status).

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;

#[derive(Default)]
pub struct CommitBox {
    pub message: String,
    pub amend: bool,
}

#[derive(Default)]
pub struct StatusState {
    pub commit: CommitBox,
}

pub fn show(ui: &mut egui::Ui, _tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) {
    ui.label(format!("{} staged, {} unstaged", snap.status.staged.len(), snap.status.unstaged.len()));
}
