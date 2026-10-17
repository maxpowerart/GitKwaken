//! Tag dialogs: create, delete, push.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;

pub struct Tag {
    name: String,
    given: Option<String>,
    use_given: bool,
    message: String,
    sign: bool,
    push: bool,
    remote: String,
    label: RevLabel,
}

impl Tag {
    pub fn new(rev: Option<String>) -> Self {
        Self {
            name: String::new(),
            use_given: rev.is_some(),
            given: rev,
            message: String::new(),
            sign: false,
            push: false,
            remote: String::new(),
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for Tag {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let remotes = remote_names(snap);
        if self.remote.is_empty() {
            self.remote = default_remote(&remotes);
        }
        let (out, ()) = modal(ctx, "tag", "Add tag", 520.0, "Add tag", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Tag name:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(320.0));
                ctl.focus(&r);
            });
            ui.label("Commit:");
            ui.indent("tag-commit", |ui| {
                let head = describe_rev(tab, snap, "HEAD");
                ui.radio_value(
                    &mut self.use_given,
                    false,
                    format!("Working copy parent (HEAD)  {head}"),
                );
                if let Some(g) = &self.given {
                    let l = self.label.get(tab, snap, g).to_string();
                    ui.radio_value(
                        &mut self.use_given,
                        true,
                        format!(
                            "Specified commit: {}",
                            if l.is_empty() { g.clone() } else { l }
                        ),
                    );
                }
            });
            ui.label("Message (creates an annotated tag when not empty):");
            let r = ui.add(
                egui::TextEdit::multiline(&mut self.message)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ctl.multiline(&r);
            ui.checkbox(&mut self.sign, "Sign tag (GPG/SSH)");
            ui.add_enabled_ui(!remotes.is_empty(), |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.push, "Push tag to");
                    combo(ui, "tag-remote", &mut self.remote, &remotes, None, 200.0);
                });
            });
            let name = self.name.trim();
            if let Err(e) = check_branch_name(name) {
                if !name.is_empty() {
                    error(ui, e);
                }
                ctl.invalid();
            } else if snap.refs.tags.iter().any(|t| t.name == name) {
                error(ui, format!("A tag named '{name}' already exists"));
                ctl.invalid();
            }
            if self.sign && self.message.trim().is_empty() {
                error(ui, "A signed tag needs a message.");
                ctl.invalid();
            }
            if !self.use_given && snap.refs.head_id.is_none() {
                error(ui, "There is no commit to tag yet.");
                ctl.invalid();
            }
        });
        finish(out, || {
            let name = self.name.trim().to_string();
            let rev = if self.use_given {
                self.given.clone().unwrap_or_else(|| "HEAD".into())
            } else {
                "HEAD".into()
            };
            let msg = self.message.trim().to_string();
            let sign = self.sign;
            let push = (self.push && !self.remote.is_empty()).then(|| self.remote.clone());
            tab.session
                .run_op(format!("Tag {name}"), move |repo, cancel, p| {
                    repo.create_tag(&name, &rev, (!msg.is_empty()).then_some(msg.as_str()), sign)?;
                    if let Some(remote) = push {
                        repo.push_tag(&remote, &name, false, cancel, p)?;
                    }
                    Ok(String::new())
                });
        })
    }
}

pub struct DeleteTag {
    name: String,
    remote_too: bool,
    remote: String,
}

impl DeleteTag {
    pub fn new(name: String) -> Self {
        Self {
            name,
            remote_too: false,
            remote: String::new(),
        }
    }
}

impl TabDialog for DeleteTag {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let remotes = remote_names(snap);
        if self.remote.is_empty() {
            self.remote = default_remote(&remotes);
        }
        let (out, ()) = modal(
            ctx,
            "delete-tag",
            "Delete tag",
            460.0,
            "Delete",
            |ui, ctl| {
                ui.label("Delete this tag?");
                ui.label(RichText::new(&self.name).strong());
                ui.add_enabled_ui(!remotes.is_empty(), |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.remote_too, "Also delete it on");
                        combo(
                            ui,
                            "delete-tag-remote",
                            &mut self.remote,
                            &remotes,
                            None,
                            200.0,
                        );
                    });
                });
                if self.remote_too {
                    ui.colored_label(DANGER, "The tag is deleted on the server for everyone.");
                }
                ctl.danger = true;
            },
        );
        finish(out, || {
            let name = self.name.clone();
            let remote = (self.remote_too && !self.remote.is_empty()).then(|| self.remote.clone());
            tab.session
                .run_op(format!("Delete tag {name}"), move |repo, cancel, p| {
                    repo.delete_tag(&name)?;
                    if let Some(r) = remote {
                        repo.push_tag(&r, &name, true, cancel, p)?;
                    }
                    Ok(String::new())
                });
        })
    }
}

pub struct PushTag {
    name: String,
    remote: String,
}

impl PushTag {
    pub fn new(name: String) -> Self {
        Self {
            name,
            remote: String::new(),
        }
    }
}

impl TabDialog for PushTag {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let remotes = remote_names(snap);
        if self.remote.is_empty() {
            self.remote = default_remote(&remotes);
        }
        let (out, ()) = modal(ctx, "push-tag", "Push tag", 420.0, "Push", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Push tag");
                ui.label(RichText::new(&self.name).strong());
                ui.label("to");
                combo(
                    ui,
                    "push-tag-remote",
                    &mut self.remote,
                    &remotes,
                    None,
                    180.0,
                );
            });
            if remotes.is_empty() {
                error(ui, "This repository has no remotes.");
                ctl.invalid();
            }
        });
        finish(out, || {
            let (name, remote) = (self.name.clone(), self.remote.clone());
            tab.session
                .run_op(format!("Push tag {name}"), move |repo, cancel, p| {
                    repo.push_tag(&remote, &name, false, cancel, p)?;
                    Ok(String::new())
                });
        })
    }
}
