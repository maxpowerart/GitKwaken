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
    let git = tab.session.repo().git().clone();
    let id = id.to_string();
    Task::spawn(ctx, move || {
        let o = git
            .run_raw(&["notes", "show", id.as_str()], None)
            .map_err(|e| format!("{e:#}"))?;
        Ok(if o.code == Some(0) {
            o.stdout_str().trim_end().to_string()
        } else {
            String::new()
        })
    })
}

/// Queues `git notes <args>` (with `stdin`), then makes the details reload notes.
fn run_notes(tab: &RepoTab, label: &str, args: Vec<String>, stdin: Option<String>) {
    tab.session.run_op(label.to_string(), move |r, _, _| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let res = match &stdin {
            Some(s) => r.git().run_stdin(&a, s.as_bytes()),
            None => r.git().run(&a),
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
                let args = ["notes", "remove", "--ignore-missing", id.as_str()];
                run_notes(tab, "Remove note", args.map(String::from).to_vec(), None);
            } else {
                let args = ["notes", "add", "-f", "-F", "-", id.as_str()];
                let mut text = self.text.trim_end().to_string();
                text.push('\n');
                run_notes(
                    tab,
                    "Save note",
                    args.map(String::from).to_vec(),
                    Some(text),
                );
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
            let args = ["notes", "remove", "--ignore-missing", self.id.as_str()];
            run_notes(tab, "Remove note", args.map(String::from).to_vec(), None);
        })
    }
}
