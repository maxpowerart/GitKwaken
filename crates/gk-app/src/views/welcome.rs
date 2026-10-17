//! Shown when no repository is open: open, clone, init, bookmarks and recents.

use crate::app::{Action, Shared};
use crate::dialogs::GlobalDialog;
use egui::{Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use std::path::Path;

/// Width of the centered content block.
const CONTENT_W: f32 = 860.0;

pub fn show(ui: &mut Ui, shared: &mut Shared) {
    let full = ui.available_rect_before_wrap();
    let w = CONTENT_W.min(full.width() - 32.0).max(200.0);
    let block = Rect::from_min_max(
        pos2(full.center().x - w / 2.0, full.top()),
        pos2(full.center().x + w / 2.0, full.bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(block), |ui| content(ui, shared));
}

fn content(ui: &mut Ui, shared: &mut Shared) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(RichText::new("GitKwaken").heading().strong());
        ui.label(RichText::new("Fast git client").color(shared.palette.muted));
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 330.0).max(0.0) / 2.0);
            if ui.button("Open repository…").clicked() {
                shared.actions.push(Action::OpenRepoDialog);
            }
            if ui.button("Clone…").clicked() {
                shared
                    .actions
                    .push(Action::Global(GlobalDialog::clone_repo()));
            }
            if ui.button("Create new…").clicked() {
                shared
                    .actions
                    .push(Action::Global(GlobalDialog::init_repo()));
            }
        });
    });
    ui.add_space(28.0);
    let muted = shared.palette.muted;
    let mut open = None;
    let mut remove = None;
    let mut bookmark = None;
    ui.columns(2, |cols| {
        section_title(&mut cols[0], "Bookmarks", shared.settings.bookmarks.len());
        egui::ScrollArea::vertical()
            .id_salt("bookmarks")
            .auto_shrink([false, true])
            .show(&mut cols[0], |ui| {
                for b in &shared.settings.bookmarks {
                    let r = repo_item(ui, &b.name, &b.path, muted);
                    if r.clicked() {
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
                    placeholder(
                        ui,
                        "No bookmarks yet. Right-click a recent repository to add one.",
                        muted,
                    );
                }
            });
        section_title(&mut cols[1], "Recent", shared.settings.recent_repos.len());
        egui::ScrollArea::vertical()
            .id_salt("recent")
            .auto_shrink([false, true])
            .show(&mut cols[1], |ui| {
                for p in &shared.settings.recent_repos {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string());
                    let r = repo_item(ui, &name, p, muted);
                    if r.clicked() {
                        open = Some(p.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button("Bookmark").clicked() {
                            bookmark = Some(p.clone());
                            ui.close();
                        }
                    });
                }
                if shared.settings.recent_repos.is_empty() {
                    placeholder(ui, "Repositories you open show up here.", muted);
                }
            });
    });
    if let Some(p) = bookmark {
        shared
            .actions
            .push(Action::Global(GlobalDialog::bookmark(p)));
    } else if let Some(p) = open {
        shared.actions.push(Action::OpenRepo(p));
    }
    if let Some(p) = remove {
        shared.settings.remove_bookmark(&p);
    }
}

fn section_title(ui: &mut Ui, title: &str, count: usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        if count > 0 {
            ui.label(RichText::new(count.to_string()).weak());
        }
    });
    ui.separator();
}

fn placeholder(ui: &mut Ui, text: &str, muted: egui::Color32) {
    ui.add_space(4.0);
    ui.add(egui::Label::new(RichText::new(text).color(muted)).wrap());
}

/// A two-line clickable entry: repository name over its (truncated) path.
fn repo_item(ui: &mut Ui, name: &str, path: &Path, muted: egui::Color32) -> egui::Response {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let h = body.size + small.size + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let text_w = rect.width() - 16.0;
        let x = rect.left() + 8.0;
        let name_g = crate::views::history::table::text_trunc(
            p,
            name.to_string(),
            body.clone(),
            ui.visuals().strong_text_color(),
            text_w,
        );
        let path_g = crate::views::history::table::text_trunc(
            p,
            path.display().to_string(),
            small,
            muted,
            text_w,
        );
        let top = rect.top() + 5.0;
        let name_h = name_g.size().y;
        p.galley(pos2(x, top), name_g, muted);
        p.galley(pos2(x, top + name_h + 1.0), path_g, muted);
    }
    resp.on_hover_text(path.display().to_string())
}
