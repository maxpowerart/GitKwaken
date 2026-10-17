//! Repository-changing operations. Everything goes through the git CLI so
//! hooks, filters, credential helpers and signing behave exactly like git.

use crate::content::{self, DiffTarget};
use crate::{Repo, repo::gix_err};
use anyhow::{Result, anyhow, bail};
use gk_cli::{CancelToken, Line, nul_paths};
use gk_diff::{Line as DLine, Options, SelectMode, apply_selection, diff};

/// Progress sink for long-running (network) operations.
pub type Progress<'a> = &'a mut (dyn FnMut(Line) + Send);

impl Repo {
    // ---------------------------------------------------------------- staging

    pub fn stage(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let (dirs, files): (Vec<String>, Vec<String>) =
            paths.iter().cloned().partition(|p| p.ends_with('/'));
        if files.len() > BULK_PATHS {
            // Linear in the number of paths; applies the same clean filters
            // (autocrlf, LFS) as `git add`.
            self.git().run_stdin(
                &["update-index", "--add", "--remove", "-z", "--stdin"],
                &nul_paths(&files),
            )?;
            if !dirs.is_empty() {
                self.git().run_stdin(
                    &["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"],
                    &literal_paths(&dirs),
                )?;
            }
            return Ok(());
        }
        self.git().run_stdin(
            &["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"],
            &literal_paths(paths),
        )?;
        Ok(())
    }

    /// Restores the HEAD version of many index entries at once (or removes
    /// entries HEAD does not have) with one `update-index --index-info`.
    fn unstage_bulk(&self, paths: &[String]) -> Result<()> {
        let tree = self
            .git()
            .run(&["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?
            .stdout;
        let mut head: std::collections::HashMap<&[u8], &[u8]> = std::collections::HashMap::new();
        for rec in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            // "<mode> <type> <oid>\t<path>"
            if let Some(tab) = rec.iter().position(|b| *b == b'\t') {
                head.insert(&rec[tab + 1..], &rec[..tab]);
            }
        }
        let zero = "0".repeat(self.gix().object_hash().len_in_hex());
        let mut input = Vec::new();
        for p in paths {
            match head.get(p.as_bytes()) {
                Some(meta) => {
                    // index-info accepts "<mode> <type> <oid>\t<path>"
                    input.extend_from_slice(meta);
                }
                None => input.extend_from_slice(format!("0 {zero}").as_bytes()),
            }
            input.push(b'\t');
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        self.git()
            .run_stdin(&["update-index", "-z", "--index-info"], &input)?;
        Ok(())
    }

    pub fn stage_all(&self) -> Result<()> {
        self.git().run(&["add", "-A"])?;
        Ok(())
    }

    pub fn unstage(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        if self.gix().head_id().is_err() {
            // No commits yet: remove from the index entirely.
            self.git().run_stdin(
                &[
                    "rm",
                    "--cached",
                    "-r",
                    "-q",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                &literal_paths(paths),
            )?;
        } else if paths.len() > BULK_PATHS && !paths.iter().any(|p| p.ends_with('/')) {
            self.unstage_bulk(paths)?;
        } else {
            self.git().run_stdin(
                &[
                    "reset",
                    "-q",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                    "--",
                ],
                &literal_paths(paths),
            )?;
        }
        Ok(())
    }

    pub fn unstage_all(&self) -> Result<()> {
        if self.gix().head_id().is_err() {
            self.git().run(&["rm", "--cached", "-r", "-q", "."])?;
        } else {
            self.git().run(&["reset", "-q"])?;
        }
        Ok(())
    }

    /// Discards worktree changes of tracked files and deletes untracked ones.
    pub fn discard(&self, tracked: &[String], untracked: &[String]) -> Result<()> {
        if !tracked.is_empty() {
            self.git().run_stdin(
                &[
                    "checkout",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                    "--",
                ],
                &literal_paths(tracked),
            )?;
        }
        for p in untracked {
            let abs = self.abs(p.trim_end_matches('/'));
            if abs.is_dir() {
                std::fs::remove_dir_all(&abs)?;
            } else if abs.exists() {
                std::fs::remove_file(&abs)?;
            }
        }
        Ok(())
    }

    /// Stages (`Apply`) or unstages (`Revert`) individual lines of a file.
    /// `pick` selects lines of a full-context diff of the matching target.
    pub fn stage_lines(
        &self,
        path: &str,
        unstage: bool,
        pick: &dyn Fn(&DLine) -> bool,
    ) -> Result<()> {
        let g = self.gix();
        if content::index_blob(&g, path, 2)?.is_some()
            || content::index_blob(&g, path, 3)?.is_some()
        {
            bail!("{path} has conflicts; resolve them first");
        }
        let (target, mode) = if unstage {
            (
                DiffTarget::Staged {
                    path: path.to_string(),
                    old_path: None,
                },
                SelectMode::Revert,
            )
        } else {
            // Untracked files get their index entry from update-index below.
            (
                DiffTarget::Unstaged {
                    path: path.to_string(),
                },
                SelectMode::Apply,
            )
        };
        let (old, new) = content::load_sides(self, &target)?;
        let d = diff(
            old.as_deref().unwrap_or_default(),
            new.as_deref().unwrap_or_default(),
            Options::full(),
        );
        if d.binary {
            bail!("cannot stage lines of a binary file");
        }
        if !d
            .lines
            .iter()
            .any(|l| l.kind != gk_diff::LineKind::Context && pick(l))
        {
            return Ok(());
        }
        let result = apply_selection(&d, pick, mode);
        // Staging all of a deletion, or unstaging all of an addition, removes
        // the index entry instead of leaving an empty file.
        let side_missing = if unstage {
            old.is_none()
        } else {
            new.is_none()
        };
        if side_missing && result.is_empty() {
            self.git()
                .run(&["rm", "--cached", "-f", "-q", "--", path])?;
            return Ok(());
        }
        let mode_str =
            index_mode(&g, path).unwrap_or_else(|| worktree_mode(&self.abs(path)).to_string());
        let id = self
            .git()
            .run_stdin(&["hash-object", "-w", "--stdin", "--no-filters"], &result)?
            .stdout_str();
        let id = id.trim();
        let info = format!("{mode_str},{id},{path}");
        self.git()
            .run(&["update-index", "--add", "--cacheinfo", &info])?;
        Ok(())
    }

    /// Reverts selected lines in the worktree file.
    pub fn discard_lines(&self, path: &str, pick: &dyn Fn(&DLine) -> bool) -> Result<()> {
        let g = self.gix();
        let t = DiffTarget::Unstaged {
            path: path.to_string(),
        };
        let (old, new) = content::load_sides(self, &t)?;
        let d = diff(
            old.as_deref().unwrap_or_default(),
            new.as_deref().unwrap_or_default(),
            Options::full(),
        );
        if d.binary {
            bail!("cannot discard lines of a binary file");
        }
        let result = apply_selection(&d, pick, SelectMode::Revert);
        content::write_worktree(self, &g, path, &result)
    }

    // ---------------------------------------------------------------- commit

    pub fn commit(&self, opts: &CommitOptions) -> Result<String> {
        let empty = opts.message.trim().is_empty();
        if empty && !opts.amend {
            bail!("commit message is empty");
        }
        // Amending with an empty message keeps the previous one.
        let mut args: Vec<&str> = if empty {
            vec!["commit", "--no-edit"]
        } else {
            vec!["commit", "-F", "-", "--cleanup=strip"]
        };
        if opts.amend {
            args.push("--amend");
        }
        if opts.signoff {
            args.push("--signoff");
        }
        if opts.no_verify {
            args.push("--no-verify");
        }
        if opts.allow_empty {
            args.push("--allow-empty");
        }
        match opts.sign {
            Some(true) => args.push("-S"),
            Some(false) => args.push("--no-gpg-sign"),
            None => {}
        }
        let author;
        if let Some(a) = &opts.author {
            author = format!("--author={a}");
            args.push(&author);
        }
        let out = if empty {
            self.git().run(&args)?
        } else {
            self.git().run_stdin(&args, opts.message.as_bytes())?
        };
        Ok(out.stdout_str())
    }

    /// Message of HEAD, for amending.
    pub fn head_message(&self) -> Result<String> {
        let g = self.gix();
        let c = g.head_commit().map_err(gix_err)?;
        Ok(c.message_raw_sloppy().to_string())
    }

    // ---------------------------------------------------------------- branches

    pub fn checkout(&self, rev: &str) -> Result<()> {
        plain(&[rev])?;
        // "--" keeps a vanished rev from being read as a path to restore.
        self.git().run(&["checkout", rev, "--"])?;
        Ok(())
    }

    /// Checks out a remote branch as a new local tracking branch.
    pub fn checkout_remote(&self, remote_branch: &str, local: &str) -> Result<()> {
        plain(&[remote_branch, local])?;
        self.git()
            .run(&["checkout", "-b", local, "--track", remote_branch])?;
        Ok(())
    }

    pub fn create_branch(&self, name: &str, start: Option<&str>, checkout: bool) -> Result<()> {
        plain(&[name, start.unwrap_or_default()])?;
        let mut args = vec![if checkout { "checkout" } else { "branch" }];
        if checkout {
            args.push("-b");
        }
        args.push(name);
        if let Some(s) = start {
            args.push(s);
        }
        self.git().run(&args)?;
        Ok(())
    }

    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        plain(&[name])?;
        self.git()
            .run(&["branch", if force { "-D" } else { "-d" }, name])?;
        Ok(())
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        plain(&[old, new])?;
        self.git().run(&["branch", "-m", old, new])?;
        Ok(())
    }

    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<()> {
        plain(&[branch, upstream.unwrap_or_default()])?;
        match upstream {
            Some(u) => self
                .git()
                .run(&["branch", "--set-upstream-to", u, branch])?,
            None => self.git().run(&["branch", "--unset-upstream", branch])?,
        };
        Ok(())
    }

    pub fn delete_remote_branch(
        &self,
        remote: &str,
        branch: &str,
        cancel: &CancelToken,
        p: Progress,
    ) -> Result<()> {
        let refspec = format!(":refs/heads/{branch}");
        self.git()
            .run_streaming(&["push", "--progress", remote, &refspec], None, cancel, p)?
            .check()?;
        Ok(())
    }

    // ---------------------------------------------------------------- tags

    pub fn create_tag(
        &self,
        name: &str,
        rev: &str,
        message: Option<&str>,
        sign: bool,
    ) -> Result<()> {
        plain(&[name, rev])?;
        match message {
            Some(m) if !m.trim().is_empty() => {
                let flag = if sign { "-s" } else { "-a" };
                self.git()
                    .run_stdin(&["tag", flag, name, rev, "-F", "-"], m.as_bytes())?;
            }
            _ => {
                self.git().run(&["tag", name, rev])?;
            }
        }
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        plain(&[name])?;
        self.git().run(&["tag", "-d", name])?;
        Ok(())
    }

    // ---------------------------------------------------------------- history rewriting

    pub fn merge(&self, rev: &str, opts: &MergeOptions) -> Result<String> {
        plain(&[rev])?;
        let mut args = vec!["merge"];
        match opts.ff {
            FastForward::Allow => {}
            FastForward::Never => args.push("--no-ff"),
            FastForward::Only => args.push("--ff-only"),
        }
        if opts.squash {
            args.push("--squash");
        }
        if !opts.commit {
            args.push("--no-commit");
        }
        let msg;
        if let Some(m) = &opts.message {
            msg = m.clone();
            args.extend(["-m", &msg]);
        }
        args.push(rev);
        let out = self.git().run_raw(&args, None)?;
        conflict_aware(out)
    }

    pub fn rebase(&self, onto: &str) -> Result<String> {
        plain(&[onto])?;
        let out = self
            .git()
            .env("GIT_EDITOR", "true")
            .run_raw(&["rebase", onto], None)?;
        conflict_aware(out)
    }

    /// Interactive rebase driven by a prepared todo list.
    pub fn rebase_interactive(
        &self,
        base: &str,
        todo: &[TodoItem],
        autosquash: bool,
    ) -> Result<String> {
        // Unique per call: concurrent rebases (several sessions) must not share files.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "gitkwaken-todo-{}-{seq}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let mut todo_text = String::new();
        for (i, t) in todo.iter().enumerate() {
            // The summary is only a comment for git, but must stay on one line.
            let summary = t.summary.replace(['\r', '\n'], " ");
            match (t.action, &t.new_message) {
                // Reword without an editor: pick, then amend with the prepared message.
                (TodoAction::Reword, Some(msg)) => {
                    let msg_file = dir.join(format!("msg-{i}"));
                    std::fs::write(&msg_file, msg)?;
                    let mf = sh_quote(&msg_file);
                    todo_text.push_str(&format!("pick {} {summary}\n", t.id));
                    todo_text.push_str(&format!(
                        "exec git commit --amend -q --allow-empty --cleanup=strip -F {mf}\n"
                    ));
                }
                _ => todo_text.push_str(&format!("{} {} {summary}\n", t.action.word(), t.id)),
            }
        }
        let todo_file = dir.join("todo");
        std::fs::write(&todo_file, todo_text)?;
        // The sequence editor overwrites git's todo with ours.
        let editor = sequence_editor(&todo_file)?;
        let mut args = vec!["rebase", "-i"];
        if autosquash {
            args.push("--autosquash");
        }
        args.push(base);
        let out = self
            .git()
            .env("GIT_SEQUENCE_EDITOR", editor)
            .env("GIT_EDITOR", "true")
            .run_raw(&args, None);
        // Reword messages are needed until the rebase finishes.
        if !self.git_dir().join("rebase-merge").exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        conflict_aware(out?)
    }

    pub fn continue_op(&self, state: crate::RepoState) -> Result<String> {
        use crate::RepoState::*;
        let args: &[&str] = match state {
            Merging => &["commit", "--no-edit"],
            Rebasing { .. } => &["rebase", "--continue"],
            CherryPicking => &["cherry-pick", "--continue"],
            Reverting => &["revert", "--continue"],
            ApplyingMailbox => &["am", "--continue"],
            Bisecting | Clean => bail!("nothing to continue"),
        };
        let out = self.git().env("GIT_EDITOR", "true").run_raw(args, None)?;
        conflict_aware(out)
    }

    pub fn abort_op(&self, state: crate::RepoState) -> Result<()> {
        use crate::RepoState::*;
        let args: &[&str] = match state {
            Merging => &["merge", "--abort"],
            Rebasing { .. } => &["rebase", "--abort"],
            CherryPicking => &["cherry-pick", "--abort"],
            Reverting => &["revert", "--abort"],
            ApplyingMailbox => &["am", "--abort"],
            Bisecting => &["bisect", "reset"],
            Clean => bail!("nothing to abort"),
        };
        self.git().run(args)?;
        Ok(())
    }

    pub fn skip_op(&self, state: crate::RepoState) -> Result<String> {
        use crate::RepoState::*;
        let args: &[&str] = match state {
            Rebasing { .. } => &["rebase", "--skip"],
            CherryPicking => &["cherry-pick", "--skip"],
            Reverting => &["revert", "--skip"],
            ApplyingMailbox => &["am", "--skip"],
            _ => bail!("cannot skip in this state"),
        };
        let out = self.git().env("GIT_EDITOR", "true").run_raw(args, None)?;
        conflict_aware(out)
    }

    pub fn cherry_pick(&self, ids: &[String], no_commit: bool) -> Result<String> {
        let mut args = vec!["cherry-pick"];
        if no_commit {
            args.push("--no-commit");
        }
        args.extend(ids.iter().map(String::as_str));
        let out = self.git().env("GIT_EDITOR", "true").run_raw(&args, None)?;
        conflict_aware(out)
    }

    pub fn revert(&self, ids: &[String], no_commit: bool) -> Result<String> {
        let mut args = vec!["revert", "--no-edit"];
        if no_commit {
            args.push("--no-commit");
        }
        args.extend(ids.iter().map(String::as_str));
        let out = self.git().run_raw(&args, None)?;
        conflict_aware(out)
    }

    pub fn reset(&self, rev: &str, mode: ResetMode) -> Result<()> {
        plain(&[rev])?;
        let m = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        self.git().run(&["reset", "-q", m, rev])?;
        Ok(())
    }

    // ---------------------------------------------------------------- conflicts

    pub fn resolve_with(&self, paths: &[String], side: Side) -> Result<()> {
        let flag = match side {
            Side::Ours => "--ours",
            Side::Theirs => "--theirs",
        };
        let mut args = vec!["checkout", flag, "--"];
        args.extend(paths.iter().map(String::as_str));
        self.git().run(&args)?;
        self.stage(paths)
    }

    pub fn mark_resolved(&self, paths: &[String]) -> Result<()> {
        self.stage(paths)
    }

    /// Writes resolved content (worktree form) and stages it.
    pub fn save_resolution(&self, path: &str, content: &[u8]) -> Result<()> {
        std::fs::write(self.abs(path), content)?;
        self.stage(&[path.to_string()])
    }

    // ---------------------------------------------------------------- stash

    pub fn stash_push(&self, message: &str, opts: &StashOptions, paths: &[String]) -> Result<()> {
        let mut args = vec!["stash", "push"];
        if opts.keep_index {
            args.push("--keep-index");
        }
        if opts.include_untracked {
            args.push("--include-untracked");
        }
        if opts.staged_only {
            args.push("--staged");
        }
        if !message.trim().is_empty() {
            args.extend(["-m", message]);
        }
        if paths.is_empty() {
            self.git().run(&args)?;
        } else {
            args.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
            self.git().run_stdin(&args, &literal_paths(paths))?;
        }
        Ok(())
    }

    pub fn stash_apply(&self, index: usize, pop: bool, restore_index: bool) -> Result<String> {
        let r = format!("stash@{{{index}}}");
        let mut args = vec!["stash", if pop { "pop" } else { "apply" }];
        if restore_index {
            args.push("--index");
        }
        args.push(&r);
        let out = self.git().run_raw(&args, None)?;
        conflict_aware(out)
    }

    pub fn stash_drop(&self, index: usize) -> Result<()> {
        self.git()
            .run(&["stash", "drop", &format!("stash@{{{index}}}")])?;
        Ok(())
    }

    pub fn stash_branch(&self, index: usize, branch: &str) -> Result<()> {
        self.git()
            .run(&["stash", "branch", branch, &format!("stash@{{{index}}}")])?;
        Ok(())
    }

    // ---------------------------------------------------------------- remotes

    pub fn fetch(
        &self,
        remote: Option<&str>,
        opts: &FetchOptions,
        cancel: &CancelToken,
        p: Progress,
    ) -> Result<String> {
        let mut args = vec!["fetch", "--progress"];
        if opts.prune {
            args.push("--prune");
        }
        if opts.tags {
            args.push("--tags");
        }
        match remote {
            Some(r) => args.push(r),
            None => args.push("--all"),
        }
        let out = self.git().run_streaming(&args, None, cancel, p)?.check()?;
        Ok(out.stderr_str())
    }

    pub fn pull(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        rebase: bool,
        cancel: &CancelToken,
        p: Progress,
    ) -> Result<String> {
        let mut args = vec![
            "pull",
            "--progress",
            if rebase { "--rebase" } else { "--no-rebase" },
        ];
        if let Some(r) = remote {
            args.push(r);
            if let Some(b) = branch {
                args.push(b);
            }
        }
        let out = self
            .git()
            .env("GIT_EDITOR", "true")
            .run_streaming(&args, None, cancel, p)?;
        conflict_aware(out)
    }

    pub fn push(&self, opts: &PushOptions, cancel: &CancelToken, p: Progress) -> Result<String> {
        let mut args: Vec<String> = vec!["push".into(), "--progress".into()];
        if opts.force {
            args.push("--force-with-lease".into());
        }
        if opts.set_upstream {
            args.push("--set-upstream".into());
        }
        if opts.tags {
            args.push("--follow-tags".into());
        }
        if opts.no_verify {
            args.push("--no-verify".into());
        }
        // An empty remote with no refspecs means git's default push.
        if !opts.remote.is_empty() || !opts.refspecs.is_empty() {
            args.push(opts.remote.clone());
        }
        for (local, remote) in &opts.refspecs {
            if remote.is_empty() || remote == local {
                args.push(local.clone());
            } else {
                args.push(format!("{local}:{remote}"));
            }
        }
        // `push -u <remote>` alone fails for a branch without upstream: push HEAD
        // to a branch of the same name.
        if opts.set_upstream && opts.refspecs.is_empty() && !opts.remote.is_empty() {
            args.push("HEAD".into());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.git().run_streaming(&args, None, cancel, p)?.check()?;
        Ok(out.stderr_str())
    }

    pub fn push_tag(
        &self,
        remote: &str,
        tag: &str,
        delete: bool,
        cancel: &CancelToken,
        p: Progress,
    ) -> Result<()> {
        let spec = if delete {
            format!(":refs/tags/{tag}")
        } else {
            format!("refs/tags/{tag}")
        };
        self.git()
            .run_streaming(&["push", "--progress", remote, &spec], None, cancel, p)?
            .check()?;
        Ok(())
    }

    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        plain(&[name, url])?;
        self.git().run(&["remote", "add", name, url])?;
        Ok(())
    }

    pub fn remove_remote(&self, name: &str) -> Result<()> {
        plain(&[name])?;
        self.git().run(&["remote", "remove", name])?;
        Ok(())
    }

    pub fn rename_remote(&self, old: &str, new: &str) -> Result<()> {
        plain(&[old, new])?;
        self.git().run(&["remote", "rename", old, new])?;
        Ok(())
    }

    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        plain(&[name, url])?;
        self.git().run(&["remote", "set-url", name, url])?;
        Ok(())
    }

    // ---------------------------------------------------------------- misc

    /// Appends patterns to `.gitignore` at the repository root.
    pub fn ignore(&self, patterns: &[String]) -> Result<()> {
        let path = self.abs(".gitignore");
        let mut content = std::fs::read_to_string(&path).unwrap_or_default();
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        for p in patterns {
            content.push_str(p);
            content.push('\n');
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn config_get(&self, key: &str) -> Option<String> {
        self.git()
            .run(&["config", "--get", key])
            .ok()
            .map(|o| o.stdout_str().trim().to_string())
    }

    pub fn config_set(&self, key: &str, value: &str, global: bool) -> Result<()> {
        let mut args = vec!["config"];
        if global {
            args.push("--global");
        }
        args.extend([key, value]);
        self.git().run(&args)?;
        Ok(())
    }

    /// Creates a patch file content for the given commits (`git format-patch --stdout`).
    pub fn format_patch(&self, ids: &[String]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for id in ids {
            let o = self.git().run(&["format-patch", "-1", "--stdout", id])?;
            out.extend_from_slice(&o.stdout);
        }
        Ok(out)
    }

    pub fn apply_patch(&self, patch: &std::path::Path, to_index: bool) -> Result<()> {
        let p = patch.to_string_lossy().to_string();
        let mut args = vec!["apply", "--3way"];
        if to_index {
            args.push("--cached");
        }
        args.push(&p);
        self.git().run(&args)?;
        Ok(())
    }

    pub fn archive(&self, rev: &str, out: &std::path::Path) -> Result<()> {
        plain(&[rev])?;
        let o = out.to_string_lossy().to_string();
        self.git().run(&["archive", "-o", &o, rev])?;
        Ok(())
    }
}

/// Index mode for a file not in the index yet.
fn worktree_mode(abs: &std::path::Path) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(abs).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
            return "100755";
        }
    }
    let _ = abs;
    "100644"
}

fn index_mode(g: &gix::Repository, path: &str) -> Option<String> {
    use bstr::ByteSlice;
    let index = g.index_or_empty().ok()?;
    let e = index.entry_by_path(path.as_bytes().as_bstr())?;
    Some(format!("{:06o}", e.mode.bits()))
}

/// Treats "stopped because of conflicts" as success, other failures as errors.
fn conflict_aware(out: gk_cli::Output) -> Result<String> {
    let text = format!("{}{}", out.stdout_str(), out.stderr_str());
    if out.success() || stopped_on_conflict(&text) {
        return Ok(text);
    }
    Err(anyhow!(text.trim().to_string()))
}

/// True when git output says an operation stopped on new conflicts. Failures
/// that merely mention conflicts (e.g. "Exiting because of an unresolved
/// conflict" when continuing too early) are not matched.
pub fn stopped_on_conflict(text: &str) -> bool {
    if text.contains("CONFLICT (") {
        return true;
    }
    let lower = text.to_lowercase();
    ["could not apply", "could not revert", "patch failed at"]
        .iter()
        .any(|m| lower.contains(m))
}

/// Single-quotes a path for sh, with forward slashes.
fn sh_quote(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A tiny script git runs as its sequence editor: copies our todo over git's.
fn sequence_editor(todo: &std::path::Path) -> Result<String> {
    // git runs the editor through sh, available with every git installation.
    Ok(format!("cp {}", sh_quote(todo)))
}

#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    pub message: String,
    pub amend: bool,
    pub signoff: bool,
    pub no_verify: bool,
    pub allow_empty: bool,
    /// `Some(true)` forces signing, `Some(false)` disables it, `None` follows config.
    pub sign: Option<bool>,
    /// "Name <email>"
    pub author: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FastForward {
    #[default]
    Allow,
    Never,
    Only,
}

#[derive(Debug, Clone)]
pub struct MergeOptions {
    pub ff: FastForward,
    pub squash: bool,
    /// Commit the merge right away (default); false stops before committing.
    pub commit: bool,
    pub message: Option<String>,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            ff: FastForward::Allow,
            squash: false,
            commit: true,
            message: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

#[derive(Debug, Clone, Default)]
pub struct StashOptions {
    pub keep_index: bool,
    pub include_untracked: bool,
    pub staged_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    pub prune: bool,
    pub tags: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    pub remote: String,
    /// (local ref, remote branch name)
    pub refspecs: Vec<(String, String)>,
    pub force: bool,
    pub set_upstream: bool,
    pub tags: bool,
    pub no_verify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl TodoAction {
    pub fn word(self) -> &'static str {
        match self {
            TodoAction::Pick => "pick",
            TodoAction::Reword => "reword",
            TodoAction::Edit => "edit",
            TodoAction::Squash => "squash",
            TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub action: TodoAction,
    pub id: String,
    pub summary: String,
    /// New message for `Reword` (applied via `exec git commit --amend -F`).
    pub new_message: Option<String>,
}

/// NUL-separated `:(literal)` pathspecs, so `*`, `?` and `[` in file names
/// never match other files.
pub(crate) fn literal_paths(paths: &[String]) -> Vec<u8> {
    nul_paths(paths.iter().map(|p| format!(":(literal){p}")))
}

/// Rejects user-supplied names, revisions, URLs and paths that git would
/// parse as options (e.g. a "branch" called `--force`).
pub(crate) fn plain(args: &[&str]) -> Result<()> {
    if let Some(a) = args.iter().find(|a| a.trim_start().starts_with('-')) {
        bail!("'{a}' is not allowed: names may not start with '-'");
    }
    Ok(())
}

/// Splits paths into groups whose `:(literal)` pathspecs fit on one command line.
pub(crate) fn arg_chunks(paths: &[String]) -> Vec<&[String]> {
    const MAX: usize = 24_000;
    let mut out = Vec::new();
    let (mut start, mut len) = (0, 0);
    for (i, p) in paths.iter().enumerate() {
        let n = p.len() + 12;
        if len + n > MAX && i > start {
            out.push(&paths[start..i]);
            start = i;
            len = 0;
        }
        len += n;
    }
    if start < paths.len() {
        out.push(&paths[start..]);
    }
    out
}

/// Above this many paths, pathspec matching (every file against every
/// pathspec) dominates; plain path lists through update-index are linear.
const BULK_PATHS: usize = 1000;
