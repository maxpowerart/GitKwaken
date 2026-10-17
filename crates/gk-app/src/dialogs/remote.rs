//! Pull, push, fetch and remote management dialogs.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use gk_core::ops::{FetchOptions, PushOptions};

fn head_text(snap: &Snapshot) -> String {
    match &snap.refs.head {
        gk_core::Head::Branch(b) | gk_core::Head::Unborn(b) => b.clone(),
        gk_core::Head::Detached(id) => format!("detached HEAD ({})", crate::util::short_id(id)),
        gk_core::Head::Unknown => "?".into(),
    }
}

fn no_remotes(ui: &mut egui::Ui, ctl: &mut Ctl) {
    error(
        ui,
        "This repository has no remotes. Add one first (Repository ▸ Add remote).",
    );
    ctl.invalid();
}

// ---------------------------------------------------------------- pull

pub struct Pull {
    remote: String,
    /// Remote branch name without the remote prefix; empty = git's default.
    branch: String,
    rebase: bool,
    /// The user picked the remote or branch: keep it across refreshes.
    touched: bool,
    watch: RefsWatch,
}

impl Pull {
    pub fn new(s: &Snapshot) -> Self {
        let mut p = Self {
            remote: String::new(),
            branch: String::new(),
            rebase: false,
            touched: false,
            watch: RefsWatch::default(),
        };
        p.sync(s);
        p
    }

    /// Defaults (current branch's upstream) until the user picks something;
    /// afterwards only drops choices that no longer exist.
    fn sync(&mut self, s: &Snapshot) {
        if !self.watch.changed(s) || !refs_loaded(s) {
            return;
        }
        let remotes = remote_names(s);
        if self.touched {
            if !remotes.contains(&self.remote) {
                self.remote = default_remote(&remotes);
                self.branch.clear();
            }
            if !s
                .refs
                .remote_branches
                .iter()
                .any(|rb| rb.remote == self.remote && rb.name == self.branch)
            {
                self.branch.clear();
            }
            return;
        }
        let (remote, branch) = pull_defaults(s, &remotes);
        self.remote = remote;
        self.branch = branch;
    }
}

/// (remote, branch) to pull: the upstream of the current branch, else the
/// default remote and a remote branch of the same name.
fn pull_defaults(s: &Snapshot, remotes: &[String]) -> (String, String) {
    if let Some(u) = s.refs.current_branch().and_then(|b| b.upstream.clone())
        && let Some((r, b)) = split_remote_ref(remotes, &u)
    {
        return (r, b);
    }
    let remote = default_remote(remotes);
    let branch = s
        .refs
        .head
        .branch()
        .filter(|cur| {
            s.refs
                .remote_branches
                .iter()
                .any(|rb| rb.remote == remote && rb.name == *cur)
        })
        .unwrap_or_default()
        .to_string();
    (remote, branch)
}

impl TabDialog for Pull {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        self.sync(snap);
        let remotes = remote_names(snap);
        let (out, ()) = modal(ctx, "pull", "Pull", 520.0, "Pull", |ui, ctl| {
            egui::Grid::new("pull-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Pull from remote:");
                    if combo(ui, "pull-remote", &mut self.remote, &remotes, None, 320.0) {
                        self.branch.clear();
                        self.touched = true;
                    }
                    ui.end_row();
                    ui.label("Remote branch to pull:");
                    let branches: Vec<String> = snap
                        .refs
                        .remote_branches
                        .iter()
                        .filter(|b| b.remote == self.remote)
                        .map(|b| b.name.clone())
                        .collect();
                    if combo(
                        ui,
                        "pull-branch",
                        &mut self.branch,
                        &branches,
                        Some("(tracked branch)"),
                        320.0,
                    ) {
                        self.touched = true;
                    }
                    ui.end_row();
                    ui.label("Pull into local branch:");
                    ui.label(RichText::new(head_text(snap)).strong());
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.checkbox(&mut self.rebase, "Rebase instead of merge");
            if self.rebase {
                hint(
                    ui,
                    "Local commits are replayed on top of the remote branch (rewrites unpushed commits).",
                );
            }
            if remotes.is_empty() {
                no_remotes(ui, ctl);
            }
            if matches!(snap.refs.head, gk_core::Head::Detached(_)) && self.branch.is_empty() {
                error(ui, "HEAD is detached: choose a remote branch to pull.");
                ctl.invalid();
            }
            if snap.refs.state != gk_core::RepoState::Clean {
                warning(
                    ui,
                    format!("An operation is in progress ({}).", snap.refs.state.label()),
                );
            }
        });
        finish(out, || {
            let remote = self.remote.clone();
            let branch = (!self.branch.is_empty()).then(|| self.branch.clone());
            let rebase = self.rebase;
            let label = match &branch {
                Some(b) => format!("Pull {remote}/{b}"),
                None => format!("Pull {remote}"),
            };
            tab.session.run_op(label, move |repo, cancel, p| {
                repo.pull(Some(&remote), branch.as_deref(), rebase, cancel, p)
            });
        })
    }
}

// ---------------------------------------------------------------- push

struct PushRow {
    branch: String,
    upstream: Option<String>,
    push: bool,
    remote_name: String,
    track: bool,
}

pub struct Push {
    remote: String,
    rows: Vec<PushRow>,
    tags: bool,
    force: bool,
    force_confirmed: bool,
    /// The user picked the remote: keep it across refreshes.
    remote_touched: bool,
    watch: RefsWatch,
}

impl Push {
    pub fn new(s: &Snapshot) -> Self {
        let mut p = Self {
            remote: String::new(),
            rows: Vec::new(),
            tags: false,
            force: false,
            force_confirmed: false,
            remote_touched: false,
            watch: RefsWatch::default(),
        };
        p.sync(s);
        p
    }

    /// Rebuilds the branch rows from the current refs, keeping the user's
    /// choices for branches that still exist.
    fn sync(&mut self, s: &Snapshot) {
        if !self.watch.changed(s) || !refs_loaded(s) {
            return;
        }
        let remotes = remote_names(s);
        let old_remote = self.remote.clone();
        if !self.remote_touched || !remotes.contains(&self.remote) {
            self.remote = s
                .refs
                .current_branch()
                .and_then(|b| b.upstream.as_deref())
                .and_then(|u| split_remote_ref(&remotes, u))
                .map(|(r, _)| r)
                .unwrap_or_else(|| default_remote(&remotes));
        }
        let mut old = std::mem::take(&mut self.rows);
        for b in &s.refs.branches {
            match old.iter().position(|r| r.branch == b.name) {
                Some(i) => {
                    let mut r = old.swap_remove(i);
                    r.upstream = b.upstream.clone();
                    self.rows.push(r);
                }
                None => {
                    let mut r = PushRow {
                        branch: b.name.clone(),
                        upstream: b.upstream.clone(),
                        push: b.is_head,
                        remote_name: String::new(),
                        track: false,
                    };
                    r.update_name(&remotes, &self.remote);
                    self.rows.push(r);
                }
            }
        }
        if self.remote != old_remote {
            self.update_names(&remotes);
        }
    }

    /// Remote branch names: the upstream's name on the chosen remote, else the same name.
    fn update_names(&mut self, remotes: &[String]) {
        for r in &mut self.rows {
            r.update_name(remotes, &self.remote);
        }
    }
}

impl PushRow {
    fn update_name(&mut self, remotes: &[String], remote: &str) {
        match self
            .upstream
            .as_deref()
            .and_then(|u| split_remote_ref(remotes, u))
        {
            Some((rm, b)) if rm == remote => {
                self.remote_name = b;
                self.track = false;
            }
            _ => {
                self.remote_name = self.branch.clone();
                self.track = self.upstream.is_none();
            }
        }
    }
}

impl TabDialog for Push {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        self.sync(snap);
        let remotes = remote_names(snap);
        let confirm_force = shared.settings.confirmations.force_push;
        let (out, ()) = modal(ctx, "push", "Push", 640.0, "Push", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Push to repository:");
                if combo(ui, "push-remote", &mut self.remote, &remotes, None, 260.0) {
                    self.remote_touched = true;
                    self.update_names(&remotes);
                }
            });
            if let Some(r) = snap.refs.remotes.iter().find(|r| r.name == self.remote) {
                let url = if r.push_url.is_empty() {
                    &r.fetch_url
                } else {
                    &r.push_url
                };
                ui.add(egui::Label::new(RichText::new(url).weak().small()).truncate());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Branches to push:");
                if ui.small_button("All").clicked() {
                    self.rows.iter_mut().for_each(|r| r.push = true);
                }
                if ui.small_button("None").clicked() {
                    self.rows.iter_mut().for_each(|r| r.push = false);
                }
            });
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("push-rows")
                            .num_columns(4)
                            .striped(true)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("Push").strong());
                                ui.label(RichText::new("Local branch").strong());
                                ui.label(RichText::new("Remote branch").strong());
                                ui.label(RichText::new("Track").strong())
                                    .on_hover_text("Set as upstream (git push -u)");
                                ui.end_row();
                                for r in &mut self.rows {
                                    ui.checkbox(&mut r.push, "");
                                    let b = snap.refs.branch(&r.branch);
                                    let mut label = r.branch.clone();
                                    if let Some(b) =
                                        b.filter(|b| b.upstream.is_some() && b.ahead > 0)
                                    {
                                        label.push_str(&format!("  ↑{}", b.ahead));
                                    }
                                    ui.label(label);
                                    // Grid cells only offer last frame's column
                                    // width: size the field explicitly.
                                    let h = ui.spacing().interact_size.y;
                                    ui.add_enabled_ui(r.push, |ui| {
                                        ui.add_sized(
                                            [240.0, h],
                                            egui::TextEdit::singleline(&mut r.remote_name),
                                        );
                                    });
                                    ui.add_enabled(
                                        r.push,
                                        egui::Checkbox::without_text(&mut r.track),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            });
            ui.checkbox(&mut self.tags, "Push all tags");
            ui.checkbox(&mut self.force, "Force push (--force-with-lease)");
            if self.force {
                ui.colored_label(
                    DANGER,
                    "Force pushing replaces the history of the remote branches. Commits pushed by others \
                     that you have fetched but not integrated will be lost.",
                );
                if confirm_force {
                    ui.checkbox(
                        &mut self.force_confirmed,
                        RichText::new("I understand, overwrite the remote history").color(DANGER),
                    );
                    if !self.force_confirmed {
                        ctl.invalid();
                    }
                }
                ctl.danger = true;
                ctl.ok_label = "Force push".into();
            }
            if remotes.is_empty() {
                no_remotes(ui, ctl);
            }
            let selected: Vec<&PushRow> = self.rows.iter().filter(|r| r.push).collect();
            if selected.is_empty() && !self.tags {
                hint(ui, "Select at least one branch or tags to push.");
                ctl.invalid();
            }
            for r in &selected {
                if let Err(e) = check_branch_name(r.remote_name.trim()) {
                    error(ui, format!("{}: remote branch name: {e}", r.branch));
                    ctl.invalid();
                }
            }
            if selected.iter().any(|r| r.track) {
                hint(
                    ui,
                    "Tracked branches get the chosen remote branch as upstream.",
                );
            }
        });
        finish(out, || {
            let remote = self.remote.clone();
            let mut tracked = Vec::new();
            let mut plain = Vec::new();
            for r in self.rows.iter().filter(|r| r.push) {
                let spec = (r.branch.clone(), r.remote_name.trim().to_string());
                if r.track {
                    tracked.push(spec)
                } else {
                    plain.push(spec)
                }
            }
            let (force, tags) = (self.force, self.tags);
            let label = if force {
                format!("Force push to {remote}")
            } else {
                format!("Push to {remote}")
            };
            tab.session.run_op(label, move |repo, cancel, p| {
                let mut out = String::new();
                for (refspecs, set_upstream) in [(tracked, true), (plain, false)] {
                    if refspecs.is_empty() {
                        continue;
                    }
                    let o = PushOptions {
                        remote: remote.clone(),
                        refspecs,
                        force,
                        set_upstream,
                        tags: false,
                        no_verify: false,
                    };
                    out.push_str(&repo.push(&o, cancel, &mut *p)?);
                }
                if tags {
                    let o = repo
                        .git()
                        .run_streaming(
                            &["push", "--progress", &remote, "--tags"],
                            None,
                            cancel,
                            &mut *p,
                        )?
                        .check()?;
                    out.push_str(&o.stderr_str());
                }
                Ok(out)
            });
        })
    }
}

// ---------------------------------------------------------------- fetch

pub struct Fetch {
    /// Empty = all remotes.
    remote: String,
    prune: bool,
    tags: bool,
}

impl Fetch {
    pub fn new(_s: &Snapshot) -> Self {
        Self {
            remote: String::new(),
            prune: false,
            tags: false,
        }
    }
}

impl TabDialog for Fetch {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let remotes = remote_names(snap);
        let (out, ()) = modal(ctx, "fetch", "Fetch", 420.0, "Fetch", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Fetch from:");
                combo(
                    ui,
                    "fetch-remote",
                    &mut self.remote,
                    &remotes,
                    Some("All remotes"),
                    220.0,
                );
            });
            ui.checkbox(
                &mut self.prune,
                "Prune tracking branches no longer on the remote",
            );
            ui.checkbox(&mut self.tags, "Fetch all tags");
            if remotes.is_empty() {
                no_remotes(ui, ctl);
            }
        });
        finish(out, || {
            let remote = (!self.remote.is_empty()).then(|| self.remote.clone());
            let opts = FetchOptions {
                prune: self.prune,
                tags: self.tags,
            };
            let label = match &remote {
                Some(r) => format!("Fetch {r}"),
                None => "Fetch all remotes".to_string(),
            };
            tab.session.run_op(label, move |repo, cancel, p| {
                repo.fetch(remote.as_deref(), &opts, cancel, p)
            });
        })
    }
}

// ---------------------------------------------------------------- remotes

pub struct AddRemote {
    name: String,
    url: String,
    fetch: bool,
}

impl Default for AddRemote {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            fetch: true,
        }
    }
}

impl TabDialog for AddRemote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.name.is_empty() && snap.refs.remotes.is_empty() {
            self.name = "origin".into();
        }
        let (out, ()) = modal(ctx, "add-remote", "Add remote", 480.0, "Add", |ui, ctl| {
            egui::Grid::new("add-remote-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(320.0));
                    ctl.focus(&r);
                    ui.end_row();
                    ui.label("URL / path:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .desired_width(320.0)
                            .hint_text("https://… or git@host:user/repo.git"),
                    );
                    ui.end_row();
                });
            ui.checkbox(&mut self.fetch, "Fetch immediately");
            let name = self.name.trim();
            if let Err(e) = check_remote_name(name) {
                if !name.is_empty() {
                    error(ui, e);
                }
                ctl.invalid();
            } else if snap.refs.remotes.iter().any(|r| r.name == name) {
                error(ui, format!("A remote named '{name}' already exists"));
                ctl.invalid();
            }
            if self.url.trim().is_empty() {
                ctl.invalid();
            } else if self.url.trim().starts_with('-') {
                error(ui, "URL cannot start with '-'");
                ctl.invalid();
            }
        });
        finish(out, || {
            let (name, url, fetch) = (
                self.name.trim().to_string(),
                self.url.trim().to_string(),
                self.fetch,
            );
            tab.session
                .run_op(format!("Add remote {name}"), move |repo, cancel, p| {
                    repo.add_remote(&name, &url)?;
                    if fetch {
                        return repo.fetch(Some(&name), &FetchOptions::default(), cancel, p);
                    }
                    Ok(String::new())
                });
        })
    }
}

pub struct EditRemote {
    old: String,
    name: String,
    url: String,
    old_url: Option<String>,
}

impl EditRemote {
    pub fn new(name: String) -> Self {
        Self {
            old: name.clone(),
            name,
            url: String::new(),
            old_url: None,
        }
    }
}

impl TabDialog for EditRemote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.old_url.is_none() && refs_loaded(snap) {
            let u = snap
                .refs
                .remotes
                .iter()
                .find(|r| r.name == self.old)
                .map(|r| r.fetch_url.clone())
                .unwrap_or_default();
            self.url = u.clone();
            self.old_url = Some(u);
        }
        let title = format!("Edit remote '{}'", self.old);
        let (out, ()) = modal(ctx, "edit-remote", &title, 480.0, "Save", |ui, ctl| {
            egui::Grid::new("edit-remote-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(320.0));
                    ui.end_row();
                    ui.label("URL / path:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(320.0));
                    ctl.focus(&r);
                    ui.end_row();
                });
            let name = self.name.trim();
            if let Err(e) = check_remote_name(name) {
                error(ui, e);
                ctl.invalid();
            } else if name != self.old && snap.refs.remotes.iter().any(|r| r.name == name) {
                error(ui, format!("A remote named '{name}' already exists"));
                ctl.invalid();
            }
            if self.url.trim().is_empty() {
                error(ui, "URL is empty");
                ctl.invalid();
            } else if self.url.trim().starts_with('-') {
                error(ui, "URL cannot start with '-'");
                ctl.invalid();
            }
            if name != self.old {
                hint(
                    ui,
                    "Renaming also renames the remote-tracking branches and upstream settings.",
                );
            }
        });
        finish(out, || {
            let old = self.old.clone();
            let name = self.name.trim().to_string();
            let url = self.url.trim().to_string();
            let url_changed = Some(&url) != self.old_url.as_ref();
            if name == old && !url_changed {
                return;
            }
            tab.session
                .run_op(format!("Edit remote {old}"), move |repo, _, _| {
                    if name != old {
                        repo.rename_remote(&old, &name)?;
                    }
                    if url_changed {
                        repo.set_remote_url(&name, &url)?;
                    }
                    Ok(String::new())
                });
        })
    }
}

pub struct RemoveRemote {
    pub name: String,
}

impl TabDialog for RemoveRemote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let (out, ()) = modal(
            ctx,
            "remove-remote",
            "Remove remote",
            420.0,
            "Remove",
            |ui, ctl| {
                ui.label(format!("Remove the remote '{}'?", self.name));
                hint(
                    ui,
                    "Its remote-tracking branches and configuration are deleted locally. Nothing changes on the server.",
                );
                ctl.danger = true;
            },
        );
        finish(out, || {
            let name = self.name.clone();
            tab.session
                .run_op(format!("Remove remote {name}"), move |repo, _, _| {
                    repo.remove_remote(&name)?;
                    Ok(String::new())
                });
        })
    }
}

// ---------------------------------------------------------------- remote branches

pub struct CheckoutRemote {
    remote_branch: String,
    local: Option<String>,
    track: bool,
}

impl CheckoutRemote {
    pub fn new(remote_branch: String) -> Self {
        Self {
            remote_branch,
            local: None,
            track: true,
        }
    }
}

impl TabDialog for CheckoutRemote {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let local = self.local.get_or_insert_with(|| {
            split_remote_ref(&remote_names(snap), &self.remote_branch)
                .map(|(_, b)| b)
                .unwrap_or_else(|| self.remote_branch.clone())
        });
        let mut checkout_existing = false;
        let (out, ()) = modal(
            ctx,
            "checkout-remote",
            "Checkout remote branch",
            460.0,
            "Checkout",
            |ui, ctl| {
                egui::Grid::new("checkout-remote-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Remote branch:");
                        ui.label(RichText::new(&self.remote_branch).strong());
                        ui.end_row();
                        ui.label("New local branch:");
                        let r = ui.add(egui::TextEdit::singleline(local).desired_width(300.0));
                        ctl.focus(&r);
                        ui.end_row();
                    });
                ui.checkbox(&mut self.track, "Track the remote branch (set upstream)");
                let name = local.trim();
                if let Err(e) = check_branch_name(name) {
                    error(ui, e);
                    ctl.invalid();
                } else if snap.refs.branch(name).is_some() {
                    error(ui, format!("A local branch named '{name}' already exists."));
                    if ui
                        .button(format!("Checkout existing '{name}' instead"))
                        .clicked()
                    {
                        checkout_existing = true;
                        ctl.close = true;
                    }
                    ctl.invalid();
                }
                if worktree_dirty(snap) {
                    warning(
                        ui,
                        "You have uncommitted changes; they are carried over unless they conflict.",
                    );
                }
            },
        );
        let local = local.trim().to_string();
        if checkout_existing {
            tab.session
                .run_op(format!("Checkout {local}"), move |repo, _, _| {
                    repo.checkout(&local)?;
                    Ok(String::new())
                });
            return false;
        }
        finish(out, || {
            let rb = self.remote_branch.clone();
            let track = self.track;
            tab.session
                .run_op(format!("Checkout {rb}"), move |repo, _, _| {
                    if track {
                        repo.checkout_remote(&rb, &local)?;
                    } else {
                        repo.git()
                            .run(&["checkout", "--no-track", "-b", &local, &rb])?;
                    }
                    Ok(String::new())
                });
        })
    }
}

pub struct DeleteRemoteBranch {
    pub remote: String,
    pub branch: String,
}

impl TabDialog for DeleteRemoteBranch {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let (out, ()) = modal(
            ctx,
            "delete-remote-branch",
            "Delete remote branch",
            460.0,
            "Delete",
            |ui, ctl| {
                ui.label("Delete this branch on the server?");
                ui.label(RichText::new(format!("{}/{}", self.remote, self.branch)).strong());
                ui.colored_label(
                    DANGER,
                    "This removes the branch for everyone using the remote.",
                );
                ctl.danger = true;
            },
        );
        finish(out, || {
            let (remote, branch) = (self.remote.clone(), self.branch.clone());
            tab.session.run_op(
                format!("Delete {remote}/{branch}"),
                move |repo, cancel, p| {
                    repo.delete_remote_branch(&remote, &branch, cancel, p)?;
                    Ok(String::new())
                },
            );
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_core::refs::{Branch, Remote, RemoteBranch};
    use std::sync::Arc;

    fn branch(name: &str, upstream: Option<&str>, is_head: bool) -> Branch {
        Branch {
            name: name.into(),
            target: "0".repeat(40),
            upstream: upstream.map(Into::into),
            ahead: 0,
            behind: 0,
            upstream_gone: false,
            is_head,
        }
    }

    fn loaded() -> Snapshot {
        let mut refs = gk_core::Refs {
            head: gk_core::Head::Branch("main".into()),
            ..Default::default()
        };
        refs.branches = vec![
            branch("main", Some("upstream/main"), true),
            branch("topic", None, false),
        ];
        refs.remotes = ["origin", "upstream"]
            .map(|n| Remote {
                name: n.into(),
                fetch_url: format!("https://example.com/{n}.git"),
                push_url: String::new(),
            })
            .to_vec();
        refs.remote_branches = ["origin", "upstream"]
            .map(|r| RemoteBranch {
                remote: r.into(),
                name: "main".into(),
                target: "0".repeat(40),
            })
            .to_vec();
        Snapshot {
            refs: Arc::new(refs),
            ..Default::default()
        }
    }

    /// A new snapshot with the same refs (as after a refresh).
    fn refreshed(s: &Snapshot) -> Snapshot {
        Snapshot {
            refs: Arc::new((*s.refs).clone()),
            ..Default::default()
        }
    }

    #[test]
    fn push_fills_in_when_refs_arrive() {
        let mut p = Push::new(&Snapshot::default());
        assert!(p.remote.is_empty() && p.rows.is_empty());
        let s = loaded();
        p.sync(&s);
        assert_eq!(p.remote, "upstream", "remote of the current upstream");
        let rows: Vec<_> = p
            .rows
            .iter()
            .map(|r| (r.branch.as_str(), r.push, r.remote_name.as_str(), r.track))
            .collect();
        assert_eq!(
            rows,
            [
                ("main", true, "main", false),
                ("topic", false, "topic", true)
            ]
        );
        // User choices survive a refresh.
        p.rows[1].push = true;
        p.rows[1].remote_name = "topic-2".into();
        p.remote_touched = true;
        p.remote = "origin".into();
        p.sync(&refreshed(&s));
        assert_eq!(p.remote, "origin");
        assert!(p.rows[1].push);
        assert_eq!(p.rows[1].remote_name, "topic-2");
    }

    #[test]
    fn pull_fills_in_when_refs_arrive() {
        let mut p = Pull::new(&Snapshot::default());
        assert!(p.remote.is_empty());
        let s = loaded();
        p.sync(&s);
        assert_eq!((p.remote.as_str(), p.branch.as_str()), ("upstream", "main"));
        p.remote = "origin".into();
        p.branch.clear();
        p.touched = true;
        p.sync(&refreshed(&s));
        assert_eq!((p.remote.as_str(), p.branch.as_str()), ("origin", ""));
    }
}
