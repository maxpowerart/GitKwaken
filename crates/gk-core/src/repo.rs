use anyhow::{Context, Result, anyhow};
use gk_cli::{CommandLog, Git};
use std::path::{Path, PathBuf};

pub(crate) fn gix_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{e}")
}

/// An opened repository: a thread-safe gitoxide handle plus a CLI runner.
#[derive(Clone)]
pub struct Repo {
    ts: gix::ThreadSafeRepository,
    workdir: PathBuf,
    git_dir: PathBuf,
    git: Git,
}

impl Repo {
    /// Opens the repository containing `path`. The git executable is found via
    /// `GITKWAKEN_GIT`, then PATH (see [`gk_cli::find_git`]).
    pub fn open(path: &Path, log: CommandLog) -> Result<Self> {
        Self::open_with_git(path, log, None)
    }

    /// Like [`open`](Self::open), with an explicit git executable.
    pub fn open_with_git(path: &Path, log: CommandLog, git_exe: Option<PathBuf>) -> Result<Self> {
        let ts = gix::ThreadSafeRepository::discover(path)
            .map_err(gix_err)
            .with_context(|| format!("not a git repository: {}", path.display()))?;
        let local = ts.to_thread_local();
        let workdir = local
            .workdir()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("bare repositories are not supported"))?;
        let git_dir = local.git_dir().to_path_buf();
        let mut git = Git::new(&workdir, log);
        if let Some(exe) = git_exe {
            git = git.with_exe(exe);
        }
        Ok(Self {
            ts,
            workdir,
            git_dir,
            git,
        })
    }

    /// Adds environment variables (e.g. `GIT_ASKPASS`) to every git call.
    pub fn with_env(mut self, vars: impl IntoIterator<Item = (String, String)>) -> Repo {
        for (k, v) in vars {
            self.git = self.git.env(k, v);
        }
        self
    }

    /// A gitoxide handle for the current thread.
    pub fn gix(&self) -> gix::Repository {
        let mut r = self.ts.to_thread_local();
        r.object_cache_size_if_unset(8 * 1024 * 1024);
        r
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    /// Display name: the working directory's folder name.
    pub fn name(&self) -> String {
        self.workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workdir.display().to_string())
    }

    /// Absolute path of a repository-relative path.
    pub fn abs(&self, rel: &str) -> PathBuf {
        let mut p = self.workdir.clone();
        for part in rel.split('/') {
            p.push(part);
        }
        p
    }
}

/// Initializes a new repository.
pub fn init(path: &Path, log: CommandLog) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Git::new(path, log).run(&["init"])?;
    Ok(())
}
