use crate::{Conflict, Kind, Status, StatusBuilder, Untracked};
use anyhow::{Result, anyhow};
use bstr::BString;
use gix::status::Item;
use gix::status::index_worktree::Item as WtItem;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{e}")
}

/// In-process status via gitoxide. Index-vs-worktree, the directory walk and
/// HEAD-vs-index all run in parallel.
pub fn compute(
    workdir: &Path,
    patterns: &[BString],
    untracked: Untracked,
    interrupt: &Arc<AtomicBool>,
) -> Result<Status> {
    let repo = gix::open(workdir).map_err(err)?;
    compute_in(&repo, patterns, untracked, interrupt)
}

pub fn compute_in(
    repo: &gix::Repository,
    patterns: &[BString],
    untracked: Untracked,
    interrupt: &Arc<AtomicBool>,
) -> Result<Status> {
    let mode = match untracked {
        Untracked::All => gix::status::UntrackedFiles::Files,
        Untracked::Normal => gix::status::UntrackedFiles::Collapsed,
        Untracked::No => gix::status::UntrackedFiles::None,
    };
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(err)?
        .untracked_files(mode)
        .should_interrupt_owned(interrupt.clone())
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true })
        .into_iter(patterns.iter().cloned())
        .map_err(err)?;

    let mut b = StatusBuilder::new();
    for item in iter {
        let item = item.map_err(err)?;
        match item {
            Item::IndexWorktree(wt) => {
                if let Some((kind, dir)) = worktree_kind(&wt) {
                    let path: &[u8] = wt.rela_path();
                    // Collapsed untracked directories are reported with a trailing slash.
                    let slash: &[u8] = if dir && !path.ends_with(b"/") {
                        b"/"
                    } else {
                        b""
                    };
                    b.push_parts(false, &[path, slash], kind, None);
                }
            }
            Item::TreeIndex(change) => {
                use gix::diff::index::Change;
                match change {
                    Change::Addition { location, .. } => b.staged(&location, Kind::Added, None),
                    Change::Deletion { location, .. } => b.staged(&location, Kind::Deleted, None),
                    Change::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let kind = if previous_entry_mode.to_tree_entry_mode().map(|m| m.kind())
                            != entry_mode.to_tree_entry_mode().map(|m| m.kind())
                        {
                            Kind::TypeChange
                        } else {
                            Kind::Modified
                        };
                        b.staged(&location, kind, None)
                    }
                    Change::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => {
                        let kind = if copy { Kind::Copied } else { Kind::Renamed };
                        b.staged(&location, kind, Some(&source_location))
                    }
                }
            }
        }
    }
    Ok(b.finish())
}

/// Kind of a worktree item, and whether it is a collapsed directory.
fn worktree_kind(item: &WtItem) -> Option<(Kind, bool)> {
    use gix::status::index_worktree::iter::Summary;
    if let WtItem::Modification {
        status: gix::status::plumbing::index_as_worktree::EntryStatus::Conflict { summary, .. },
        ..
    } = item
    {
        use gix::status::plumbing::index_as_worktree::Conflict as C;
        let c = match summary {
            C::BothDeleted => Conflict::BothDeleted,
            C::AddedByUs => Conflict::AddedByUs,
            C::DeletedByThem => Conflict::DeletedByThem,
            C::AddedByThem => Conflict::AddedByThem,
            C::DeletedByUs => Conflict::DeletedByUs,
            C::BothAdded => Conflict::BothAdded,
            C::BothModified => Conflict::BothModified,
        };
        return Some((Kind::Conflict(c), false));
    }
    let kind = match item.summary()? {
        Summary::Removed => Kind::Deleted,
        Summary::Added => Kind::Untracked,
        Summary::Modified => Kind::Modified,
        Summary::TypeChange => Kind::TypeChange,
        Summary::Renamed => Kind::Renamed,
        Summary::Copied => Kind::Copied,
        Summary::IntentToAdd => Kind::IntentToAdd,
        Summary::Conflict => Kind::Conflict(Conflict::BothModified),
    };
    let dir = matches!(item, WtItem::DirectoryContents { entry: dir, .. }
        if dir.disk_kind == Some(gix::dir::entry::Kind::Directory));
    Some((kind, dir))
}
