//! Search commits by message, author or id (agent: history).

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;

#[derive(Default)]
pub struct SearchState {}

pub fn show(ui: &mut egui::Ui, _tab: &mut RepoTab, _snap: &Snapshot, _shared: &mut Shared) {
    ui.label("search");
}
