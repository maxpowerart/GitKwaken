//! Working-tree status: data model, two interchangeable engines
//! (in-process gitoxide and the git CLI), incremental merging and a
//! file-system watcher that turns OS events into refresh requests.

mod cli_engine;
pub mod fast;
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
        Self {
            path: path.into(),
            kind,
            orig_path: None,
        }
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
        self.unstaged
            .iter()
            .filter(|e| matches!(e.kind, Kind::Conflict(_)))
    }

    /// Replaces everything inside `scope` with the entries from `update`.
    ///
    /// `scope` holds repository-relative paths (files or directories); an
    /// entry is in scope when its path equals a scope path or lies below it.
    /// `staged_too` controls whether the staged side is replaced as well.
    pub fn merge(&mut self, update: Status, scope: &[BString], staged_too: bool) {
        let in_scope = |p: &BStr| scope.iter().any(|s| path_within(p, s.as_ref()));
        self.unstaged.retain(|e| !in_scope(e.path.as_ref()));
        self.unstaged.extend(
            update
                .unstaged
                .into_iter()
                .filter(|e| in_scope(e.path.as_ref())),
        );
        if staged_too {
            self.staged.retain(|e| !in_scope(e.path.as_ref()));
            self.staged.extend(
                update
                    .staged
                    .into_iter()
                    .filter(|e| in_scope(e.path.as_ref())),
            );
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

/// Whether git's builtin fsmonitor daemon serves this repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsMonitor {
    /// `core.fsmonitor` is unset or false.
    Off,
    /// Configured (a hook path or `true`) but the daemon is not running yet;
    /// the next `git status` starts it.
    Configured,
    /// `git fsmonitor--daemon status` reports it watching this worktree.
    Running,
}

/// Reports the fsmonitor state (two short git calls, ~50 ms on Windows: call
/// it off the UI thread). With [`Engine::Cli`], full scans of a repository
/// in state `Configured` or `Running` are answered by the daemon (see
/// `cli_engine::compute`).
pub fn fsmonitor_state(git: &gk_cli::Git) -> FsMonitor {
    let configured = git
        .run_raw(&["config", "--type=bool-or-str", "core.fsmonitor"], None)
        .ok()
        .filter(|o| o.success())
        .map(|o| o.stdout_str().trim().to_string())
        .is_some_and(|v| !v.is_empty() && v != "false");
    if !configured {
        return FsMonitor::Off;
    }
    match git.run_raw(&["fsmonitor--daemon", "status"], None) {
        Ok(o) if o.success() => FsMonitor::Running,
        _ => FsMonitor::Configured,
    }
}

/// Computes status, limited to `patterns` when non-empty.
pub fn compute(
    workdir: &std::path::Path,
    git: &gk_cli::Git,
    patterns: &[BString],
    opts: &Options,
    interrupt: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<Status> {
    let _span = tracing::info_span!(
        "status.compute",
        engine = ?opts.engine,
        paths = patterns.len()
    )
    .entered();
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
    fn fsmonitor_detection() {
        let dir = tempfile::tempdir().unwrap();
        let git = gk_cli::Git::new(dir.path(), gk_cli::CommandLog::default());
        git.run(&["init", "-q"]).unwrap();
        assert_eq!(fsmonitor_state(&git), FsMonitor::Off);
        git.run(&["config", "core.fsmonitor", "false"]).unwrap();
        assert_eq!(fsmonitor_state(&git), FsMonitor::Off);
        git.run(&["config", "core.fsmonitor", "true"]).unwrap();
        // Not started by anything yet (no status ran since).
        assert_ne!(fsmonitor_state(&git), FsMonitor::Off);
    }

    #[test]
    fn merge_replaces_scope() {
        let mut s = Status {
            unstaged: vec![
                Entry::new("a/x", Kind::Modified),
                Entry::new("b/y", Kind::Modified),
            ],
            ..Default::default()
        };
        let update = Status {
            unstaged: vec![Entry::new("a/z", Kind::Untracked)],
            ..Default::default()
        };
        s.merge(update, &["a".into()], false);
        let paths: Vec<_> = s.unstaged.iter().map(|e| e.path.to_string()).collect();
        assert_eq!(paths, ["a/z", "b/y"]);
    }
}
