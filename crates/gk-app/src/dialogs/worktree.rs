//! Working copy dialogs: discard and stash.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use gk_core::ops::StashOptions;
use gk_status::Kind;

// ---------------------------------------------------------------- discard

/// What "discard everything" has to do, derived from the status.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct DiscardPlan {
    /// Paths to unstage first.
    pub unstage: Vec<String>,
    /// Paths to restore from the index after unstaging.
    pub tracked: Vec<String>,
    /// Paths to delete.
    pub untracked: Vec<String>,
}

pub fn discard_plan(snap: &Snapshot) -> DiscardPlan {
    let unborn = matches!(snap.refs.head, gk_core::Head::Unborn(_));
    let mut p = DiscardPlan::default();
    for e in &snap.status.staged {
        let path = e.path.to_string();
        p.unstage.push(path.clone());
        match e.kind {
            _ if unborn => p.untracked.push(path),
            Kind::Added | Kind::IntentToAdd => p.untracked.push(path),
            Kind::Renamed | Kind::Copied => {
                p.untracked.push(path);
                if let Some(o) = &e.orig_path {
                    let o = o.to_string();
                    p.unstage.push(o.clone());
                    if e.kind == Kind::Renamed {
                        p.tracked.push(o);
                    }
                }
            }
            _ => p.tracked.push(path),
        }
    }
    for e in &snap.status.unstaged {
        let path = e.path.to_string();
        match e.kind {
            Kind::Untracked => p.untracked.push(path),
            Kind::Conflict(_) => {
                p.unstage.push(path.clone());
                p.tracked.push(path);
            }
            _ if unborn => {}
            _ => p.tracked.push(path),
        }
    }
    for v in [&mut p.unstage, &mut p.tracked, &mut p.untracked] {
        v.sort();
        v.dedup();
    }
    // A staged addition that was then modified is still deleted, not restored.
    let untracked = p.untracked.clone();
    p.tracked.retain(|t| untracked.binary_search(t).is_err());
    p
}

pub struct Discard {
    all: bool,
    plan: Option<DiscardPlan>,
}

impl Discard {
    pub fn all() -> Self {
        Self { all: true, plan: None }
    }

    pub fn paths(tracked: Vec<String>, untracked: Vec<String>) -> Self {
        Self { all: false, plan: Some(DiscardPlan { unstage: Vec::new(), tracked, untracked }) }
    }

    fn run(tab: &RepoTab, plan: DiscardPlan, all: bool) {
        let label = if all { "Discard all changes".to_string() } else { "Discard changes".to_string() };
        let job = move |repo: &gk_core::Repo| -> anyhow::Result<()> {
            repo.unstage(&plan.unstage)?;
            repo.discard(&plan.tracked, &plan.untracked)
        };
        if all {
            tab.session.run_op(label, move |repo, _, _| job(repo).map(|_| String::new()));
        } else {
            tab.session.run_index_op(label, job);
        }
    }
}

impl TabDialog for Discard {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) -> bool {
        let plan = self.plan.get_or_insert_with(|| discard_plan(snap));
        if !shared.settings.confirmations.discard {
            Self::run(tab, std::mem::take(plan), self.all);
            return false;
        }
        let palette = shared.palette;
        let (out, ()) = modal(ctx, "discard", "Discard changes", 560.0, "Discard", |ui, ctl| {
            ui.label("The following changes will be permanently discarded:");
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                    for p in &plan.tracked {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("M").monospace().strong().color(palette.modified));
                            ui.label(p);
                        });
                    }
                    for p in &plan.untracked {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("?").monospace().strong().color(palette.untracked));
                            ui.label(p);
                            ui.label(RichText::new("(deleted)").color(DANGER).small());
                        });
                    }
                });
            });
            ui.colored_label(DANGER, "This cannot be undone.");
            ctl.danger = true;
            if plan.tracked.is_empty() && plan.untracked.is_empty() {
                hint(ui, "Nothing to discard.");
                ctl.invalid();
            }
        });
        let all = self.all;
        finish(out, || Self::run(tab, std::mem::take(plan), all))
    }
}

// ---------------------------------------------------------------- stash

pub struct Stash {
    paths: Vec<String>,
    message: String,
    keep_index: bool,
    include_untracked: bool,
    staged_only: bool,
}

impl Stash {
    pub fn new(paths: Vec<String>) -> Self {
        Self { paths, message: String::new(), keep_index: false, include_untracked: false, staged_only: false }
    }
}

impl TabDialog for Stash {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let title = if self.paths.is_empty() { "Stash changes" } else { "Stash selected files" };
        let (out, ()) = modal(ctx, "stash", title, 500.0, "Stash", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Message:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.message).desired_width(360.0).hint_text("optional"));
                ctl.focus(&r);
            });
            if !self.paths.is_empty() {
                ui.label(format!("{} file(s):", self.paths.len()));
                egui::ScrollArea::vertical().max_height(140.0).auto_shrink([false, true]).show(ui, |ui| {
                    for p in &self.paths {
                        ui.label(RichText::new(p).monospace());
                    }
                });
            }
            ui.add_enabled(!self.staged_only, egui::Checkbox::new(&mut self.keep_index, "Keep staged changes"));
            ui.add_enabled(!self.staged_only, egui::Checkbox::new(&mut self.include_untracked, "Include untracked files"));
            ui.checkbox(&mut self.staged_only, "Stash only staged changes");
            let st = &snap.status;
            if st.staged.is_empty() && st.unstaged.is_empty() {
                hint(ui, "There are no changes to stash.");
                ctl.invalid();
            } else if self.staged_only && st.staged.is_empty() {
                error(ui, "There are no staged changes.");
                ctl.invalid();
            } else if !self.include_untracked
                && !self.staged_only
                && self.paths.is_empty()
                && !worktree_dirty(snap)
            {
                hint(ui, "Only untracked files changed: enable 'Include untracked files'.");
                ctl.invalid();
            }
        });
        finish(out, || {
            let msg = self.message.trim().to_string();
            let opts = StashOptions {
                keep_index: self.keep_index && !self.staged_only,
                include_untracked: self.include_untracked && !self.staged_only,
                staged_only: self.staged_only,
            };
            let paths = self.paths.clone();
            tab.session.run_op("Stash", move |repo, _, _| {
                repo.stash_push(&msg, &opts, &paths)?;
                Ok(String::new())
            });
        })
    }
}

fn stash_label(snap: &Snapshot, index: usize) -> Option<String> {
    snap.refs.stashes.iter().find(|s| s.index == index).map(|s| format!("stash@{{{index}}}: {}", s.message))
}

pub struct StashApply {
    index: usize,
    pop: bool,
    restore_index: bool,
}

impl StashApply {
    pub fn new(index: usize) -> Self {
        Self { index, pop: false, restore_index: false }
    }
}

impl TabDialog for StashApply {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let label = stash_label(snap, self.index);
        let (out, ()) = modal(ctx, "stash-apply", "Apply stash", 480.0, "Apply", |ui, ctl| {
            match &label {
                Some(l) => {
                    ui.label(RichText::new(l).strong());
                }
                None => {
                    error(ui, "The stash no longer exists.");
                    ctl.invalid();
                }
            }
            ui.checkbox(&mut self.pop, "Delete the stash after applying (pop)");
            ui.checkbox(&mut self.restore_index, "Restore staged state (--index)");
            if worktree_dirty(snap) {
                warning(ui, "You have uncommitted changes; applying may produce conflicts.");
            }
        });
        finish(out, || {
            let (index, pop, restore) = (self.index, self.pop, self.restore_index);
            let l = if pop { "Pop stash" } else { "Apply stash" };
            tab.session.run_op(l, move |repo, _, _| repo.stash_apply(index, pop, restore));
        })
    }
}

pub struct StashDrop {
    pub index: usize,
}

impl TabDialog for StashDrop {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let label = stash_label(snap, self.index);
        let (out, ()) = modal(ctx, "stash-drop", "Delete stash", 460.0, "Delete", |ui, ctl| {
            match &label {
                Some(l) => {
                    ui.label("Permanently delete this stash?");
                    ui.label(RichText::new(l).strong());
                }
                None => {
                    error(ui, "The stash no longer exists.");
                    ctl.invalid();
                }
            }
            ctl.danger = true;
        });
        finish(out, || {
            let index = self.index;
            tab.session.run_op("Delete stash", move |repo, _, _| {
                repo.stash_drop(index)?;
                Ok(String::new())
            });
        })
    }
}

pub struct StashBranch {
    index: usize,
    name: String,
}

impl StashBranch {
    pub fn new(index: usize) -> Self {
        Self { index, name: String::new() }
    }
}

impl TabDialog for StashBranch {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        let label = stash_label(snap, self.index);
        let (out, ()) = modal(ctx, "stash-branch", "Branch from stash", 480.0, "Create branch", |ui, ctl| {
            match &label {
                Some(l) => {
                    ui.label(RichText::new(l).strong());
                }
                None => {
                    error(ui, "The stash no longer exists.");
                    ctl.invalid();
                }
            }
            ui.horizontal(|ui| {
                ui.label("New branch:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(300.0));
                ctl.focus(&r);
            });
            hint(ui, "Creates the branch at the stash's base commit, checks it out, applies the stash and drops it.");
            let name = self.name.trim();
            match check_new_branch(snap, name) {
                Err(e) if !name.is_empty() => {
                    error(ui, e);
                    ctl.invalid();
                }
                Err(_) => ctl.invalid(),
                Ok(()) => {}
            }
            if worktree_dirty(snap) {
                warning(ui, "You have uncommitted changes; git may refuse to switch branches.");
            }
        });
        finish(out, || {
            let (index, name) = (self.index, self.name.trim().to_string());
            tab.session.run_op(format!("Branch {name} from stash"), move |repo, _, _| {
                repo.stash_branch(index, &name)?;
                Ok(String::new())
            });
        })
    }
}
