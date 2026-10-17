//! Sidebar: workspace views, branches, remotes, tags, stashes, submodules (agent: sidebar).

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;

#[derive(Default)]
pub struct SidebarState {}

pub fn show(ui: &mut egui::Ui, _tab: &mut RepoTab, _snap: &Snapshot, _shared: &mut Shared) {
    ui.label("sidebar");
}
