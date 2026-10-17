//! Modal dialogs. `Dialog` belongs to a repository tab, `GlobalDialog` to the
//! app. Views only use the constructors below; this module owns the UI.

mod bisect;
mod branch;
mod common;
mod config_edit;
mod diagnostics;
mod flow;
mod fsmonitor;
mod global;
mod lfs;
mod misc;
mod notes;
mod remote;
mod repo_tools;
mod rewrite;
mod scan;
mod settings;
mod tag;
mod undo;
mod worktree;

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;
use std::path::PathBuf;

pub use global::CloneJob;

/// Askpass environment for git processes the app starts outside a tab
/// (clone). Call once at startup with `GkApp::askpass_env()`.
pub fn set_askpass_env(env: Vec<(String, String)>) {
    common::set_askpass_env(env);
}

/// A dialog of a repository tab.
pub(crate) trait TabDialog {
    /// Returns false when the dialog should close.
    fn show(
        &mut self,
        ctx: &egui::Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool;
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
    pub fn pull(s: &Snapshot) -> Self {
        Self::new(remote::Pull::new(s))
    }
    pub fn push(s: &Snapshot) -> Self {
        Self::new(remote::Push::new(s))
    }
    pub fn fetch(s: &Snapshot) -> Self {
        Self::new(remote::Fetch::new(s))
    }
    pub fn add_remote() -> Self {
        Self::new(remote::AddRemote::default())
    }
    pub fn edit_remote(name: String) -> Self {
        Self::new(remote::EditRemote::new(name))
    }
    pub fn remove_remote(name: String) -> Self {
        Self::new(remote::RemoveRemote { name })
    }
    /// Checkout `remote/branch` as a new local tracking branch.
    pub fn checkout_remote(remote_branch: String) -> Self {
        Self::new(remote::CheckoutRemote::new(remote_branch))
    }
    pub fn delete_remote_branch(remote: String, branch: String) -> Self {
        Self::new(remote::DeleteRemoteBranch { remote, branch })
    }

    // branches
    /// New branch, starting at `start` (a rev) or HEAD.
    pub fn branch(s: &Snapshot, start: Option<String>) -> Self {
        Self::new(branch::Branch::new(s, start))
    }
    pub fn delete_branch(name: String) -> Self {
        Self::new(branch::DeleteBranch::new(name))
    }
    pub fn rename_branch(name: String) -> Self {
        Self::new(branch::RenameBranch::new(name))
    }
    /// Checkout of a branch name or commit id; warns about detached HEAD and dirty worktree.
    pub fn checkout(rev: String) -> Self {
        Self::new(branch::Checkout::new(rev))
    }
    pub fn set_upstream(branch: String) -> Self {
        Self::new(branch::SetUpstream::new(branch))
    }

    // history rewriting
    pub fn merge(s: &Snapshot, rev: Option<String>) -> Self {
        Self::new(rewrite::Merge::new(s, rev))
    }
    pub fn rebase(s: &Snapshot, onto: Option<String>) -> Self {
        Self::new(rewrite::Rebase::new(s, onto))
    }
    /// Interactive rebase of the commits after `base` (exclusive) up to HEAD.
    pub fn interactive_rebase(_s: &Snapshot, base: String) -> Self {
        Self::new(rewrite::InteractiveRebase::new(base))
    }
    pub fn cherry_pick(ids: Vec<String>) -> Self {
        Self::new(rewrite::Picks::new(ids, false))
    }
    pub fn revert(ids: Vec<String>) -> Self {
        Self::new(rewrite::Picks::new(ids, true))
    }
    pub fn reset(rev: String) -> Self {
        Self::new(rewrite::Reset::new(rev))
    }

    // working copy
    pub fn discard_all() -> Self {
        Self::new(worktree::Discard::all())
    }
    pub fn discard_paths(tracked: Vec<String>, untracked: Vec<String>) -> Self {
        Self::new(worktree::Discard::paths(tracked, untracked))
    }
    pub fn stash() -> Self {
        Self::new(worktree::Stash::new(Vec::new()))
    }
    pub fn stash_paths(paths: Vec<String>) -> Self {
        Self::new(worktree::Stash::new(paths))
    }
    pub fn stash_apply(index: usize) -> Self {
        Self::new(worktree::StashApply::new(index))
    }
    pub fn stash_drop(index: usize) -> Self {
        Self::new(worktree::StashDrop::new(index))
    }
    pub fn stash_branch(index: usize) -> Self {
        Self::new(worktree::StashBranch::new(index))
    }

    // tags
    pub fn tag(rev: Option<String>) -> Self {
        Self::new(tag::Tag::new(rev))
    }
    pub fn delete_tag(name: String) -> Self {
        Self::new(tag::DeleteTag::new(name))
    }
    pub fn push_tag(name: String) -> Self {
        Self::new(tag::PushTag::new(name))
    }

    // misc
    pub fn create_patch(ids: Vec<String>) -> Self {
        Self::new(misc::CreatePatch::new(ids))
    }
    pub fn apply_patch() -> Self {
        Self::new(misc::ApplyPatch::default())
    }
    pub fn archive(rev: String) -> Self {
        Self::new(misc::Archive::new(rev))
    }
    /// Free-form git command line, output shown in the dialog.
    pub fn console() -> Self {
        Self::new(misc::Console::new())
    }

    // advanced
    /// Start a bisect, or mark good/bad/skip while bisecting.
    pub fn bisect(s: &Snapshot) -> Self {
        Self::new(bisect::Bisect::new(s))
    }
    /// Git Flow init (when not set up), start and finish.
    pub fn git_flow(s: &Snapshot) -> Self {
        Self::new(flow::GitFlow::new(s))
    }
    pub fn lfs(s: &Snapshot) -> Self {
        Self::new(lfs::Lfs::new(s))
    }
    pub fn sparse_checkout(s: &Snapshot) -> Self {
        Self::new(repo_tools::Sparse::new(s))
    }
    /// Delete untracked (optionally ignored) files with a preview.
    pub fn clean() -> Self {
        Self::new(repo_tools::Clean::new())
    }
    pub fn optimize(s: &Snapshot) -> Self {
        Self::new(repo_tools::Optimize::new(s))
    }
    pub fn submodule_add() -> Self {
        Self::new(repo_tools::SubmoduleAdd::default())
    }
    /// Text editor for the repository's `.gitignore` with an ignored-files preview.
    pub fn edit_gitignore() -> Self {
        Self::new(config_edit::ConfigEditor::new(
            config_edit::ConfigKind::Gitignore,
        ))
    }
    /// Text editor for `.git/config` (validated, backed up on save).
    pub fn edit_repo_config() -> Self {
        Self::new(config_edit::ConfigEditor::new(
            config_edit::ConfigKind::RepoConfig,
        ))
    }
    /// Text editor for the global `~/.gitconfig` (validated, backed up on save).
    pub fn edit_global_config() -> Self {
        Self::new(config_edit::ConfigEditor::new(
            config_edit::ConfigKind::GlobalConfig,
        ))
    }
    /// Undo the last operation (from the reflog), with a preview.
    pub fn undo() -> Self {
        Self::new(undo::Undo::new())
    }
    /// Add or edit the `git notes` note of commit `id`.
    pub fn edit_note(id: String) -> Self {
        Self::new(notes::EditNote::new(id))
    }
    pub fn remove_note(id: String) -> Self {
        Self::new(notes::RemoveNote::new(id))
    }

    /// Renders the dialog; returns false when it should close. A dialog may
    /// replace itself by setting `tab.dialog` and returning true.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        if let Some(d) = debug_swap(|spec| debug_open(spec, snap)) {
            *self = d;
        }
        self.0.show(ctx, tab, snap, shared)
    }
}

/// Env var naming a dialog to show for UI screenshots: `<name>[:<arg>]`
/// (see [`debug_open`] / [`debug_open_global`]).
pub const DEBUG_DIALOG_ENV: &str = "GITKWAKEN_DIALOG";

/// Dev hook: the first dialog shown replaces itself (once per process) with
/// the one named by `GITKWAKEN_DIALOG`, so dialogs without a palette entry
/// can be screenshotted: `GITKWAKEN_PALETTE="Fetch…" GITKWAKEN_DIALOG=reset`.
fn debug_swap<T>(open: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) {
        return None;
    }
    let spec = std::env::var(DEBUG_DIALOG_ENV).ok()?;
    let d = open(spec.trim())?;
    DONE.store(true, Ordering::Relaxed);
    Some(d)
}

/// Builds a repository dialog by name for tests and screenshots
/// (`GITKWAKEN_DIALOG=<name>[:<arg>]`). Without an argument, a fitting
/// branch / commit / tag / stash of `snap` is used. Unknown names: None.
pub fn debug_open(spec: &str, snap: &Snapshot) -> Option<Dialog> {
    let (name, arg) = match spec.split_once(':') {
        Some((n, a)) => (n, Some(a.to_string())),
        None => (spec, None),
    };
    let refs = &snap.refs;
    let commit = |n: usize| {
        snap.history
            .ids
            .get(n)
            .or(snap.history.ids.last())
            .map(|id| id.to_string())
            .unwrap_or_else(|| "HEAD".into())
    };
    let branch = || {
        arg.clone().unwrap_or_else(|| {
            refs.branches
                .iter()
                .find(|b| !b.is_head)
                .or(refs.branches.first())
                .map(|b| b.name.clone())
                .unwrap_or_else(|| "main".into())
        })
    };
    let remote_branch = || {
        arg.clone().unwrap_or_else(|| {
            refs.remote_branches
                .first()
                .map(|b| b.full())
                .unwrap_or_else(|| "origin/main".into())
        })
    };
    let remote = || {
        arg.clone().unwrap_or_else(|| {
            refs.remotes
                .first()
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "origin".into())
        })
    };
    let tag = || {
        arg.clone().unwrap_or_else(|| {
            refs.tags
                .first()
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "v1.0".into())
        })
    };
    let rev = || arg.clone().unwrap_or_else(|| commit(5));
    let ids = || match &arg {
        Some(a) => a.split(',').map(str::to_string).collect(),
        None => vec![commit(1), commit(2), commit(3)],
    };
    let stash = || arg.as_deref().and_then(|a| a.parse().ok()).unwrap_or(0);
    let paths = || match &arg {
        Some(a) => a.split(',').map(str::to_string).collect(),
        None => snap
            .status
            .unstaged
            .iter()
            .chain(&snap.status.staged)
            .map(|e| e.path.to_string())
            .collect(),
    };
    Some(match name {
        "pull" => Dialog::pull(snap),
        "push" => Dialog::push(snap),
        "fetch" => Dialog::fetch(snap),
        "add-remote" => Dialog::add_remote(),
        "edit-remote" => Dialog::edit_remote(remote()),
        "remove-remote" => Dialog::remove_remote(remote()),
        "checkout-remote" => Dialog::checkout_remote(remote_branch()),
        "delete-remote-branch" => {
            let full = remote_branch();
            let (r, b) = common::split_remote_ref(&common::remote_names(snap), &full)
                .unwrap_or(("origin".into(), full));
            Dialog::delete_remote_branch(r, b)
        }
        "branch" => Dialog::branch(snap, arg.clone()),
        "delete-branch" => Dialog::delete_branch(branch()),
        "rename-branch" => Dialog::rename_branch(branch()),
        "checkout" => Dialog::checkout(branch()),
        "checkout-detached" => Dialog::checkout(rev()),
        "set-upstream" => Dialog::set_upstream(branch()),
        "merge" => Dialog::merge(snap, arg.clone()),
        "rebase" => Dialog::rebase(snap, arg.clone()),
        "interactive-rebase" => Dialog::interactive_rebase(snap, rev()),
        "cherry-pick" => Dialog::cherry_pick(ids()),
        "revert" => Dialog::revert(ids()),
        "reset" => Dialog::reset(rev()),
        "discard-all" => Dialog::discard_all(),
        "discard-paths" => Dialog::discard_paths(paths(), Vec::new()),
        "stash" => Dialog::stash(),
        "stash-paths" => Dialog::stash_paths(paths()),
        "stash-apply" => Dialog::stash_apply(stash()),
        "stash-drop" => Dialog::stash_drop(stash()),
        "stash-branch" => Dialog::stash_branch(stash()),
        "tag" => Dialog::tag(arg.clone()),
        "delete-tag" => Dialog::delete_tag(tag()),
        "push-tag" => Dialog::push_tag(tag()),
        "create-patch" => Dialog::create_patch(ids()),
        "apply-patch" => Dialog::apply_patch(),
        "archive" => Dialog::archive(arg.clone().unwrap_or_else(|| "HEAD".into())),
        "console" => Dialog::console(),
        "bisect" => Dialog::bisect(snap),
        "git-flow" => Dialog::git_flow(snap),
        "lfs" => Dialog::lfs(snap),
        "sparse" => Dialog::sparse_checkout(snap),
        "clean" => Dialog::clean(),
        "optimize" => Dialog::optimize(snap),
        "submodule-add" => Dialog::submodule_add(),
        "gitignore" => Dialog::edit_gitignore(),
        "repo-config" => Dialog::edit_repo_config(),
        "global-config" => Dialog::edit_global_config(),
        _ => return None,
    })
}

/// [`debug_open`] for application dialogs.
pub fn debug_open_global(spec: &str) -> Option<GlobalDialog> {
    let (name, arg) = spec.split_once(':').unwrap_or((spec, ""));
    Some(match name {
        "settings" => GlobalDialog(Box::new(settings::SettingsDialog::at(arg))),
        "clone" => GlobalDialog::clone_repo(),
        "init" => GlobalDialog::init_repo(),
        "bookmark" => GlobalDialog::bookmark(if arg.is_empty() {
            std::env::current_dir().unwrap_or_default()
        } else {
            PathBuf::from(arg)
        }),
        "about" => GlobalDialog::about(),
        "global-config" => GlobalDialog::edit_global_config(),
        _ => return None,
    })
}

pub struct GlobalDialog(Box<dyn AppDialog>);

#[allow(dead_code)]
impl GlobalDialog {
    fn new(d: impl AppDialog + 'static) -> Self {
        GlobalDialog(Box::new(d))
    }

    pub fn settings() -> Self {
        Self::new(settings::SettingsDialog::new())
    }
    pub fn clone_repo() -> Self {
        Self::new(global::CloneRepo::new())
    }
    pub fn init_repo() -> Self {
        Self::new(global::InitRepo::default())
    }
    pub fn bookmark(p: PathBuf) -> Self {
        Self::new(global::Bookmark::new(p))
    }
    /// Walks a folder for repositories to open or bookmark.
    pub fn scan_repos() -> Self {
        Self::new(scan::ScanRepos::new())
    }
    pub fn about() -> Self {
        Self::new(global::About::default())
    }
    /// Text editor for the global `~/.gitconfig` (no repository needed).
    pub fn edit_global_config() -> Self {
        Self::new(config_edit::ConfigEditor::new(
            config_edit::ConfigKind::GlobalConfig,
        ))
    }

    pub fn show(&mut self, ctx: &egui::Context, shared: &mut Shared) -> bool {
        if let Some(d) = debug_swap(debug_open_global) {
            *self = d;
        }
        self.0.show(ctx, shared)
    }
}
