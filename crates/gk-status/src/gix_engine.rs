use crate::{Conflict, Entry, Kind, Status, Untracked};
use anyhow::{Result, anyhow};
use bstr::BString;
use gix::status::index_worktree::Item as WtItem;
use gix::status::Item;
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

    let mut status = Status::default();
    for item in iter {
        let item = item.map_err(err)?;
        match item {
            Item::IndexWorktree(wt) => {
                if let Some(entry) = worktree_entry(&wt) {
                    status.unstaged.push(entry);
                }
            }
            Item::TreeIndex(change) => {
                use gix::diff::index::Change;
                let entry = match change {
                    Change::Addition { location, .. } => Entry::new(location.into_owned(), Kind::Added),
                    Change::Deletion { location, .. } => Entry::new(location.into_owned(), Kind::Deleted),
                    Change::Modification { location, previous_entry_mode, entry_mode, .. } => {
                        let kind = if previous_entry_mode.to_tree_entry_mode().map(|m| m.kind())
                            != entry_mode.to_tree_entry_mode().map(|m| m.kind())
                        {
                            Kind::TypeChange
                        } else {
                            Kind::Modified
                        };
                        Entry::new(location.into_owned(), kind)
                    }
                    Change::Rewrite { source_location, location, copy, .. } => Entry {
                        path: location.into_owned(),
                        kind: if copy { Kind::Copied } else { Kind::Renamed },
                        orig_path: Some(source_location.into_owned()),
                    },
                };
                status.staged.push(entry);
            }
        }
    }
    Ok(status)
}

fn worktree_entry(item: &WtItem) -> Option<Entry> {
    use gix::status::index_worktree::iter::Summary;
    let path = item.rela_path().to_owned();
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
        return Some(Entry::new(path, Kind::Conflict(c)));
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
    let mut entry = Entry::new(path, kind);
    // Collapsed untracked directories are reported with a trailing slash.
    if let WtItem::DirectoryContents { entry: dir, .. } = item
        && dir.disk_kind == Some(gix::dir::entry::Kind::Directory)
        && !entry.path.ends_with(b"/")
    {
        entry.path.push(b'/');
    }
    Some(entry)
}
