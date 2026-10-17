//! Less frequent features: blame, bisect, git-flow, LFS, sparse checkout,
//! submodules, worktrees, cleaning and repository optimization.

use crate::Repo;
use crate::ops::plain;
use anyhow::{Result, bail};
use gk_cli::CancelToken;
use std::path::PathBuf;

// ---------------------------------------------------------------- blame

// Blame lives in `crate::blame`; re-exported here to keep the API.
pub use crate::blame::{
    Blame, BlameCommit, BlameFeed, BlameGroup, BlameLine, IncrementalParser, SharedBlameFeed,
    UNCOMMITTED_ID, parse_blame_porcelain,
};

// ---------------------------------------------------------------- worktrees / submodules

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// The commit id from `git bisect log`'s final "# first bad commit: [<id>] …" line.
pub fn first_bad_from_log(log: &str) -> Option<String> {
    log.lines().rev().find_map(|l| {
        let rest = l.strip_prefix("# first bad commit: [")?;
        let id = rest.split(']').next()?;
        (id.len() >= 7 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_string())
    })
}

pub fn parse_worktrees(data: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in data.split("\n\n") {
        let mut wt = Worktree {
            path: PathBuf::new(),
            head: String::new(),
            branch: None,
            bare: false,
            detached: false,
            locked: false,
            prunable: false,
        };
        let mut any = false;
        for line in block.lines() {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "worktree" => {
                    wt.path = PathBuf::from(v);
                    any = true;
                }
                "HEAD" => wt.head = v.to_string(),
                "branch" => {
                    wt.branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v).to_string())
                }
                "bare" => wt.bare = true,
                "detached" => wt.detached = true,
                "locked" => wt.locked = true,
                "prunable" => wt.prunable = true,
                _ => {}
            }
        }
        if any {
            out.push(wt);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submodule {
    pub path: String,
    pub commit: String,
    /// ' ' in sync, '-' not initialized, '+' checked-out commit differs, 'U' conflicts.
    pub state: char,
    pub describe: Option<String>,
}

pub fn parse_submodule_status(data: &str) -> Vec<Submodule> {
    data.lines()
        .filter(|l| l.len() > 41)
        .map(|l| {
            let state = l.chars().next().unwrap_or(' ');
            let rest = &l[1..];
            let (commit, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            let (path, describe) = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => {
                    (&rest[..i], Some(rest[i + 2..rest.len() - 1].to_string()))
                }
                _ => (rest, None),
            };
            Submodule {
                path: path.to_string(),
                commit: commit.to_string(),
                state,
                describe,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- bisect

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BisectMark {
    Good,
    Bad,
    Skip,
}

// ---------------------------------------------------------------- git-flow

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowConfig {
    pub master: String,
    pub develop: String,
    pub feature: String,
    pub release: String,
    pub hotfix: String,
    pub version_tag: String,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            master: "main".into(),
            develop: "develop".into(),
            feature: "feature/".into(),
            release: "release/".into(),
            hotfix: "hotfix/".into(),
            version_tag: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    Feature,
    Release,
    Hotfix,
}

impl FlowConfig {
    pub fn prefix(&self, kind: FlowKind) -> &str {
        match kind {
            FlowKind::Feature => &self.feature,
            FlowKind::Release => &self.release,
            FlowKind::Hotfix => &self.hotfix,
        }
    }

    /// Which kind a branch belongs to, and its short name.
    pub fn classify<'a>(&self, branch: &'a str) -> Option<(FlowKind, &'a str)> {
        for k in [FlowKind::Feature, FlowKind::Release, FlowKind::Hotfix] {
            let p = self.prefix(k);
            if !p.is_empty()
                && let Some(rest) = branch.strip_prefix(p)
            {
                return Some((k, rest));
            }
        }
        None
    }
}

// ---------------------------------------------------------------- optimization

/// Settings that make big repositories fast; see [`Repo::optimization_status`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Optimization {
    pub fsmonitor: bool,
    pub untracked_cache: bool,
    pub many_files: bool,
    pub commit_graph: bool,
    pub maintenance: bool,
}

/// History size above which a repository without a commit-graph gets the
/// "write it now?" offer (small repositories walk fast enough without one).
pub const COMMIT_GRAPH_OFFER_MIN_COMMITS: usize = 20_000;

/// Whether to offer writing a commit-graph: a large history and none on disk.
pub fn offer_commit_graph(commits: usize, has_commit_graph: bool) -> bool {
    commits > COMMIT_GRAPH_OFFER_MIN_COMMITS && !has_commit_graph
}

impl Repo {
    // worktrees ------------------------------------------------------------
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        Ok(parse_worktrees(
            &self
                .git()
                .run(&["worktree", "list", "--porcelain"])?
                .stdout_str(),
        ))
    }

    pub fn worktree_add(
        &self,
        path: &std::path::Path,
        branch: &str,
        new_branch: bool,
    ) -> Result<()> {
        plain(&[&path.to_string_lossy(), branch])?;
        let p = path.to_string_lossy().to_string();
        if new_branch {
            self.git().run(&["worktree", "add", "-b", branch, &p])?;
        } else {
            self.git().run(&["worktree", "add", &p, branch])?;
        }
        Ok(())
    }

    pub fn worktree_remove(&self, path: &std::path::Path, force: bool) -> Result<()> {
        let p = path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&p);
        self.git().run(&args)?;
        Ok(())
    }

    pub fn worktree_prune(&self) -> Result<()> {
        self.git().run(&["worktree", "prune"])?;
        Ok(())
    }

    // submodules -------------------------------------------------------------
    pub fn submodules(&self) -> Result<Vec<Submodule>> {
        if !self.abs(".gitmodules").exists() {
            return Ok(Vec::new());
        }
        Ok(parse_submodule_status(
            &self
                .git()
                .run(&["submodule", "status", "--recursive"])?
                .stdout_str(),
        ))
    }

    pub fn submodule_update(
        &self,
        paths: &[String],
        cancel: &CancelToken,
        p: crate::ops::Progress,
    ) -> Result<()> {
        let mut args = vec![
            "submodule",
            "update",
            "--init",
            "--recursive",
            "--progress",
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        self.git().run_streaming(&args, None, cancel, p)?.check()?;
        Ok(())
    }

    pub fn submodule_add(
        &self,
        url: &str,
        path: &str,
        cancel: &CancelToken,
        p: crate::ops::Progress,
    ) -> Result<()> {
        plain(&[url, path])?;
        self.git()
            .run_streaming(
                &["submodule", "add", "--progress", url, path],
                None,
                cancel,
                p,
            )?
            .check()?;
        Ok(())
    }

    pub fn submodule_sync(&self) -> Result<()> {
        self.git().run(&["submodule", "sync", "--recursive"])?;
        Ok(())
    }

    // bisect ------------------------------------------------------------------
    pub fn bisect_start(&self, bad: &str, good: &str) -> Result<String> {
        plain(&[bad, good])?;
        self.git().run(&["bisect", "start", bad, good])?;
        Ok(self.git().run(&["bisect", "log"])?.stdout_str())
    }

    pub fn bisect_mark(&self, mark: BisectMark, rev: Option<&str>) -> Result<String> {
        let word = match mark {
            BisectMark::Good => "good",
            BisectMark::Bad => "bad",
            BisectMark::Skip => "skip",
        };
        let mut args = vec!["bisect", word];
        if let Some(r) = rev {
            args.push(r);
        }
        let out = self.git().run(&args)?.stdout_str();
        if out.contains("is the first bad commit") {
            return Ok(out);
        }
        // Some git builds (seen on Linux and macOS) print nothing when the
        // search ends; the bisect log always records the result.
        let log = self.git().run(&["bisect", "log"])?.stdout_str();
        Ok(match first_bad_from_log(&log) {
            Some(id) => format!("{id} is the first bad commit\n{out}"),
            None => out,
        })
    }

    pub fn bisect_reset(&self) -> Result<()> {
        self.git().run(&["bisect", "reset"])?;
        Ok(())
    }

    // lfs ----------------------------------------------------------------------
    pub fn lfs_available(&self) -> bool {
        self.git().run(&["lfs", "version"]).is_ok()
    }

    pub fn lfs_track(&self, pattern: &str) -> Result<()> {
        self.git().run(&["lfs", "track", pattern])?;
        Ok(())
    }

    /// Locked paths with their owners.
    pub fn lfs_locks(&self) -> Result<Vec<(String, String)>> {
        let out = self.git().run(&["lfs", "locks"])?.stdout_str();
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let path = it.next()?.trim().to_string();
                let owner = it.next().unwrap_or_default().trim().to_string();
                (!path.is_empty()).then_some((path, owner))
            })
            .collect())
    }

    pub fn lfs_lock(&self, path: &str, unlock: bool, force: bool) -> Result<()> {
        let mut args = vec!["lfs", if unlock { "unlock" } else { "lock" }];
        if unlock && force {
            args.push("--force");
        }
        args.push(path);
        self.git().run(&args)?;
        Ok(())
    }

    pub fn lfs_pull(&self, cancel: &CancelToken, p: crate::ops::Progress) -> Result<()> {
        self.git()
            .run_streaming(&["lfs", "pull"], None, cancel, p)?
            .check()?;
        Ok(())
    }

    // sparse checkout -------------------------------------------------------------
    pub fn sparse_list(&self) -> Result<Option<Vec<String>>> {
        if self.config_get("core.sparseCheckout").as_deref() != Some("true") {
            return Ok(None);
        }
        let out = self.git().run(&["sparse-checkout", "list"])?.stdout_str();
        Ok(Some(out.lines().map(String::from).collect()))
    }

    pub fn sparse_set(&self, dirs: &[String]) -> Result<()> {
        self.git().run_stdin(
            &["sparse-checkout", "set", "--cone", "--stdin"],
            dirs.join("\n").as_bytes(),
        )?;
        Ok(())
    }

    pub fn sparse_disable(&self) -> Result<()> {
        self.git().run(&["sparse-checkout", "disable"])?;
        Ok(())
    }

    // clean -------------------------------------------------------------------
    /// Untracked (and optionally ignored) files `git clean` would remove.
    pub fn clean_preview(&self, ignored: bool, dirs: bool) -> Result<Vec<String>> {
        let mut args = vec!["clean", "-n"];
        if dirs {
            args.push("-d");
        }
        if ignored {
            args.push("-x");
        }
        let out = self.git().run(&args)?.stdout_str();
        Ok(out
            .lines()
            .filter_map(|l| l.strip_prefix("Would remove ").map(String::from))
            .collect())
    }

    pub fn clean(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        // `git clean` has no --pathspec-from-file: pass literal pathspecs as
        // arguments, in chunks that stay under the Windows command-line limit.
        for chunk in crate::ops::arg_chunks(paths) {
            let mut args: Vec<String> =
                ["clean", "-f", "-d", "-x", "--"].map(String::from).to_vec();
            args.extend(chunk.iter().map(|p| format!(":(literal){p}")));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            self.git().run(&args)?;
        }
        Ok(())
    }

    // git-flow (built in, no git-flow extension needed) ------------------------------
    pub fn flow_config(&self) -> Option<FlowConfig> {
        let master = self.config_get("gitflow.branch.master")?;
        let get = |k: &str, d: &str| self.config_get(k).unwrap_or_else(|| d.to_string());
        Some(FlowConfig {
            master,
            develop: get("gitflow.branch.develop", "develop"),
            feature: get("gitflow.prefix.feature", "feature/"),
            release: get("gitflow.prefix.release", "release/"),
            hotfix: get("gitflow.prefix.hotfix", "hotfix/"),
            version_tag: get("gitflow.prefix.versiontag", ""),
        })
    }

    pub fn flow_init(&self, cfg: &FlowConfig) -> Result<()> {
        let set = |k: &str, v: &str| self.config_set(k, v, false);
        set("gitflow.branch.master", &cfg.master)?;
        set("gitflow.branch.develop", &cfg.develop)?;
        set("gitflow.prefix.feature", &cfg.feature)?;
        set("gitflow.prefix.release", &cfg.release)?;
        set("gitflow.prefix.hotfix", &cfg.hotfix)?;
        set("gitflow.prefix.versiontag", &cfg.version_tag)?;
        let exists = |b: &str| {
            self.git()
                .run(&["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")])
                .is_ok()
        };
        if !exists(&cfg.develop) {
            if !exists(&cfg.master) {
                bail!("branch '{}' does not exist", cfg.master);
            }
            self.git().run(&["branch", &cfg.develop, &cfg.master])?;
        }
        Ok(())
    }

    pub fn flow_start(
        &self,
        cfg: &FlowConfig,
        kind: FlowKind,
        name: &str,
        base: Option<&str>,
    ) -> Result<()> {
        plain(&[name, base.unwrap_or_default()])?;
        let branch = format!("{}{name}", cfg.prefix(kind));
        let default_base = match kind {
            FlowKind::Hotfix => &cfg.master,
            _ => &cfg.develop,
        };
        self.git()
            .run(&["checkout", "-b", &branch, base.unwrap_or(default_base)])?;
        Ok(())
    }

    /// Finishes a flow branch: merges with --no-ff, tags releases/hotfixes,
    /// deletes the branch. Stops (Ok with text) on merge conflicts.
    pub fn flow_finish(
        &self,
        cfg: &FlowConfig,
        kind: FlowKind,
        name: &str,
        tag_message: Option<&str>,
        keep: bool,
    ) -> Result<String> {
        let branch = format!("{}{name}", cfg.prefix(kind));
        let mut log = String::new();
        let merge_into = |target: &str, log: &mut String| -> Result<bool> {
            self.git().run(&["checkout", target])?;
            let out = self
                .git()
                .run_raw(&["merge", "--no-ff", "--no-edit", &branch], None)?;
            log.push_str(&out.stdout_str());
            if !out.success() {
                log.push_str(&out.stderr_str());
                return Ok(false);
            }
            Ok(true)
        };
        match kind {
            FlowKind::Feature => {
                if !merge_into(&cfg.develop, &mut log)? {
                    return Ok(log);
                }
            }
            FlowKind::Release | FlowKind::Hotfix => {
                if !merge_into(&cfg.master, &mut log)? {
                    return Ok(log);
                }
                let tag = format!("{}{name}", cfg.version_tag);
                let msg = tag_message.unwrap_or(&tag).to_string();
                self.create_tag(&tag, &cfg.master, Some(&msg), false)?;
                if !merge_into(&cfg.develop, &mut log)? {
                    return Ok(log);
                }
            }
        }
        if !keep {
            self.git().run(&["branch", "-d", &branch])?;
        }
        Ok(log)
    }

    // optimization -----------------------------------------------------------
    pub fn optimization_status(&self) -> Optimization {
        let is_true = |k: &str| self.config_get(k).is_some_and(|v| v == "true" || v == "1");
        Optimization {
            fsmonitor: is_true("core.fsmonitor"),
            untracked_cache: is_true("core.untrackedCache"),
            many_files: is_true("feature.manyFiles"),
            commit_graph: self.has_commit_graph(),
            maintenance: self
                .git()
                .run(&["config", "--global", "--get-all", "maintenance.repo"])
                .map(|o| {
                    // Canonical paths: 8.3 short names (C:\Users\RUNNER~1) and
                    // separators differ between how git and we spell a path.
                    let canon = |p: &std::path::Path| {
                        p.canonicalize()
                            .unwrap_or_else(|_| p.to_path_buf())
                            .to_string_lossy()
                            .replace('\\', "/")
                            .to_lowercase()
                    };
                    let me = canon(self.workdir());
                    o.stdout_str()
                        .lines()
                        .any(|l| canon(std::path::Path::new(l.trim())) == me)
                })
                .unwrap_or(false),
        }
    }

    /// A commit-graph file (or split chain) exists. Two file-existence checks:
    /// cheap, but still disk I/O, so call it off the UI thread.
    pub fn has_commit_graph(&self) -> bool {
        let info = self.git_dir().join("objects/info");
        info.join("commit-graph").exists() || info.join("commit-graphs").exists()
    }

    /// Writes the commit-graph (reachable commits, with changed-path Bloom
    /// filters), streaming git's progress.
    pub fn write_commit_graph(&self, cancel: &CancelToken, p: crate::ops::Progress) -> Result<()> {
        self.git()
            .run_streaming(
                &[
                    "commit-graph",
                    "write",
                    "--reachable",
                    "--changed-paths",
                    "--progress",
                ],
                None,
                cancel,
                p,
            )?
            .check()?;
        Ok(())
    }

    /// One-click optimization for large repositories. On Windows it also
    /// turns on `core.longpaths`, so paths over 260 characters check out.
    pub fn optimize(&self, cancel: &CancelToken, p: crate::ops::Progress) -> Result<()> {
        self.config_set("core.fsmonitor", "true", false)?;
        if cfg!(windows) {
            self.config_set("core.longpaths", "true", false)?;
        }
        self.config_set("core.untrackedCache", "true", false)?;
        self.config_set("feature.manyFiles", "true", false)?;
        self.config_set("fetch.writeCommitGraph", "true", false)?;
        self.git()
            .run_streaming(
                &[
                    "commit-graph",
                    "write",
                    "--reachable",
                    "--changed-paths",
                    "--progress",
                ],
                None,
                cancel,
                &mut *p,
            )?
            .check()?;
        self.git()
            .run_streaming(
                &["multi-pack-index", "write", "--progress"],
                None,
                cancel,
                p,
            )?
            .check()?;
        // Registers background maintenance (prefetch, commit-graph, loose objects).
        let _ = self.git().run(&["maintenance", "register"]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_bad_commit_from_bisect_log() {
        let log = "git bisect start 'abc' 'def'\n# good: [1111111] c5\ngit bisect good 1111111\n\
                   # bad: [b58f6457dfe79580a4d5d9e66464e67dd28c9feb] c7\ngit bisect bad b58f645\n\
                   # first bad commit: [b58f6457dfe79580a4d5d9e66464e67dd28c9feb] c7\n";
        assert_eq!(
            first_bad_from_log(log).as_deref(),
            Some("b58f6457dfe79580a4d5d9e66464e67dd28c9feb")
        );
        assert_eq!(
            first_bad_from_log("git bisect start\n# bad: [abcdef1] x\n"),
            None
        );
    }

    #[test]
    fn worktree_porcelain() {
        let data = "worktree C:/repo\nHEAD abc\nbranch refs/heads/main\n\nworktree C:/wt\nHEAD def\ndetached\n";
        let w = parse_worktrees(data);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[1].detached);
    }

    #[test]
    fn submodule_status() {
        let data = " 1234567890123456789012345678901234567890 libs/foo (v1.0)\n-abcdefabcdefabcdefabcdefabcdefabcdefabcd libs/bar\n";
        let s = parse_submodule_status(data);
        assert_eq!(s[0].path, "libs/foo");
        assert_eq!(s[0].describe.as_deref(), Some("v1.0"));
        assert_eq!(s[1].state, '-');
    }

    #[test]
    fn flow_classify() {
        let c = FlowConfig::default();
        assert_eq!(
            c.classify("feature/login"),
            Some((FlowKind::Feature, "login"))
        );
        assert_eq!(c.classify("main"), None);
    }
}
