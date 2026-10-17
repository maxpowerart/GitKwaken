//! Working-tree status: data model, two interchangeable engines
//! (in-process gitoxide and the git CLI), incremental merging and a
//! file-system watcher that turns OS events into refresh requests.

mod cli_engine;
mod gix_engine;
pub mod watch;

use bstr::{BStr, BString};
use std::time::Duration;

pub use cli_engine::compute as compute_cli;
pub use gix_engine::compute as compute_gix;

/// Which side of a conflict did what, mirroring `git status` short codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Conflict {
    BothDeleted,
    AddedByUs,
    DeletedByThem,
    AddedByThem,
    DeletedByUs,
    BothAdded,
    BothModified,
}

impl Conflict {
    pub fn label(self) -> &'static str {
        match self {
            Conflict::BothDeleted => "both deleted",
            Conflict::AddedByUs => "added by us",
            Conflict::DeletedByThem => "deleted by them",
            Conflict::AddedByThem => "added by them",
            Conflict::DeletedByUs => "deleted by us",
            Conflict::BothAdded => "both added",
            Conflict::BothModified => "both modified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    IntentToAdd,
    Conflict(Conflict),
}

impl Kind {
    /// Single-letter code as shown in file lists.
    pub fn code(self) -> char {
        match self {
            Kind::Added => 'A',
            Kind::Modified => 'M',
            Kind::Deleted => 'D',
            Kind::Renamed => 'R',
            Kind::Copied => 'C',
            Kind::TypeChange => 'T',
            Kind::Untracked => '?',
            Kind::IntentToAdd => 'A',
            Kind::Conflict(_) => 'U',
        }
    }
}

/// One changed path on one side (staged or unstaged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Repository-relative path with `/` separators.
    pub path: BString,
    pub kind: Kind,
    /// Source path for renames and copies.
    pub orig_path: Option<BString>,
}

impl Entry {
    pub fn new(path: impl Into<BString>, kind: Kind) -> Self {
        Self { path: path.into(), kind, orig_path: None }
    }
}

/// A complete status snapshot.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// HEAD vs index.
    pub staged: Vec<Entry>,
    /// Index vs worktree, including untracked files and conflicts.
    pub unstaged: Vec<Entry>,
    /// Time the computation took.
    pub took: Duration,
}

impl Status {
    pub fn sort(&mut self) {
        self.staged.sort_by(|a, b| a.path.cmp(&b.path));
        self.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
        self.staged.dedup_by(|a, b| a.path == b.path);
        self.unstaged.dedup_by(|a, b| a.path == b.path);
    }

    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &Entry> {
        self.unstaged.iter().filter(|e| matches!(e.kind, Kind::Conflict(_)))
    }

    /// Replaces everything inside `scope` with the entries from `update`.
    ///
    /// `scope` holds repository-relative paths (files or directories); an
    /// entry is in scope when its path equals a scope path or lies below it.
    /// `staged_too` controls whether the staged side is replaced as well.
    pub fn merge(&mut self, update: Status, scope: &[BString], staged_too: bool) {
        let in_scope = |p: &BStr| scope.iter().any(|s| path_within(p, s.as_ref()));
        self.unstaged.retain(|e| !in_scope(e.path.as_ref()));
        self.unstaged.extend(update.unstaged.into_iter().filter(|e| in_scope(e.path.as_ref())));
        if staged_too {
            self.staged.retain(|e| !in_scope(e.path.as_ref()));
            self.staged.extend(update.staged.into_iter().filter(|e| in_scope(e.path.as_ref())));
        }
        self.took = update.took;
        self.sort();
    }
}

/// True if `path` equals `dir` or is located below it.
pub fn path_within(path: &BStr, dir: &BStr) -> bool {
    if dir.is_empty() {
        return true;
    }
    path.starts_with(dir) && (path.len() == dir.len() || path[dir.len()] == b'/')
}

/// Engine selection; the CLI engine is the reference implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    Gix,
    /// Default: on Windows git's fscache makes the CLI the fastest full scan.
    #[default]
    Cli,
}

/// Untracked-file listing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Untracked {
    /// Every untracked file individually (`-uall`).
    #[default]
    All,
    /// Untracked directories collapsed into one entry (`-unormal`).
    Normal,
    No,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub engine: Engine,
    pub untracked: Untracked,
}

/// Computes status, limited to `patterns` when non-empty.
pub fn compute(
    workdir: &std::path::Path,
    git: &gk_cli::Git,
    patterns: &[BString],
    opts: &Options,
    interrupt: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<Status> {
    let start = std::time::Instant::now();
    let mut status = match opts.engine {
        Engine::Gix => gix_engine::compute(workdir, patterns, opts.untracked, interrupt)?,
        Engine::Cli => cli_engine::compute(git, patterns, opts.untracked)?,
    };
    status.sort();
    status.took = start.elapsed();
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within() {
        use bstr::ByteSlice;
        assert!(path_within(b"a/b/c".as_bstr(), b"a/b".as_bstr()));
        assert!(path_within(b"a/b".as_bstr(), b"a/b".as_bstr()));
        assert!(!path_within(b"a/bc".as_bstr(), b"a/b".as_bstr()));
        assert!(path_within(b"x".as_bstr(), b"".as_bstr()));
    }

    #[test]
    fn merge_replaces_scope() {
        let mut s = Status {
            unstaged: vec![Entry::new("a/x", Kind::Modified), Entry::new("b/y", Kind::Modified)],
            ..Default::default()
        };
        let update = Status { unstaged: vec![Entry::new("a/z", Kind::Untracked)], ..Default::default() };
        s.merge(update, &["a".into()], false);
        let paths: Vec<_> = s.unstaged.iter().map(|e| e.path.to_string()).collect();
        assert_eq!(paths, ["a/z", "b/y"]);
    }
}
