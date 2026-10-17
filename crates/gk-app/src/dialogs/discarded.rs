//! "Discarded changes": every discard is saved first (`gk_core::backup`);
//! this lists the saved discards and restores one into the working copy.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::{DiscardBackup, RestoreOutcome, Snapshot};

/// Backups listed at most.
const LIMIT: usize = 200;
/// Paths shown for the selected backup.
const SHOWN_PATHS: usize = 200;

pub struct Discarded {
    list: Option<Task<Result<Vec<DiscardBackup>, String>>>,
    sel: usize,
    overwrite: bool,
    op: Option<TrackedOp<RestoreOutcome>>,
    error: Option<String>,
}

impl Discarded {
    pub fn new() -> Self {
        Self {
            list: None,
            sel: 0,
            overwrite: false,
            op: None,
            error: None,
        }
    }
}

impl TabDialog for Discarded {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        if self.list.is_none() {
            let repo = tab.session.repo().clone();
            self.list = Some(Task::spawn(ctx, move || {
                repo.discard_backups(LIMIT).map_err(|e| format!("{e:#}"))
            }));
        }
        if let Some(op) = &mut self.op
            && let Some(r) = op.take()
        {
            self.op = None;
            match r {
                Ok(o) => {
                    let mut text = format!(
                        "Restored {} file{}",
                        o.restored,
                        if o.restored == 1 { "" } else { "s" }
                    );
                    if o.overwritten_backup.is_some() {
                        text.push_str("; the overwritten local changes were saved as a new entry");
                    }
                    tab.session
                        .push_message(gk_core::MessageKind::Info, format!("{text}."));
                    return false;
                }
                Err(e) => self.error = Some(e),
            }
        }
        let relative = shared.settings.history.relative_dates;
        let busy = self.op.is_some();
        let list = self.list.as_mut().and_then(|t| t.poll()).cloned();
        let (out, selected) = modal(
            ctx,
            "discarded",
            "Discarded changes",
            680.0,
            "Restore",
            |ui, ctl| -> Option<String> {
                ui.label(
                    "Every discard is saved first. Restore writes the saved files back into \
                     the working copy (the index is not changed).",
                );
                ui.add_space(4.0);
                let backups = match &list {
                    None => {
                        ctl.invalid();
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Reading saved discards…");
                        });
                        return None;
                    }
                    Some(Err(e)) => {
                        ctl.invalid();
                        error(ui, e);
                        return None;
                    }
                    Some(Ok(v)) if v.is_empty() => {
                        ctl.invalid();
                        ui.label(RichText::new("No discarded changes saved yet.").weak());
                        return None;
                    }
                    Some(Ok(v)) => v,
                };
                self.sel = self.sel.min(backups.len() - 1);
                egui::ScrollArea::vertical()
                    .id_salt("discarded-list")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (i, b) in backups.iter().enumerate() {
                            let when = crate::util::format_time(b.time, b.offset, relative);
                            let text = format!("{when}   {}", b.message);
                            if ui.selectable_label(self.sel == i, text).clicked() {
                                self.sel = i;
                                self.error = None;
                            }
                        }
                    });
                let b = &backups[self.sel];
                ui.add_space(4.0);
                ui.label(RichText::new("Files:").strong());
                egui::ScrollArea::vertical()
                    .id_salt("discarded-paths")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        let deleted = b.deleted.iter().map(|p| (p, true));
                        let all = b.files.iter().map(|p| (p, false)).chain(deleted);
                        for (p, del) in all.clone().take(SHOWN_PATHS) {
                            let t = if del {
                                format!("{p}  (was deleted)")
                            } else {
                                p.clone()
                            };
                            ui.label(RichText::new(t).monospace());
                        }
                        let n = all.count();
                        if n > SHOWN_PATHS {
                            ui.label(
                                RichText::new(format!("… and {} more", n - SHOWN_PATHS)).weak(),
                            );
                        }
                    });
                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.overwrite,
                    "Overwrite files changed since (their content is saved first)",
                );
                if let Some(e) = &self.error {
                    error(ui, e);
                }
                if busy {
                    ctl.invalid();
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Restoring…");
                    });
                }
                Some(b.id.clone())
            },
        );
        match out {
            Outcome::Cancel => false,
            Outcome::Ok => {
                if let Some(id) = selected {
                    self.error = None;
                    let overwrite = self.overwrite;
                    self.op = Some(TrackedOp::queue(
                        tab,
                        "Restore discarded changes",
                        move |repo, _, _| repo.restore_discarded(&id, overwrite),
                    ));
                }
                true
            }
            Outcome::None => true,
        }
    }
}
