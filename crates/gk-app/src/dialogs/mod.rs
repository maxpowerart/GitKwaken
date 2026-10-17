//! Modal dialogs. `Dialog` belongs to a repository tab, `GlobalDialog` to the
//! app. Views only use the constructors below; this module owns the UI.

mod branch;
mod common;
mod global;
mod misc;
mod remote;
mod rewrite;
mod settings;
mod tag;
mod worktree;

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;
use std::path::PathBuf;

/// Askpass environment for git processes the app starts outside a tab
/// (clone). Call once at startup with `GkApp::askpass_env()`.

pub fn set_askpass_env(env: Vec<(String, String)>) {
    common::set_askpass_env(env);
}

/// A dialog of a repository tab.
pub(crate) trait TabDialog {
    /// Returns false when the dialog should close.
    fn show(&mut self, ctx: &egui::Context, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) -> bool;
}

/// A dialog of the application (no repository).
pub(crate) trait AppDialog {
    fn show(&mut self, ctx: &egui::Context, shared: &mut Shared) -> bool;
}

pub struct Dialog(Box<dyn TabDialog>);

// Views wire these up progressively; unused ones must not warn.
#[allow(dead_code)]
impl Dialog {
    fn new(d: impl TabDialog + 'static) -> Self {
        Dialog(Box::new(d))
    }

    // remotes
    pub fn pull(s: &Snapshot) -> Self { Self::new(remote::Pull::new(s)) }
    pub fn push(s: &Snapshot) -> Self { Self::new(remote::Push::new(s)) }
    pub fn fetch(s: &Snapshot) -> Self { Self::new(remote::Fetch::new(s)) }
    pub fn add_remote() -> Self { Self::new(remote::AddRemote::default()) }
    pub fn edit_remote(name: String) -> Self { Self::new(remote::EditRemote::new(name)) }
    pub fn remove_remote(name: String) -> Self { Self::new(remote::RemoveRemote { name }) }
    /// Checkout `remote/branch` as a new local tracking branch.
    pub fn checkout_remote(remote_branch: String) -> Self { Self::new(remote::CheckoutRemote::new(remote_branch)) }
    pub fn delete_remote_branch(remote: String, branch: String) -> Self {
        Self::new(remote::DeleteRemoteBranch { remote, branch })
    }

    // branches
    /// New branch, starting at `start` (a rev) or HEAD.
    pub fn branch(s: &Snapshot, start: Option<String>) -> Self { Self::new(branch::Branch::new(s, start)) }
    pub fn delete_branch(name: String) -> Self { Self::new(branch::DeleteBranch::new(name)) }
    pub fn rename_branch(name: String) -> Self { Self::new(branch::RenameBranch::new(name)) }
    /// Checkout of a branch name or commit id; warns about detached HEAD and dirty worktree.
    pub fn checkout(rev: String) -> Self { Self::new(branch::Checkout::new(rev)) }
    pub fn set_upstream(branch: String) -> Self { Self::new(branch::SetUpstream::new(branch)) }

    // history rewriting
    pub fn merge(s: &Snapshot, rev: Option<String>) -> Self { Self::new(rewrite::Merge::new(s, rev)) }
    pub fn rebase(s: &Snapshot, onto: Option<String>) -> Self { Self::new(rewrite::Rebase::new(s, onto)) }
    /// Interactive rebase of the commits after `base` (exclusive) up to HEAD.
    pub fn interactive_rebase(_s: &Snapshot, base: String) -> Self { Self::new(rewrite::InteractiveRebase::new(base)) }
    pub fn cherry_pick(ids: Vec<String>) -> Self { Self::new(rewrite::Picks::new(ids, false)) }
    pub fn revert(ids: Vec<String>) -> Self { Self::new(rewrite::Picks::new(ids, true)) }
    pub fn reset(rev: String) -> Self { Self::new(rewrite::Reset::new(rev)) }

    // working copy
    pub fn discard_all() -> Self { Self::new(worktree::Discard::all()) }
    pub fn discard_paths(tracked: Vec<String>, untracked: Vec<String>) -> Self {
        Self::new(worktree::Discard::paths(tracked, untracked))
    }
    pub fn stash() -> Self { Self::new(worktree::Stash::new(Vec::new())) }
    pub fn stash_paths(paths: Vec<String>) -> Self { Self::new(worktree::Stash::new(paths)) }
    pub fn stash_apply(index: usize) -> Self { Self::new(worktree::StashApply::new(index)) }
    pub fn stash_drop(index: usize) -> Self { Self::new(worktree::StashDrop { index }) }
    pub fn stash_branch(index: usize) -> Self { Self::new(worktree::StashBranch::new(index)) }

    // tags
    pub fn tag(rev: Option<String>) -> Self { Self::new(tag::Tag::new(rev)) }
    pub fn delete_tag(name: String) -> Self { Self::new(tag::DeleteTag::new(name)) }
    pub fn push_tag(name: String) -> Self { Self::new(tag::PushTag::new(name)) }

    // misc
    pub fn create_patch(ids: Vec<String>) -> Self { Self::new(misc::CreatePatch::new(ids)) }
    pub fn apply_patch() -> Self { Self::new(misc::ApplyPatch::default()) }
    pub fn archive(rev: String) -> Self { Self::new(misc::Archive::new(rev)) }
    /// Free-form git command line, output shown in the dialog.
    pub fn console() -> Self { Self::new(misc::Console::new()) }

    /// Renders the dialog; returns false when it should close. A dialog may
    /// replace itself by setting `tab.dialog` and returning true.
    pub fn show(&mut self, ctx: &egui::Context, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) -> bool {
        self.0.show(ctx, tab, snap, shared)
    }
}

pub struct GlobalDialog(Box<dyn AppDialog>);

#[allow(dead_code)]
impl GlobalDialog {
    fn new(d: impl AppDialog + 'static) -> Self {
        GlobalDialog(Box::new(d))
    }

    pub fn settings() -> Self { Self::new(settings::SettingsDialog::new()) }
    pub fn clone_repo() -> Self { Self::new(global::CloneRepo::new()) }
    pub fn init_repo() -> Self { Self::new(global::InitRepo::default()) }
    pub fn bookmark(p: PathBuf) -> Self { Self::new(global::Bookmark::new(p)) }
    pub fn about() -> Self { Self::new(global::About::default()) }

    pub fn show(&mut self, ctx: &egui::Context, shared: &mut Shared) -> bool {
        self.0.show(ctx, shared)
    }
}
