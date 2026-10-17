//! Helpers: throwaway repositories driven through the git CLI.

use gk_cli::{CancelToken, CommandLog, Git, Line};
use gk_core::Repo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Fixed, increasing commit dates make history order deterministic.
static CLOCK: AtomicI64 = AtomicI64::new(1_700_000_000);

fn next_date() -> String {
    format!("{} +0000", CLOCK.fetch_add(60, Ordering::SeqCst))
}

pub struct TestRepo {
    _dir: TempDir,
    pub repo: Repo,
}

/// Local config every test repository gets, independent of the user's global config.
pub fn configure(git: &Git, autocrlf: bool) {
    for (k, v) in [
        ("user.name", "Test User"),
        ("user.email", "test@example.com"),
        ("core.autocrlf", if autocrlf { "true" } else { "false" }),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("pull.rebase", "false"),
        ("merge.conflictstyle", "merge"),
        ("advice.detachedHead", "false"),
        ("core.hooksPath", ".git/no-hooks"),
    ] {
        git.run(&["config", k, v]).unwrap();
    }
}

pub fn git_at(path: &Path) -> Git {
    Git::new(path, CommandLog::default())
}

impl TestRepo {
    pub fn new() -> Self {
        Self::with_autocrlf(false)
    }

    pub fn with_autocrlf(autocrlf: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let git = git_at(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        configure(&git, autocrlf);
        let repo = Repo::open(dir.path(), CommandLog::default()).unwrap();
        Self { _dir: dir, repo }
    }

    /// Clones `url` into a fresh directory.
    pub fn clone_from(url: &Path) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("w");
        git_at(dir.path())
            .run(&["clone", "-q", &url.to_string_lossy(), &target.to_string_lossy()])
            .unwrap();
        configure(&git_at(&target), false);
        let repo = Repo::open(&target, CommandLog::default()).unwrap();
        Self { _dir: dir, repo }
    }

    /// Runs git, panicking on failure; returns trimmed stdout.
    pub fn git(&self, args: &[&str]) -> String {
        self.repo.git().read(args).unwrap_or_else(|e| panic!("git {args:?} failed: {e:#}"))
    }

    /// Runs git with fixed author/committer dates.
    pub fn git_dated(&self, args: &[&str]) -> String {
        let d = next_date();
        self.repo
            .git()
            .env("GIT_AUTHOR_DATE", &d)
            .env("GIT_COMMITTER_DATE", &d)
            .env("GIT_EDITOR", "true")
            .read(args)
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e:#}"))
    }

    /// Runs git and returns the raw output without checking the exit code.
    pub fn git_raw(&self, args: &[&str]) -> gk_cli::Output {
        self.repo.git().env("GIT_EDITOR", "true").run_raw(args, None).unwrap()
    }

    pub fn write(&self, rel: &str, content: impl AsRef<[u8]>) {
        let p = self.repo.abs(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        String::from_utf8(self.read_bytes(rel)).unwrap()
    }

    pub fn read_bytes(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.repo.abs(rel)).unwrap()
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.repo.abs(rel).exists()
    }

    /// Writes a file, stages it and commits; returns the new HEAD id.
    pub fn commit_file(&self, rel: &str, content: &str, msg: &str) -> String {
        self.write(rel, content);
        self.git(&["add", "--", rel]);
        self.commit(msg)
    }

    /// Commits whatever is staged with a deterministic date.
    pub fn commit(&self, msg: &str) -> String {
        self.git_dated(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.head()
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    pub fn rev(&self, r: &str) -> String {
        self.git(&["rev-parse", r])
    }

    pub fn oid(&self, r: &str) -> gk_core::ObjectId {
        gk_core::ObjectId::from_hex(self.rev(r).as_bytes()).unwrap()
    }

    /// Commit subjects of `rev`, newest first.
    pub fn subjects(&self, rev: &str) -> Vec<String> {
        self.git(&["log", "--format=%s", rev]).lines().map(str::to_string).collect()
    }

    pub fn staged_diff(&self) -> String {
        self.git(&["diff", "--cached", "--no-color"])
    }

    /// `git show :path`.
    pub fn index_content(&self, path: &str) -> Vec<u8> {
        self.repo.git().run(&["show", &format!(":{path}")]).unwrap().stdout
    }

    pub fn status(&self) -> String {
        self.git(&["status", "--porcelain=v1", "--untracked-files=all"])
    }
}

/// A bare repository usable as a remote via its file path.
pub struct Remote {
    pub dir: TempDir,
}

impl Remote {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        git_at(dir.path()).run(&["init", "-q", "--bare", "-b", "main"]).unwrap();
        Self { dir }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub fn url(&self) -> String {
        self.dir.path().to_string_lossy().replace('\\', "/")
    }

    pub fn git(&self, args: &[&str]) -> String {
        git_at(self.dir.path()).read(args).unwrap()
    }
}

/// No-op progress sink plus a fresh cancel token.
pub fn net() -> (CancelToken, impl FnMut(Line) + Send) {
    (CancelToken::new(), |_l: Line| {})
}

/// Polls `cond` until it holds or `timeout` passes.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn s(v: &str) -> String {
    v.to_string()
}
