//! Loading the two sides of a file diff and writing rebuilt content back.

use crate::{Repo, repo::gix_err};
use anyhow::{Result, anyhow, bail};
use bstr::ByteSlice;
use gix::ObjectId;

/// Files larger than this are not diffed in-process.
pub const MAX_DIFF_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
    TypeChange,
}

impl ChangeKind {
    pub fn code(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Modified => 'M',
            ChangeKind::Renamed => 'R',
            ChangeKind::Copied => 'C',
            ChangeKind::TypeChange => 'T',
        }
    }
}

/// A file changed by a commit (or between two commits).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub old_id: Option<ObjectId>,
    pub new_id: Option<ObjectId>,
}

/// What a diff view shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffTarget {
    /// Index vs worktree (includes untracked and conflicted files).
    Unstaged { path: String },
    /// HEAD vs index.
    Staged {
        path: String,
        old_path: Option<String>,
    },
    /// Two blobs, e.g. a file in a commit vs its parent.
    Blobs {
        path: String,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
    },
}

impl DiffTarget {
    pub fn path(&self) -> &str {
        match self {
            DiffTarget::Unstaged { path }
            | DiffTarget::Staged { path, .. }
            | DiffTarget::Blobs { path, .. } => path,
        }
    }
}

pub fn blob(g: &gix::Repository, id: ObjectId) -> Result<Vec<u8>> {
    let obj = g.find_object(id).map_err(gix_err)?;
    if obj.data.len() as u64 > MAX_DIFF_BYTES {
        bail!(
            "file too large to diff ({} MB)",
            obj.data.len() / (1024 * 1024)
        );
    }
    Ok(obj.detach().data)
}

/// Blob of `path` in the index at `stage` (0 = normal, 2 = ours, 3 = theirs).
pub fn index_blob(g: &gix::Repository, path: &str, stage: u32) -> Result<Option<Vec<u8>>> {
    let index = g.index_or_empty().map_err(gix_err)?;
    let stage = match stage {
        1 => gix::index::entry::Stage::Base,
        2 => gix::index::entry::Stage::Ours,
        3 => gix::index::entry::Stage::Theirs,
        _ => gix::index::entry::Stage::Unconflicted,
    };
    match index.entry_by_path_and_stage(path.as_bytes().as_bstr(), stage) {
        Some(e) => Ok(Some(blob(g, e.id)?)),
        None => Ok(None),
    }
}

pub fn head_blob(g: &gix::Repository, path: &str) -> Result<Option<Vec<u8>>> {
    let Ok(commit) = g.head_commit() else {
        return Ok(None);
    };
    tree_blob(g, commit.tree_id().map_err(gix_err)?.detach(), path)
}

pub fn tree_blob(g: &gix::Repository, tree: ObjectId, path: &str) -> Result<Option<Vec<u8>>> {
    let tree = g.find_tree(tree).map_err(gix_err)?;
    match tree.lookup_entry_by_path(path).map_err(gix_err)? {
        Some(e) if e.mode().is_blob() || e.mode().is_link() => Ok(Some(blob(g, e.object_id())?)),
        _ => Ok(None),
    }
}

/// Worktree file converted to its git representation (CRLF and clean filters applied).
pub fn worktree_clean(repo: &Repo, g: &gix::Repository, path: &str) -> Result<Option<Vec<u8>>> {
    let abs = repo.abs(path);
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.is_symlink() {
        let target = std::fs::read_link(&abs)?;
        return Ok(Some(
            target.to_string_lossy().replace('\\', "/").into_bytes(),
        ));
    }
    if meta.len() > MAX_DIFF_BYTES {
        bail!("file too large to diff ({} MB)", meta.len() / (1024 * 1024));
    }
    let raw = std::fs::read(&abs)?;
    let (mut pipeline, index) = g.filter_pipeline(None).map_err(gix_err)?;
    let rel = std::path::PathBuf::from(path);
    let outcome = pipeline
        .convert_to_git(raw.as_slice(), &rel, &index)
        .map_err(gix_err)?;
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
    let mut out = Vec::new();
    match outcome {
        ToGitOutcome::Unchanged(_) => out = raw.clone(),
        ToGitOutcome::Buffer(b) => out.extend_from_slice(b),
        ToGitOutcome::Process(mut r) => {
            std::io::Read::read_to_end(&mut r, &mut out)?;
        }
    }
    Ok(Some(out))
}

/// Converts git-form content to the worktree form and writes it.
pub fn write_worktree(repo: &Repo, g: &gix::Repository, path: &str, data: &[u8]) -> Result<()> {
    let (mut pipeline, _index) = g.filter_pipeline(None).map_err(gix_err)?;
    let outcome = pipeline
        .convert_to_worktree(data, path.as_bytes().as_bstr(), Default::default())
        .map_err(gix_err)?;
    use gix::filter::plumbing::pipeline::convert::ToWorktreeOutcome;
    let bytes: Vec<u8> = match outcome {
        ToWorktreeOutcome::Unchanged(b) => b.to_vec(),
        ToWorktreeOutcome::Buffer(b) => b.to_vec(),
        ToWorktreeOutcome::Process(_) => return Err(anyhow!("delayed filters are not supported")),
    };
    std::fs::write(repo.abs(path), bytes)?;
    Ok(())
}

/// Old and new content of a diff; `None` means the side does not exist.
pub type Sides = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Old and new content for a diff target.
pub fn load_sides(repo: &Repo, target: &DiffTarget) -> Result<Sides> {
    let g = repo.gix();
    match target {
        DiffTarget::Unstaged { path } => {
            let mut old = index_blob(&g, path, 0)?;
            if old.is_none() {
                // conflicted: compare against "ours"
                old = index_blob(&g, path, 2)?;
            }
            let new = worktree_clean(repo, &g, path)?;
            Ok((old, new))
        }
        DiffTarget::Staged { path, old_path } => {
            let old = head_blob(&g, old_path.as_deref().unwrap_or(path))?;
            let new = index_blob(&g, path, 0)?;
            Ok((old, new))
        }
        DiffTarget::Blobs { old, new, .. } => {
            let o = old.map(|id| blob(&g, id)).transpose()?;
            let n = new.map(|id| blob(&g, id)).transpose()?;
            Ok((o, n))
        }
    }
}

static DETECT_RENAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Rename/copy detection for commit and comparison file lists (settings:
/// diff.renames). When on, git's own diff.renames configuration applies
/// (`false` turns it off, `copies` adds copies), with the similarity
/// threshold of [`set_rename_similarity`].
pub fn set_detect_renames(on: bool) {
    DETECT_RENAMES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Minimum similarity in percent (1-100, default 50) for a rename in commit
/// and comparison file lists and in status (settings: diff.rename_similarity);
/// see [`gk_status::set_rename_similarity`].
pub fn set_rename_similarity(percent: u8) {
    gk_status::set_rename_similarity(percent);
}

fn diff_options(g: &gix::Repository) -> gix::diff::Options {
    let rewrites = if DETECT_RENAMES.load(std::sync::atomic::Ordering::Relaxed) {
        let threshold = gk_status::rewrites();
        match gix::diff::new_rewrites(&g.config_snapshot(), true) {
            Ok((configured, true)) => configured.map(|r| gix::diff::Rewrites {
                percentage: threshold.percentage,
                ..r
            }),
            _ => Some(threshold),
        }
    } else {
        None
    };
    gix::diff::Options::default().with_rewrites(rewrites)
}

/// Files changed between two trees (`old` = None means the empty tree).
pub fn tree_changes(
    g: &gix::Repository,
    old: Option<ObjectId>,
    new: ObjectId,
) -> Result<Vec<ChangedFile>> {
    let new_tree = g.find_tree(new).map_err(gix_err)?;
    let old_tree = old.map(|o| g.find_tree(o)).transpose().map_err(gix_err)?;
    let changes = g
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), diff_options(g))
        .map_err(gix_err)?;
    use gix::object::tree::diff::ChangeDetached as C;
    let mut out = Vec::new();
    for c in changes {
        let f = match c {
            C::Addition {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                ChangedFile {
                    path: location.to_string(),
                    old_path: None,
                    kind: ChangeKind::Added,
                    old_id: None,
                    new_id: Some(id),
                }
            }
            C::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                ChangedFile {
                    path: location.to_string(),
                    old_path: None,
                    kind: ChangeKind::Deleted,
                    old_id: Some(id),
                    new_id: None,
                }
            }
            C::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                let kind = if previous_entry_mode.kind() != entry_mode.kind() {
                    ChangeKind::TypeChange
                } else {
                    ChangeKind::Modified
                };
                ChangedFile {
                    path: location.to_string(),
                    old_path: None,
                    kind,
                    old_id: Some(previous_id),
                    new_id: Some(id),
                }
            }
            C::Rewrite {
                source_location,
                source_id,
                location,
                id,
                copy,
                entry_mode,
                ..
            } => {
                if entry_mode.is_tree() {
                    continue;
                }
                ChangedFile {
                    path: location.to_string(),
                    old_path: Some(source_location.to_string()),
                    kind: if copy {
                        ChangeKind::Copied
                    } else {
                        ChangeKind::Renamed
                    },
                    old_id: Some(source_id),
                    new_id: Some(id),
                }
            }
        };
        out.push(f);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Full commit information for the details panel.
#[derive(Debug, Clone)]
pub struct CommitDetails {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    pub tree: ObjectId,
    pub author: String,
    pub author_email: String,
    pub author_time: i64,
    pub author_offset: i32,
    pub committer: String,
    pub committer_email: String,
    pub committer_time: i64,
    pub message: String,
    /// Changes against the first parent.
    pub files: Vec<ChangedFile>,
}

pub fn commit_details(repo: &Repo, id: ObjectId) -> Result<CommitDetails> {
    let g = repo.gix();
    let c = g.find_commit(id).map_err(gix_err)?;
    let author = c.author().map_err(gix_err)?;
    let committer = c.committer().map_err(gix_err)?;
    let at = author.time().unwrap_or_default();
    let ct = committer.time().unwrap_or_default();
    let parents: Vec<ObjectId> = c.parent_ids().map(|p| p.detach()).collect();
    let tree = c.tree_id().map_err(gix_err)?.detach();
    let parent_tree = match parents.first() {
        Some(p) => Some(
            g.find_commit(*p)
                .map_err(gix_err)?
                .tree_id()
                .map_err(gix_err)?
                .detach(),
        ),
        None => None,
    };
    let files = tree_changes(&g, parent_tree, tree)?;
    Ok(CommitDetails {
        id,
        parents,
        tree,
        author: author.name.to_string().trim().to_string(),
        author_email: author.email.to_string().trim().to_string(),
        author_time: at.seconds,
        author_offset: at.offset,
        committer: committer.name.to_string().trim().to_string(),
        committer_email: committer.email.to_string().trim().to_string(),
        committer_time: ct.seconds,
        message: c.message_raw_sloppy().to_string(),
        files,
    })
}

/// Changes between two arbitrary commits (`a` older, `b` newer).
pub fn compare_commits(repo: &Repo, a: ObjectId, b: ObjectId) -> Result<Vec<ChangedFile>> {
    let g = repo.gix();
    let ta = g
        .find_commit(a)
        .map_err(gix_err)?
        .tree_id()
        .map_err(gix_err)?
        .detach();
    let tb = g
        .find_commit(b)
        .map_err(gix_err)?
        .tree_id()
        .map_err(gix_err)?
        .detach();
    tree_changes(&g, Some(ta), tb)
}
