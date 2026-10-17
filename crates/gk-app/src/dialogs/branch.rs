//! Branch dialogs: create/delete, rename, checkout, upstream.

use super::common::*;
use super::{Dialog, TabDialog};
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use gk_core::ops::StashOptions;

#[derive(PartialEq, Eq, Clone, Copy)]
enum BranchTab {
    New,
    Delete,
}

pub struct Branch {
    tab: BranchTab,
    name: String,
    /// Given start point; `None` = HEAD.
    start: Option<String>,
    use_start: bool,
    checkout: bool,
    start_label: RevLabel,
    // delete tab
    selected: Vec<String>,
    force: bool,
    delete_remote: bool,
}

impl Branch {
    pub fn new(_s: &Snapshot, start: Option<String>) -> Self {
        Self {
            tab: BranchTab::New,
            name: String::new(),
            use_start: start.is_some(),
            start,
            checkout: true,
            start_label: RevLabel::default(),
            selected: Vec::new(),
            force: false,
            delete_remote: false,
        }
    }
}

impl TabDialog for Branch {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let current = snap.refs.head.branch().unwrap_or_default().to_string();
        let ok_label = if self.tab == BranchTab::New { "Create branch" } else { "Delete branches" };
        let (out, ()) = modal(ctx, "branch", "Branch", 520.0, ok_label, |ui, ctl| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, BranchTab::New, "New branch");
                ui.selectable_value(&mut self.tab, BranchTab::Delete, "Delete branches");
            });
            ui.separator();
            match self.tab {
                BranchTab::New => {
                    ui.horizontal(|ui| {
                        ui.label("Current branch:");
                        ui.label(RichText::new(if current.is_empty() { "(detached HEAD)" } else { &current }).strong());
                    });
                    ui.horizontal(|ui| {
                        ui.label("New branch:");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(300.0));
                        ctl.focus(&r);
                    });
                    ui.label("Commit:");
                    ui.indent("branch-start", |ui| {
                        ui.radio_value(&mut self.use_start, false, "Working copy parent (HEAD)");
                        if let Some(s) = &self.start {
                            let label = self.start_label.get(tab, snap, s).to_string();
                            let text = if label.is_empty() { s.clone() } else { format!("{s}  —  {label}") };
                            ui.radio_value(&mut self.use_start, true, format!("Specified commit: {text}"));
                        }
                    });
                    ui.checkbox(&mut self.checkout, "Checkout new branch");
                    let name = self.name.trim();
                    match check_new_branch(snap, name) {
                        Err(e) if !name.is_empty() => {
                            error(ui, e);
                            ctl.invalid();
                        }
                        Err(_) => ctl.invalid(),
                        Ok(()) => {}
                    }
                    if self.checkout && worktree_dirty(snap) && self.use_start {
                        warning(ui, "Uncommitted changes are carried over to the new branch unless they conflict.");
                    }
                }
                BranchTab::Delete => {
                    ui.label("Select the local branches to delete:");
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                            for b in &snap.refs.branches {
                                let mut on = self.selected.contains(&b.name);
                                let mut text = b.name.clone();
                                if let Some(u) = &b.upstream {
                                    text.push_str(&format!("   → {u}"));
                                }
                                if b.is_head {
                                    ui.add_enabled(false, egui::Checkbox::new(&mut false, format!("{text}  (current)")));
                                    continue;
                                }
                                if ui.checkbox(&mut on, text).changed() {
                                    if on {
                                        self.selected.push(b.name.clone());
                                    } else {
                                        self.selected.retain(|n| n != &b.name);
                                    }
                                }
                            }
                        });
                    });
                    ui.checkbox(&mut self.force, "Force delete regardless of merge status (-D)");
                    ui.checkbox(&mut self.delete_remote, "Also delete the remote branches they track");
                    if self.delete_remote {
                        ui.colored_label(DANGER, "Remote branches are deleted on the server for everyone.");
                    }
                    if self.force {
                        warning(ui, "Unmerged commits of force-deleted branches can only be recovered from the reflog.");
                    }
                    if self.selected.is_empty() {
                        ctl.invalid();
                    }
                    ctl.danger = true;
                }
            }
        });
        finish(out, || match self.tab {
            BranchTab::New => {
                let name = self.name.trim().to_string();
                let start = if self.use_start { self.start.clone() } else { None };
                let checkout = self.checkout;
                tab.session.run_op(format!("Create branch {name}"), move |repo, _, _| {
                    repo.create_branch(&name, start.as_deref(), checkout)?;
                    Ok(String::new())
                });
            }
            BranchTab::Delete => {
                queue_delete(tab, snap, self.selected.clone(), self.force, self.delete_remote);
            }
        })
    }
}

/// Deletes local branches and optionally their upstream branches on the remote.
fn queue_delete(tab: &RepoTab, snap: &Snapshot, names: Vec<String>, force: bool, remote_too: bool) {
    let remotes = remote_names(snap);
    let jobs: Vec<(String, Option<(String, String)>)> = names
        .into_iter()
        .map(|n| {
            let up = if remote_too {
                snap.refs
                    .branch(&n)
                    .and_then(|b| b.upstream.as_deref().filter(|_| !b.upstream_gone))
                    .and_then(|u| split_remote_ref(&remotes, u))
            } else {
                None
            };
            (n, up)
        })
        .collect();
    let label = if jobs.len() == 1 { format!("Delete branch {}", jobs[0].0) } else { format!("Delete {} branches", jobs.len()) };
    tab.session.run_op(label, move |repo, cancel, p| {
        for (name, up) in &jobs {
            repo.delete_branch(name, force)?;
            if let Some((remote, branch)) = up {
                repo.delete_remote_branch(remote, branch, cancel, &mut *p)?;
            }
        }
        Ok(String::new())
    });
}

pub struct DeleteBranch {
    name: String,
    force: bool,
    delete_remote: bool,
}

impl DeleteBranch {
    pub fn new(name: String) -> Self {
        Self { name, force: false, delete_remote: false }
    }
}

impl TabDialog for DeleteBranch {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let b = snap.refs.branch(&self.name);
        let (out, ()) = modal(ctx, "delete-branch", "Delete branch", 460.0, "Delete", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Delete local branch");
                ui.label(RichText::new(&self.name).strong());
                ui.label("?");
            });
            ui.checkbox(&mut self.force, "Force delete regardless of merge status (-D)");
            if let Some(u) = b.and_then(|b| b.upstream.as_ref().filter(|_| !b.upstream_gone)) {
                ui.checkbox(&mut self.delete_remote, format!("Also delete the remote branch {u}"));
                if self.delete_remote {
                    ui.colored_label(DANGER, "The remote branch is deleted on the server for everyone.");
                }
            }
            match b {
                None => {
                    error(ui, "The branch no longer exists.");
                    ctl.invalid();
                }
                Some(b) if b.is_head => {
                    error(ui, "Cannot delete the checked-out branch. Checkout another branch first.");
                    ctl.invalid();
                }
                _ => {}
            }
            ctl.danger = true;
        });
        finish(out, || queue_delete(tab, snap, vec![self.name.clone()], self.force, self.delete_remote))
    }
}

pub struct RenameBranch {
    old: String,
    name: String,
}

impl RenameBranch {
    pub fn new(name: String) -> Self {
        Self { old: name.clone(), name }
    }
}

impl TabDialog for RenameBranch {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let title = format!("Rename branch '{}'", self.old);
        let (out, ()) = modal(ctx, "rename-branch", &title, 440.0, "Rename", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("New name:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(300.0));
                ctl.focus(&r);
            });
            let name = self.name.trim();
            if name == self.old {
                ctl.invalid();
            } else if let Err(e) = check_new_branch(snap, name) {
                error(ui, e);
                ctl.invalid();
            }
            if snap.refs.branch(&self.old).is_some_and(|b| b.upstream.is_some()) {
                hint(ui, "The upstream setting is kept; the remote branch is not renamed.");
            }
        });
        finish(out, || {
            let (old, new) = (self.old.clone(), self.name.trim().to_string());
            tab.session.run_op(format!("Rename {old} to {new}"), move |repo, _, _| {
                repo.rename_branch(&old, &new)?;
                Ok(String::new())
            });
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Local,
    Other,
}

pub struct Checkout {
    rev: String,
    target: Option<Target>,
    label: RevLabel,
}

impl Checkout {
    pub fn new(rev: String) -> Self {
        Self { rev, target: None, label: RevLabel::default() }
    }
}

fn queue_checkout(tab: &RepoTab, rev: String, stash: bool) {
    tab.session.run_op(format!("Checkout {rev}"), move |repo, _, _| {
        if stash {
            repo.stash_push(&format!("GitKwaken: before checkout of {rev}"), &StashOptions::default(), &[])?;
        }
        repo.checkout(&rev)?;
        Ok(String::new())
    });
}

impl TabDialog for Checkout {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let dirty = worktree_dirty(snap);
        let target = match self.target {
            Some(t) => t,
            None => {
                let refs = &snap.refs;
                if refs.branch(&self.rev).is_some() {
                    if refs.head.branch() == Some(self.rev.as_str()) {
                        return false; // already there
                    }
                    if !dirty {
                        queue_checkout(tab, self.rev.clone(), false);
                        return false;
                    }
                    self.target = Some(Target::Local);
                } else if refs.remote_branches.iter().any(|b| b.full() == self.rev) {
                    tab.dialog = Some(Dialog::checkout_remote(self.rev.clone()));
                    return true;
                } else {
                    self.target = Some(Target::Other);
                }
                self.target.unwrap_or(Target::Other)
            }
        };
        #[derive(PartialEq)]
        enum Extra {
            None,
            Stash,
            NewBranch,
        }
        let mut extra = Extra::None;
        let ok = if target == Target::Local { "Checkout" } else { "Checkout (detached HEAD)" };
        let (out, ()) = modal(ctx, "checkout", "Checkout", 500.0, ok, |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Checkout");
                ui.label(RichText::new(&self.rev).strong());
            });
            let label = self.label.get(tab, snap, &self.rev).to_string();
            if target == Target::Other && !label.is_empty() {
                hint(ui, label);
            }
            if target == Target::Other {
                warning(
                    ui,
                    "This puts the repository in 'detached HEAD' state. New commits will not belong to any \
                     branch and are easy to lose. Create a branch to keep working from here.",
                );
            }
            if dirty {
                warning(ui, "You have uncommitted changes. They are carried over if they do not conflict; otherwise the checkout fails.");
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if target == Target::Other && ui.button("Create branch instead…").clicked() {
                    extra = Extra::NewBranch;
                }
                if dirty && ui.button("Stash changes & checkout").on_hover_text("git stash, then checkout").clicked() {
                    extra = Extra::Stash;
                }
            });
            if extra != Extra::None {
                ctl.close = true;
            }
        });
        match extra {
            Extra::NewBranch => {
                tab.dialog = Some(Dialog::branch(snap, Some(self.rev.clone())));
                return true;
            }
            Extra::Stash => {
                queue_checkout(tab, self.rev.clone(), true);
                return false;
            }
            Extra::None => {}
        }
        finish(out, || queue_checkout(tab, self.rev.clone(), false))
    }
}

pub struct SetUpstream {
    branch: String,
    upstream: Option<String>,
}

impl SetUpstream {
    pub fn new(branch: String) -> Self {
        Self { branch, upstream: None }
    }
}

impl TabDialog for SetUpstream {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let current = snap.refs.branch(&self.branch).and_then(|b| b.upstream.clone()).unwrap_or_default();
        let upstream = self.upstream.get_or_insert_with(|| current.clone());
        let options: Vec<String> = snap.refs.remote_branches.iter().map(|b| b.full()).collect();
        let (out, ()) = modal(ctx, "set-upstream", "Track remote branch", 460.0, "OK", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Local branch:");
                ui.label(RichText::new(&self.branch).strong());
            });
            ui.horizontal(|ui| {
                ui.label("Tracks:");
                combo(ui, "upstream-combo", upstream, &options, Some("(none)"), 300.0);
            });
            if snap.refs.branch(&self.branch).is_none() {
                error(ui, "The branch no longer exists.");
                ctl.invalid();
            }
        });
        let new = upstream.clone();
        if new == current {
            return finish(out, || {});
        }
        finish(out, || {
            let branch = self.branch.clone();
            let up = (!new.is_empty()).then_some(new);
            tab.session.run_op(format!("Set upstream of {branch}"), move |repo, _, _| {
                repo.set_upstream(&branch, up.as_deref())?;
                Ok(String::new())
            });
        })
    }
}
