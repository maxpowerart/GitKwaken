//! Bottom bar: branch, ahead/behind, running operation, background activity, messages.

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::{Head, MessageKind, Snapshot};

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    // Error messages pop up above the status bar until dismissed.
    if !snap.messages.is_empty() {
        egui::Panel::bottom(egui::Id::new(("messages", &tab.path))).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .show(ui, |ui| {
                    for (i, m) in snap.messages.iter().enumerate().rev() {
                        let color = match m.kind {
                            MessageKind::Error => shared.palette.removed_fg,
                            MessageKind::Info => ui.visuals().text_color(),
                        };
                        // Info messages with details (e.g. commit hook output)
                        // collapse to their first line.
                        match (m.kind == MessageKind::Info, m.text.split_once('\n')) {
                            (true, Some((first, rest))) if !rest.trim().is_empty() => {
                                egui::CollapsingHeader::new(
                                    egui::RichText::new(first).color(color),
                                )
                                .id_salt(("gk-message", i, m.at))
                                .default_open(rest.lines().count() <= 3)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(rest).monospace().small());
                                });
                            }
                            _ => {
                                ui.colored_label(color, &m.text);
                            }
                        }
                    }
                });
            if ui.small_button("Dismiss").clicked() {
                tab.session.dismiss_messages();
            }
        });
    }
    egui::Panel::bottom(egui::Id::new(("statusbar", &tab.path)))
        .exact_size((24.0 * crate::theme::text_scale(ui.style())).round())
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let refs = &snap.refs;
                match &refs.head {
                    Head::Branch(b) => {
                        crate::icons::show(
                            ui,
                            crate::icons::Icon::Branch,
                            1.0,
                            crate::icons::Colors::theme(),
                        );
                        ui.label(egui::RichText::new(b.to_string()).strong());
                        if let Some(br) = refs.current_branch()
                            && let Some(u) = &br.upstream
                        {
                            let t = if br.upstream_gone {
                                format!("{u} (gone)")
                            } else {
                                format!("↑ {}  ↓ {}   {u}", br.ahead, br.behind)
                            };
                            ui.label(egui::RichText::new(t).color(shared.palette.muted));
                        }
                    }
                    Head::Detached(id) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "HEAD detached at {}",
                                crate::util::short_id(id)
                            ))
                            .strong(),
                        );
                    }
                    Head::Unborn(b) => {
                        crate::icons::show(
                            ui,
                            crate::icons::Icon::Branch,
                            1.0,
                            crate::icons::Colors::theme(),
                        );
                        ui.label(format!("{b} (no commits yet)"));
                    }
                    Head::Unknown => {}
                }
                if refs.state != gk_core::RepoState::Clean {
                    ui.label(
                        egui::RichText::new(refs.state.label())
                            .strong()
                            .color(shared.palette.conflict),
                    );
                }
                ui.separator();
                if let Some(op) = &snap.op {
                    ui.spinner();
                    ui.label(format!("{}…", op.label));
                    if !op.last_line.is_empty() {
                        ui.label(egui::RichText::new(&op.last_line).color(shared.palette.muted));
                    }
                    if ui.small_button("Cancel").clicked() {
                        tab.session.cancel_op();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if snap.status_loading || snap.history_loading {
                        ui.spinner();
                    }
                    if snap.status.fsmonitor {
                        ui.label(egui::RichText::new("· fsmonitor").color(shared.palette.muted))
                            .on_hover_text(
                                "The last full status scan was answered by git's file system \
                                 monitor daemon (core.fsmonitor). See More ▸ Optimize repository.",
                            );
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "{} commits · status {} ms",
                            snap.history.len(),
                            snap.status.took.as_millis()
                        ))
                        .color(shared.palette.muted),
                    );
                });
            });
        });
}
