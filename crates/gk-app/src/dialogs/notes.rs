//! Commit notes (`git notes`): add / edit with a multi-line editor, remove.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use std::sync::atomic::Ordering;

/// Loads the current note of `id` ("" when none).
fn load_note(ctx: &Context, tab: &RepoTab, id: &str) -> Task<Result<String, String>> {
    let repo = tab.session.repo().clone();
    let id = id.to_string();
    Task::spawn(ctx, move || {
        repo.note(&id)
            .map(Option::unwrap_or_default)
            .map_err(|e| format!("{e:#}"))
    })
}

/// Queues saving `text` as the note of `id` (`None` removes it), then makes
/// the details reload notes.
fn run_notes(tab: &RepoTab, label: &str, id: String, text: Option<String>) {
    tab.session.run_op(label.to_string(), move |r, _, _| {
        let res = match &text {
            Some(t) => r.set_note(&id, t),
            None => r.remove_note(&id),
        };
        crate::views::details::NOTES_GEN.fetch_add(1, Ordering::SeqCst);
        res?;
        Ok(String::new())
    });
}

pub struct EditNote {
    id: String,
    text: String,
    original: Option<String>,
    load: Option<Task<Result<String, String>>>,
    label: RevLabel,
    /// The editor got the keyboard once (after the note loaded).
    focused: bool,
}

impl EditNote {
    pub fn new(id: String) -> Self {
        Self {
            id,
            text: String::new(),
            original: None,
            load: None,
            label: RevLabel::default(),
            focused: false,
        }
    }
}

impl TabDialog for EditNote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let load = self
            .load
            .get_or_insert_with(|| load_note(ctx, tab, &self.id));
        let mut failed = None;
        if self.original.is_none() {
            match load.poll() {
                Some(Ok(t)) => {
                    self.text = t.clone();
                    self.original = Some(t.clone());
                }
                Some(Err(e)) => failed = Some(e.clone()),
                None => {}
            }
        }
        let title = if self.original.as_deref().is_some_and(|o| !o.is_empty()) {
            "Edit note"
        } else {
            "Add note"
        };
        let (out, ()) = modal(ctx, "note", title, 560.0, "Save", |ui, ctl| {
            let l = self.label.get(tab, snap, &self.id).to_string();
            ui.label(RichText::new(if l.is_empty() { self.id.clone() } else { l }).monospace());
            ui.add_space(4.0);
            if let Some(e) = &failed {
                error(ui, e);
            }
            if self.original.is_none() {
                ctl.invalid();
                ui.spinner();
                return;
            }
            let r = ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .hint_text("Note text (empty removes the note)")
                    .desired_rows(8)
                    .desired_width(f32::INFINITY),
            );
            if !self.focused {
                r.request_focus();
                self.focused = true;
            }
            ctl.multiline(&r);
            hint(
                ui,
                "Ctrl+Enter saves. Notes are stored in refs/notes/commits.",
            );
            if self.original.as_deref() == Some(self.text.as_str()) {
                ctl.invalid();
            }
        });
        finish(out, || {
            let id = self.id.clone();
            if self.text.trim().is_empty() {
                run_notes(tab, "Remove note", id, None);
            } else {
                run_notes(tab, "Save note", id, Some(self.text.clone()));
            }
        })
    }
}

pub struct RemoveNote {
    id: String,
    load: Option<Task<Result<String, String>>>,
    label: RevLabel,
}

impl RemoveNote {
    pub fn new(id: String) -> Self {
        Self {
            id,
            load: None,
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for RemoveNote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let load = self
            .load
            .get_or_insert_with(|| load_note(ctx, tab, &self.id));
        let note = load.poll().cloned();
        let (out, ()) = modal(
            ctx,
            "remove-note",
            "Remove note",
            520.0,
            "Remove",
            |ui, ctl| {
                ctl.danger = true;
                let l = self.label.get(tab, snap, &self.id).to_string();
                ui.label(RichText::new(if l.is_empty() { self.id.clone() } else { l }).monospace());
                ui.add_space(4.0);
                match &note {
                    None => {
                        ctl.invalid();
                        ui.spinner();
                    }
                    Some(Err(e)) => {
                        ctl.invalid();
                        error(ui, e);
                    }
                    Some(Ok(n)) if n.is_empty() => {
                        ctl.invalid();
                        ui.label("This commit has no note.");
                    }
                    Some(Ok(n)) => {
                        ui.label("Remove this note?");
                        ui.label(RichText::new(n).italics());
                    }
                }
            },
        );
        finish(out, || {
            run_notes(tab, "Remove note", self.id.clone(), None);
        })
    }
}
