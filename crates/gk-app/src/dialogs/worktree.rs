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
    /// The plan was refreshed at OK because the working copy changed.
    changed: bool,
    /// Status generation the plan was computed for (discard all).
    status_gen: Option<u64>,
}

impl Discard {
    pub fn all() -> Self {
        Self {
            all: true,
            plan: None,
            changed: false,
            status_gen: None,
        }
    }

    pub fn paths(tracked: Vec<String>, untracked: Vec<String>) -> Self {
        Self {
            all: false,
            plan: Some(DiscardPlan {
                unstage: Vec::new(),
                tracked,
                untracked,
            }),
            changed: false,
            status_gen: None,
        }
    }

    fn run(tab: &RepoTab, plan: DiscardPlan, all: bool) {
        let label = if all {
            "Discard all changes".to_string()
        } else {
            "Discard changes".to_string()
        };
        let job = move |repo: &gk_core::Repo| -> anyhow::Result<()> {
            repo.unstage(&plan.unstage)?;
            repo.discard(&plan.tracked, &plan.untracked)
        };
        if all {
            tab.session
                .run_op(label, move |repo, _, _| job(repo).map(|_| String::new()));
        } else {
            tab.session.run_index_op(label, job);
        }
    }
}

impl TabDialog for Discard {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        if self.all && self.status_gen != Some(snap.status_gen) {
            // Follow the working copy while the dialog is open (it may have
            // been opened before the status was loaded).
            self.status_gen = Some(snap.status_gen);
            let fresh = discard_plan(snap);
            if let Some(p) = &self.plan
                && *p != fresh
                && !(p.tracked.is_empty() && p.untracked.is_empty())
            {
                self.changed = true;
            }
            self.plan = Some(fresh);
        }
        let plan = self.plan.get_or_insert_with(|| discard_plan(snap));
        if self.all && snap.status_loading && plan.tracked.is_empty() && plan.untracked.is_empty() {
            return true; // wait for the status
        }
        if !shared.settings.confirmations.discard {
            Self::run(tab, std::mem::take(plan), self.all);
            return false;
        }
        let palette = shared.palette;
        let changed = self.changed;
        let (out, ()) = modal(
            ctx,
            "discard",
            "Discard changes",
            560.0,
            "Discard",
            |ui, ctl| {
                ui.label("The following changes will be permanently discarded:");
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for p in &plan.tracked {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("M")
                                            .monospace()
                                            .strong()
                                            .color(palette.modified),
                                    );
                                    ui.label(p);
                                });
                            }
                            for p in &plan.untracked {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("?")
                                            .monospace()
                                            .strong()
                                            .color(palette.untracked),
                                    );
                                    ui.label(p);
                                    ui.label(RichText::new("(deleted)").color(DANGER).small());
                                });
                            }
                        });
                });
                if changed {
                    warning(ui, "The working copy changed: review the updated list.");
                }
                ui.colored_label(DANGER, "This cannot be undone.");
                ctl.danger = true;
                if plan.tracked.is_empty() && plan.untracked.is_empty() {
                    hint(ui, "Nothing to discard.");
                    ctl.invalid();
                }
            },
        );
        let all = self.all;
        if out == Outcome::Ok && all {
            // The working copy may have changed while the dialog was open:
            // never discard anything the user has not seen.
            let fresh = discard_plan(snap);
            if fresh != *plan {
                *plan = fresh;
                self.changed = true;
                return true;
            }
        }
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
        Self {
            paths,
            message: String::new(),
            keep_index: false,
            include_untracked: false,
            staged_only: false,
        }
    }
}

impl TabDialog for Stash {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let title = if self.paths.is_empty() {
            "Stash changes"
        } else {
            "Stash selected files"
        };
        let (out, ()) = modal(ctx, "stash", title, 500.0, "Stash", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Message:");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.message)
                        .desired_width(360.0)
                        .hint_text("optional"),
                );
                ctl.focus(&r);
            });
            if !self.paths.is_empty() {
                ui.label(format!("{} file(s):", self.paths.len()));
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for p in &self.paths {
                            ui.label(RichText::new(p).monospace());
                        }
                    });
            }
            ui.add_enabled(
                !self.staged_only,
                egui::Checkbox::new(&mut self.keep_index, "Keep staged changes"),
            );
            ui.add_enabled(
                !self.staged_only,
                egui::Checkbox::new(&mut self.include_untracked, "Include untracked files"),
            );
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
                hint(
                    ui,
                    "Only untracked files changed: enable 'Include untracked files'.",
                );
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

/// A stash picked by index, pinned to its commit id on first show: queued
/// operations can shift indices, so the index is resolved again at run time.
struct StashRef {
    index: usize,
    id: Option<String>,
}

impl StashRef {
    fn new(index: usize) -> Self {
        Self { index, id: None }
    }

    /// "stash@{n}: message" of the pinned stash (by id), None when gone.
    fn label(&mut self, snap: &Snapshot) -> Option<String> {
        if self.id.is_none() {
            self.id = snap
                .refs
                .stashes
                .iter()
                .find(|s| s.index == self.index)
                .map(|s| s.id.clone());
        }
        let id = self.id.as_deref()?;
        snap.refs
            .stashes
            .iter()
            .find(|s| s.id == id)
            .map(|s| format!("stash@{{{}}}: {}", s.index, s.message))
    }

    fn id(&self) -> String {
        self.id.clone().unwrap_or_default()
    }
}

/// Current index of the stash commit `id` (`git stash list`), at run time.
fn stash_index(repo: &gk_core::Repo, id: &str) -> anyhow::Result<usize> {
    let out = repo
        .git()
        .run(&["stash", "list", "--format=%H"])?
        .stdout_str();
    find_stash(&out, id)
        .ok_or_else(|| anyhow::anyhow!("the stash no longer exists (it was applied or deleted)"))
}

pub fn find_stash(list: &str, id: &str) -> Option<usize> {
    if id.is_empty() {
        return None;
    }
    list.lines().position(|l| l.trim() == id)
}

pub struct StashApply {
    stash: StashRef,
    pop: bool,
    restore_index: bool,
}

impl StashApply {
    pub fn new(index: usize) -> Self {
        Self {
            stash: StashRef::new(index),
            pop: false,
            restore_index: false,
        }
    }

    /// The same dialog with "delete the stash after applying" checked.
    pub fn pop(index: usize) -> Self {
        Self {
            pop: true,
            ..Self::new(index)
        }
    }
}

impl TabDialog for StashApply {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let label = self.stash.label(snap);
        let (out, ()) = modal(
            ctx,
            "stash-apply",
            "Apply stash",
            480.0,
            "Apply",
            |ui, ctl| {
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
                    warning(
                        ui,
                        "You have uncommitted changes; applying may produce conflicts.",
                    );
                }
            },
        );
        finish(out, || {
            let (id, pop, restore) = (self.stash.id(), self.pop, self.restore_index);
            let l = if pop { "Pop stash" } else { "Apply stash" };
            tab.session.run_op(l, move |repo, _, _| {
                repo.stash_apply(stash_index(repo, &id)?, pop, restore)
            });
        })
    }
}

pub struct StashDrop {
    stash: StashRef,
}

impl StashDrop {
    pub fn new(index: usize) -> Self {
        Self {
            stash: StashRef::new(index),
        }
    }
}

impl TabDialog for StashDrop {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let label = self.stash.label(snap);
        let (out, ()) = modal(
            ctx,
            "stash-drop",
            "Delete stash",
            460.0,
            "Delete",
            |ui, ctl| {
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
            },
        );
        finish(out, || {
            let id = self.stash.id();
            tab.session.run_op("Delete stash", move |repo, _, _| {
                repo.stash_drop(stash_index(repo, &id)?)?;
                Ok(String::new())
            });
        })
    }
}

pub struct StashBranch {
    stash: StashRef,
    name: String,
}

impl StashBranch {
    pub fn new(index: usize) -> Self {
        Self {
            stash: StashRef::new(index),
            name: String::new(),
        }
    }
}

impl TabDialog for StashBranch {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let label = self.stash.label(snap);
        let (out, ()) = modal(
            ctx,
            "stash-branch",
            "Branch from stash",
            480.0,
            "Create branch",
            |ui, ctl| {
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
                hint(
                    ui,
                    "Creates the branch at the stash's base commit, checks it out, applies the stash and drops it.",
                );
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
                    warning(
                        ui,
                        "You have uncommitted changes; git may refuse to switch branches.",
                    );
                }
            },
        );
        finish(out, || {
            let (id, name) = (self.stash.id(), self.name.trim().to_string());
            tab.session
                .run_op(format!("Branch {name} from stash"), move |repo, _, _| {
                    repo.stash_branch(stash_index(repo, &id)?, &name)?;
                    Ok(String::new())
                });
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_lookup() {
        let list = "aaa\nbbb\nccc\n";
        assert_eq!(find_stash(list, "bbb"), Some(1));
        assert_eq!(find_stash(list, "zzz"), None);
        assert_eq!(find_stash(list, ""), None);
    }
}
