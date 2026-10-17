//! Modal dialogs. `Dialog` belongs to a repository tab, `GlobalDialog` to the
//! app. Views only use the constructors below; this module owns the UI.

use crate::app::Shared;
use crate::tab::RepoTab;
use gk_core::Snapshot;
use std::path::PathBuf;

pub enum Dialog {
    Todo(String),
}

impl Dialog {
    fn todo(name: &str) -> Self {
        Dialog::Todo(name.to_string())
    }

    // remotes
    pub fn pull(_s: &Snapshot) -> Self { Self::todo("Pull") }
    pub fn push(_s: &Snapshot) -> Self { Self::todo("Push") }
    pub fn fetch(_s: &Snapshot) -> Self { Self::todo("Fetch") }
    pub fn add_remote() -> Self { Self::todo("Add remote") }
    pub fn edit_remote(_name: String) -> Self { Self::todo("Edit remote") }
    pub fn remove_remote(_name: String) -> Self { Self::todo("Remove remote") }
    /// Checkout `remote/branch` as a new local tracking branch.
    pub fn checkout_remote(_remote_branch: String) -> Self { Self::todo("Checkout remote branch") }
    pub fn delete_remote_branch(_remote: String, _branch: String) -> Self { Self::todo("Delete remote branch") }

    // branches
    /// New branch, starting at `start` (a rev) or HEAD.
    pub fn branch(_s: &Snapshot, _start: Option<String>) -> Self { Self::todo("Branch") }
    pub fn delete_branch(_name: String) -> Self { Self::todo("Delete branch") }
    pub fn rename_branch(_name: String) -> Self { Self::todo("Rename branch") }
    /// Checkout of a branch name or commit id; warns about detached HEAD and dirty worktree.
    pub fn checkout(_rev: String) -> Self { Self::todo("Checkout") }
    pub fn set_upstream(_branch: String) -> Self { Self::todo("Track remote branch") }

    // history rewriting
    pub fn merge(_s: &Snapshot, _rev: Option<String>) -> Self { Self::todo("Merge") }
    pub fn rebase(_s: &Snapshot, _onto: Option<String>) -> Self { Self::todo("Rebase") }
    /// Interactive rebase of the commits after `base` (exclusive) up to HEAD.
    pub fn interactive_rebase(_s: &Snapshot, _base: String) -> Self { Self::todo("Interactive rebase") }
    pub fn cherry_pick(_ids: Vec<String>) -> Self { Self::todo("Cherry-pick") }
    pub fn revert(_ids: Vec<String>) -> Self { Self::todo("Revert") }
    pub fn reset(_rev: String) -> Self { Self::todo("Reset") }

    // working copy
    pub fn discard_all() -> Self { Self::todo("Discard all") }
    pub fn discard_paths(_tracked: Vec<String>, _untracked: Vec<String>) -> Self { Self::todo("Discard") }
    pub fn stash() -> Self { Self::todo("Stash") }
    pub fn stash_paths(_paths: Vec<String>) -> Self { Self::todo("Stash selected") }
    pub fn stash_apply(_index: usize) -> Self { Self::todo("Apply stash") }
    pub fn stash_drop(_index: usize) -> Self { Self::todo("Drop stash") }
    pub fn stash_branch(_index: usize) -> Self { Self::todo("Branch from stash") }

    // tags
    pub fn tag(_rev: Option<String>) -> Self { Self::todo("Tag") }
    pub fn delete_tag(_name: String) -> Self { Self::todo("Delete tag") }
    pub fn push_tag(_name: String) -> Self { Self::todo("Push tag") }

    // misc
    pub fn create_patch(_ids: Vec<String>) -> Self { Self::todo("Create patch") }
    pub fn apply_patch() -> Self { Self::todo("Apply patch") }
    pub fn archive(_rev: String) -> Self { Self::todo("Archive") }
    /// Free-form git command line, output shown in the dialog.
    pub fn console() -> Self { Self::todo("Git console") }

    /// Renders the dialog; returns false when it should close.
    pub fn show(&mut self, ctx: &egui::Context, _tab: &mut RepoTab, _snap: &Snapshot, _shared: &mut Shared) -> bool {
        let Dialog::Todo(name) = self;
        let mut open = true;
        egui::Modal::new(egui::Id::new("todo-dialog")).show(ctx, |ui| {
            ui.label(format!("{name}: not implemented yet"));
            if ui.button("Close").clicked() {
                open = false;
            }
        });
        open
    }
}

pub enum GlobalDialog {
    Todo(String),
}

impl GlobalDialog {
    pub fn settings() -> Self { GlobalDialog::Todo("Settings".into()) }
    pub fn clone_repo() -> Self { GlobalDialog::Todo("Clone".into()) }
    pub fn init_repo() -> Self { GlobalDialog::Todo("Create repository".into()) }
    pub fn bookmark(_p: PathBuf) -> Self { GlobalDialog::Todo("Bookmark".into()) }
    pub fn about() -> Self { GlobalDialog::Todo("About".into()) }

    pub fn show(&mut self, ctx: &egui::Context, _shared: &mut Shared) -> bool {
        let GlobalDialog::Todo(name) = self;
        let mut open = true;
        egui::Modal::new(egui::Id::new("todo-global")).show(ctx, |ui| {
            ui.label(format!("{name}: not implemented yet"));
            if ui.button("Close").clicked() {
                open = false;
            }
        });
        open
    }
}
