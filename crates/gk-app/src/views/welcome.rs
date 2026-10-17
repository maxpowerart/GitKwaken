//! Shown when no repository is open: open, clone, init, bookmarks and recents.

use crate::app::{Action, Shared};
use crate::dialogs::GlobalDialog;

pub fn show(ui: &mut egui::Ui, shared: &mut Shared) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.heading("GitKwaken");
        ui.label(egui::RichText::new("Fast git client").color(shared.palette.muted));
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 330.0).max(0.0) / 2.0);
            if ui.button("Open repository…").clicked() {
                shared.actions.push(Action::OpenRepoDialog);
            }
            if ui.button("Clone…").clicked() {
                shared.actions.push(Action::Global(GlobalDialog::clone_repo()));
            }
            if ui.button("Create new…").clicked() {
                shared.actions.push(Action::Global(GlobalDialog::init_repo()));
            }
        });
    });
    ui.add_space(24.0);
    ui.columns(2, |cols| {
        cols[0].strong("Bookmarks");
        let mut open = None;
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt("bookmarks").show(&mut cols[0], |ui| {
            for b in &shared.settings.bookmarks {
                let r = ui.selectable_label(false, format!("{}   {}", b.name, b.path.display()));
                if r.double_clicked() || r.clicked() {
                    open = Some(b.path.clone());
                }
                r.context_menu(|ui| {
                    if ui.button("Remove bookmark").clicked() {
                        remove = Some(b.path.clone());
                        ui.close();
                    }
                });
            }
            if shared.settings.bookmarks.is_empty() {
                ui.label(egui::RichText::new("No bookmarks yet.").color(shared.palette.muted));
            }
        });
        cols[1].strong("Recent");
        egui::ScrollArea::vertical().id_salt("recent").show(&mut cols[1], |ui| {
            for p in &shared.settings.recent_repos {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let r = ui.selectable_label(false, format!("{name}   {}", p.display()));
                if r.clicked() {
                    open = Some(p.clone());
                }
                r.context_menu(|ui| {
                    if ui.button("Bookmark").clicked() {
                        let p2 = p.clone();
                        open = None;
                        shared.actions.push(Action::Global(GlobalDialog::bookmark(p2)));
                        ui.close();
                    }
                });
            }
        });
        if let Some(p) = open {
            shared.actions.push(Action::OpenRepo(p));
        }
        if let Some(p) = remove {
            shared.settings.remove_bookmark(&p);
        }
    });
}
