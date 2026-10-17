//! Stash view: the changes of one stash (the stash commit against its first
//! parent, plus untracked files saved in `stash^3`), with Apply / Pop / Drop.

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use crate::views::history::task::{Task, TaskState};
use egui::{RichText, Ui};
use gk_core::content::{ChangeKind, ChangedFile};
use gk_core::{Load, ObjectId, Snapshot};
use std::sync::Arc;

#[derive(Default)]
pub struct StashViewState {
    /// The stash commit shown.
    pub id: Option<ObjectId>,
    /// Files of `stash^3` (untracked files), keyed by that commit.
    untracked: Task<ObjectId, Vec<ChangedFile>>,
    /// Tracked + untracked files of `id`, built once both are loaded.
    files: Option<(ObjectId, Arc<Vec<ChangedFile>>)>,
}

/// Shows the stash with commit `id` (hex) in the stash view.
pub fn open(tab: &mut RepoTab, id: &str) {
    if let Ok(oid) = ObjectId::from_hex(id.trim().as_bytes()) {
        if tab.stash_view.id != Some(oid) {
            tab.stash_view.files = None;
        }
        tab.stash_view.id = Some(oid);
        tab.view = View::Stash;
    }
}

/// Parses `git ls-tree -r -z <tree-ish>` into added files.
pub fn parse_ls_tree(out: &str) -> Vec<ChangedFile> {
    out.split('\0')
        .filter_map(|rec| {
            let (meta, path) = rec.split_once('\t')?;
            let mut it = meta.split_whitespace();
            let (_mode, kind, oid) = (it.next()?, it.next()?, it.next()?);
            if kind != "blob" {
                return None;
            }
            Some(ChangedFile {
                path: path.to_string(),
                old_path: None,
                kind: ChangeKind::Added,
                old_id: None,
                new_id: ObjectId::from_hex(oid.as_bytes()).ok(),
            })
        })
        .collect()
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let Some(id) = tab.stash_view.id else {
        placeholder(ui, tab, "Select a stash in the sidebar to see its changes.");
        return;
    };
    let hex = id.to_string();
    let Some(stash) = snap.refs.stashes.iter().find(|s| s.id == hex).cloned() else {
        if snap.op.is_some() {
            ui.spinner();
        } else {
            placeholder(ui, tab, "This stash no longer exists.");
        }
        return;
    };
    let idx = stash.index;
    ui.horizontal(|ui| {
        if ui
            .button("Apply…")
            .on_hover_text("Apply the stash to the working copy")
            .clicked()
        {
            tab.dialog = Some(Dialog::stash_apply(idx));
        }
        if ui
            .button("Pop")
            .on_hover_text("Apply the stash and remove it")
            .clicked()
        {
            let name = format!("stash@{{{idx}}}");
            tab.session.run_op(format!("Pop {name}"), move |r, c, p| {
                r.git()
                    .run_streaming(&["stash", "pop", name.as_str()], None, c, p)?
                    .check()?;
                Ok(String::new())
            });
        }
        if ui
            .button("Drop…")
            .on_hover_text("Delete the stash")
            .clicked()
        {
            tab.dialog = Some(Dialog::stash_drop(idx));
        }
        if ui
            .button("Branch…")
            .on_hover_text("New branch from the stash")
            .clicked()
        {
            tab.dialog = Some(Dialog::stash_branch(idx));
        }
        ui.separator();
        ui.label(
            RichText::new(format!("stash@{{{idx}}}"))
                .strong()
                .monospace(),
        );
        ui.label(RichText::new(&stash.message).strong());
    });
    let d = match tab.session.commit_details(id) {
        Load::Ready(d) if d.id == id => d,
        Load::Failed(e) => {
            ui.colored_label(shared.palette.removed_fg, e);
            return;
        }
        _ => {
            ui.spinner();
            return;
        }
    };
    ui.horizontal(|ui| {
        let muted = shared.palette.muted;
        ui.label(
            RichText::new(crate::util::format_time(
                d.author_time,
                d.author_offset,
                false,
            ))
            .color(muted),
        );
        if let Some(base) = d.parents.first() {
            ui.label(RichText::new("on").color(muted));
            let b = base.to_string();
            ui.label(
                RichText::new(crate::util::short_id(&b))
                    .monospace()
                    .color(muted),
            );
            if let Some(m) = tab.session.metas(&[*base]).pop().flatten() {
                ui.label(RichText::new(&m.summary).color(muted));
            }
        }
    });
    // Untracked files (`git stash -u`) live in a third parent.
    let untracked = match d.parents.get(2).copied() {
        None => Some(Arc::new(Vec::new())),
        Some(u) => {
            let git = tab.session.repo().git().clone();
            match tab.stash_view.untracked.get(&u, ui.ctx(), move || {
                let h = u.to_string();
                let o = git.run(&["ls-tree", "-r", "-z", h.as_str()])?;
                Ok(parse_ls_tree(&o.stdout_str()))
            }) {
                TaskState::Done(v) => Some(v),
                TaskState::Failed(e) => {
                    ui.colored_label(shared.palette.removed_fg, e);
                    Some(Arc::new(Vec::new()))
                }
                _ => None,
            }
        }
    };
    let Some(untracked) = untracked else {
        ui.spinner();
        return;
    };
    let files = match &tab.stash_view.files {
        Some((fid, f)) if *fid == id => f.clone(),
        _ => {
            let mut v = d.files.clone();
            v.extend(untracked.iter().cloned());
            v.sort_by(|a, b| a.path.cmp(&b.path));
            let f = Arc::new(v);
            tab.stash_view.files = Some((id, f.clone()));
            f
        }
    };
    if !untracked.is_empty() {
        ui.label(
            RichText::new(format!("{} untracked file(s) included", untracked.len()))
                .color(shared.palette.muted),
        );
    }
    ui.separator();
    crate::views::details::files_view(ui, tab, snap, shared, &files, &hex, id);
}

fn placeholder(ui: &mut Ui, tab: &mut RepoTab, text: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(RichText::new(text).weak());
        if ui.link("Back to history").clicked() {
            tab.view = View::History;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_tree_blobs() {
        let out = "100644 blob 4ca56ea8275359cbf761c0ca70e915ac2d28f067\tdir/a b.txt\0\
                   160000 commit 9ef121074b65d1f768ada8c890282ab0aacdca6f\tsub\0";
        let v = parse_ls_tree(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "dir/a b.txt");
        assert_eq!(v[0].kind, ChangeKind::Added);
        assert!(v[0].new_id.is_some());
    }
}
