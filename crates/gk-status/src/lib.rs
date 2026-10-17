//! Working-tree status: data model, two interchangeable engines
//! (in-process gitoxide and the git CLI), incremental merging and a
//! file-system watcher that turns OS events into refresh requests.

mod cli_engine;
pub mod fast;
mod gix_engine;
mod path;
pub mod watch;

use bstr::{BStr, BString};
use std::time::Duration;

pub use cli_engine::compute as compute_cli;
pub use gix_engine::compute as compute_gix;
pub use path::{PathStr, StatusBuilder};

static RENAME_SIMILARITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(50);

/// Minimum similarity in percent (1-100, git's default is 50) for a deleted
/// and an added file to count as a rename, in both status engines
/// (`git status --find-renames=<n>%`) and, through `gk-core`, in commit
/// file lists. Settings: diff.rename_similarity.
pub fn set_rename_similarity(percent: u8) {
    RENAME_SIMILARITY.store(percent.clamp(1, 100), std::sync::atomic::Ordering::Relaxed);
}

pub fn rename_similarity() -> u8 {
    RENAME_SIMILARITY.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`rename_similarity`] as gitoxide rename tracking options.
pub fn rewrites() -> gix::diff::Rewrites {
    gix::diff::Rewrites {
        percentage: Some(f32::from(rename_similarity()) / 100.0),
        ..Default::default()
    }
}

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
    pub path: PathStr,
    pub kind: Kind,
    /// Source path for renames and copies.
    pub orig_path: Option<PathStr>,
}

impl Entry {
    pub fn new(path: impl Into<PathStr>, kind: Kind) -> Self {
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
    /// The last full scan was answered by `git status` with `core.fsmonitor`
    /// in effect (see [`compute`]). Kept across partial merges.
    pub fsmonitor: bool,
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

/// Starts (`true`) or stops git's builtin fsmonitor daemon for the worktree
/// of `git` (`git fsmonitor--daemon start|stop`). Starting returns once the
/// daemon listens; it keeps running after GitKwaken exits, like one started
/// by `git status`.
pub fn fsmonitor_daemon(git: &gk_cli::Git, start: bool) -> anyhow::Result<()> {
    let verb = if start { "start" } else { "stop" };
    git.run(&["fsmonitor--daemon", verb])?;
    Ok(())
}

/// What a `core.fsmonitor` value asks for, as git reads it
/// (`--type=bool-or-str`): `true` is the builtin daemon, any other non-false
/// value is the path of a hook (e.g. Watchman's).
fn fsmonitor_value_active(value: &str) -> bool {
    let v = value.trim();
    match v.to_ascii_lowercase().as_str() {
        "" | "false" | "no" | "off" | "0" => false,
        // The builtin daemon exists on Windows and macOS only; elsewhere git
        // warns and scans normally, and the gix engine is the faster scan.
        "true" | "yes" | "on" | "1" => cfg!(any(windows, target_os = "macos")),
        _ => true,
    }
}

/// `core.fsmonitor` is set to something `git status` would use here.
fn fsmonitor_configured(repo: &gix::Repository) -> bool {
    repo.config_snapshot()
        .string("core.fsmonitor")
        .is_some_and(|v| fsmonitor_value_active(&v.to_string()))
}

/// Computes status, limited to `patterns` when non-empty.
///
/// fsmonitor: gitoxide has no client for git's fsmonitor daemon (nor for
/// fsmonitor hooks): it reads the index extension but stats every entry. So
/// when the repository has `core.fsmonitor` set (the Optimize dialog does
/// that), full scans with [`Engine::Gix`] are handed to the CLI engine, whose
/// `git status` asks the daemon which paths changed and skips the rest.
/// Pathspec-limited scans stay in-process: they only stat their scope.
/// [`Status::fsmonitor`] tells which scans went through the daemon.
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
    let full = patterns.is_empty();
    let mut status = match opts.engine {
        Engine::Gix => {
            let repo = gix::open(workdir).map_err(|e| anyhow::anyhow!("{e}"))?;
            if full && fsmonitor_configured(&repo) {
                let mut s = cli_engine::compute(git, patterns, opts.untracked)?;
                s.fsmonitor = true;
                s
            } else {
                gix_engine::compute_in(&repo, patterns, opts.untracked, interrupt)?
            }
        }
        Engine::Cli => {
            let mut s = cli_engine::compute(git, patterns, opts.untracked)?;
            s.fsmonitor = full
                && gix::open(workdir)
                    .ok()
                    .is_some_and(|r| fsmonitor_configured(&r));
            s
        }
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
    fn fsmonitor_values() {
        for off in ["", "false", "No", "off", "0"] {
            assert!(!fsmonitor_value_active(off), "{off}");
        }
        assert_eq!(
            fsmonitor_value_active("true"),
            cfg!(any(windows, target_os = "macos"))
        );
        assert!(fsmonitor_value_active(".git/hooks/query-watchman"));
    }

    /// With `core.fsmonitor` on, a full scan with the default (gix) engine
    /// goes through `git status` and the daemon, with the same result.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn gix_engine_full_scan_uses_fsmonitor() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let git = gk_cli::Git::new(dir.path(), gk_cli::CommandLog::default());
        git.run(&["init", "-q"]).unwrap();
        git.run(&["config", "core.autocrlf", "false"]).unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git.run(&["add", "a.txt"]).unwrap();
        let opts = Options {
            engine: Engine::Gix,
            untracked: Untracked::All,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let plain = compute(dir.path(), &git, &[], &opts, &stop).unwrap();
        assert!(!plain.fsmonitor);

        git.run(&["config", "core.fsmonitor", "true"]).unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let via = compute(dir.path(), &git, &[], &opts, &stop).unwrap();
            assert!(via.fsmonitor);
            assert_eq!(via.staged, plain.staged);
            assert_eq!(via.unstaged, plain.unstaged);
            // The scan started the daemon; stop/start round-trips.
            fsmonitor_daemon(&git, false).unwrap();
            assert_eq!(fsmonitor_state(&git), FsMonitor::Configured);
            fsmonitor_daemon(&git, true).unwrap();
            assert_eq!(fsmonitor_state(&git), FsMonitor::Running);
            // Scoped scans stay in-process.
            let scoped = compute(dir.path(), &git, &["b.txt".into()], &opts, &stop).unwrap();
            assert!(!scoped.fsmonitor);
        }));
        // Do not leave a daemon holding the temp dir open.
        let _ = git.run_raw(&["fsmonitor--daemon", "stop"], None);
        r.unwrap();
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
