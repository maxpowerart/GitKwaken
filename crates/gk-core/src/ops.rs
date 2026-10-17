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
        self.git()
            .run_stdin(&["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"], &nul_paths(paths))?;
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
                &["rm", "--cached", "-r", "-q", "--pathspec-from-file=-", "--pathspec-file-nul"],
                &nul_paths(paths),
            )?;
        } else {
            self.git().run_stdin(
                &["reset", "-q", "--pathspec-from-file=-", "--pathspec-file-nul", "--"],
                &nul_paths(paths),
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
                &["checkout", "--pathspec-from-file=-", "--pathspec-file-nul", "--"],
                &nul_paths(tracked),
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
    pub fn stage_lines(&self, path: &str, unstage: bool, pick: &dyn Fn(&DLine) -> bool) -> Result<()> {
        let g = self.gix();
        let (old, new, mode) = if unstage {
            let t = DiffTarget::Staged { path: path.to_string(), old_path: None };
            let (o, n) = content::load_sides(self, &t)?;
            (o, n, SelectMode::Revert)
        } else {
            // Untracked files need an intent-to-add entry first.
            if content::index_blob(&g, path, 0)?.is_none() {
                self.git().run(&["add", "-N", "--", path])?;
            }
            let t = DiffTarget::Unstaged { path: path.to_string() };
            let (o, n) = content::load_sides(self, &t)?;
            (o, n, SelectMode::Apply)
        };
        let d = diff(old.as_deref().unwrap_or_default(), new.as_deref().unwrap_or_default(), Options::full());
        if d.binary {
            bail!("cannot stage lines of a binary file");
        }
        let result = apply_selection(&d, pick, mode);
        let mode_str = index_mode(&g, path).unwrap_or_else(|| "100644".to_string());
        let id = self.git().run_stdin(&["hash-object", "-w", "--stdin", "--no-filters"], &result)?.stdout_str();
        let id = id.trim();
        let info = format!("{mode_str},{id},{path}");
        self.git().run(&["update-index", "--add", "--cacheinfo", &info])?;
        Ok(())
    }

    /// Reverts selected lines in the worktree file.
    pub fn discard_lines(&self, path: &str, pick: &dyn Fn(&DLine) -> bool) -> Result<()> {
        let g = self.gix();
        let t = DiffTarget::Unstaged { path: path.to_string() };
        let (old, new) = content::load_sides(self, &t)?;
        let d = diff(old.as_deref().unwrap_or_default(), new.as_deref().unwrap_or_default(), Options::full());
        if d.binary {
            bail!("cannot discard lines of a binary file");
        }
        let result = apply_selection(&d, pick, SelectMode::Revert);
        content::write_worktree(self, &g, path, &result)
    }

    // ---------------------------------------------------------------- commit

    pub fn commit(&self, opts: &CommitOptions) -> Result<String> {
        let mut args: Vec<&str> = vec!["commit", "-F", "-", "--cleanup=strip"];
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
        if opts.message.trim().is_empty() && !opts.amend {
            bail!("commit message is empty");
        }
        let out = self.git().run_stdin(&args, opts.message.as_bytes())?;
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
        self.git().run(&["checkout", rev])?;
        Ok(())
    }

    /// Checks out a remote branch as a new local tracking branch.
    pub fn checkout_remote(&self, remote_branch: &str, local: &str) -> Result<()> {
        self.git().run(&["checkout", "-b", local, "--track", remote_branch])?;
        Ok(())
    }

    pub fn create_branch(&self, name: &str, start: Option<&str>, checkout: bool) -> Result<()> {
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
        self.git().run(&["branch", if force { "-D" } else { "-d" }, name])?;
        Ok(())
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        self.git().run(&["branch", "-m", old, new])?;
        Ok(())
    }

    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<()> {
        match upstream {
            Some(u) => self.git().run(&["branch", "--set-upstream-to", u, branch])?,
            None => self.git().run(&["branch", "--unset-upstream", branch])?,
        };
        Ok(())
    }

    pub fn delete_remote_branch(&self, remote: &str, branch: &str, cancel: &CancelToken, p: Progress) -> Result<()> {
        let refspec = format!(":refs/heads/{branch}");
        self.git()
            .run_streaming(&["push", "--progress", remote, &refspec], None, cancel, p)?
            .check()?;
        Ok(())
    }

    // ---------------------------------------------------------------- tags

    pub fn create_tag(&self, name: &str, rev: &str, message: Option<&str>, sign: bool) -> Result<()> {
        match message {
            Some(m) if !m.trim().is_empty() => {
                let flag = if sign { "-s" } else { "-a" };
                self.git().run_stdin(&["tag", flag, name, rev, "-F", "-"], m.as_bytes())?;
            }
            _ => {
                self.git().run(&["tag", name, rev])?;
            }
        }
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.git().run(&["tag", "-d", name])?;
        Ok(())
    }

    // ---------------------------------------------------------------- history rewriting

    pub fn merge(&self, rev: &str, opts: &MergeOptions) -> Result<String> {
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
        let out = self.git().env("GIT_EDITOR", "true").run_raw(&["rebase", onto], None)?;
        conflict_aware(out)
    }

    /// Interactive rebase driven by a prepared todo list.
    pub fn rebase_interactive(&self, base: &str, todo: &[TodoItem], autosquash: bool) -> Result<String> {
        let dir = std::env::temp_dir().join(format!("gitkwaken-todo-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let mut todo_text = String::new();
        for (i, t) in todo.iter().enumerate() {
            match (t.action, &t.new_message) {
                // Reword without an editor: pick, then amend with the prepared message.
                (TodoAction::Reword, Some(msg)) => {
                    let msg_file = dir.join(format!("msg-{i}"));
                    std::fs::write(&msg_file, msg)?;
                    let mf = msg_file.to_string_lossy().replace('\\', "/");
                    todo_text.push_str(&format!("pick {} {}\n", t.id, t.summary));
                    todo_text.push_str(&format!("exec git commit --amend -q --allow-empty -F '{mf}'\n"));
                }
                _ => todo_text.push_str(&format!("{} {} {}\n", t.action.word(), t.id, t.summary)),
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
            .run_raw(&args, None)?;
        conflict_aware(out)
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
            self.git().run_stdin(&args, &nul_paths(paths))?;
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
        self.git().run(&["stash", "drop", &format!("stash@{{{index}}}")])?;
        Ok(())
    }

    pub fn stash_branch(&self, index: usize, branch: &str) -> Result<()> {
        self.git().run(&["stash", "branch", branch, &format!("stash@{{{index}}}")])?;
        Ok(())
    }

    // ---------------------------------------------------------------- remotes

    pub fn fetch(&self, remote: Option<&str>, opts: &FetchOptions, cancel: &CancelToken, p: Progress) -> Result<String> {
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

    pub fn pull(&self, remote: Option<&str>, branch: Option<&str>, rebase: bool, cancel: &CancelToken, p: Progress) -> Result<String> {
        let mut args = vec!["pull", "--progress", if rebase { "--rebase" } else { "--no-rebase" }];
        if let Some(r) = remote {
            args.push(r);
            if let Some(b) = branch {
                args.push(b);
            }
        }
        let out = self.git().env("GIT_EDITOR", "true").run_streaming(&args, None, cancel, p)?;
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
        args.push(opts.remote.clone());
        for (local, remote) in &opts.refspecs {
            if remote.is_empty() || remote == local {
                args.push(local.clone());
            } else {
                args.push(format!("{local}:{remote}"));
            }
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.git().run_streaming(&args, None, cancel, p)?.check()?;
        Ok(out.stderr_str())
    }

    pub fn push_tag(&self, remote: &str, tag: &str, delete: bool, cancel: &CancelToken, p: Progress) -> Result<()> {
        let spec = if delete { format!(":refs/tags/{tag}") } else { format!("refs/tags/{tag}") };
        self.git().run_streaming(&["push", "--progress", remote, &spec], None, cancel, p)?.check()?;
        Ok(())
    }

    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.git().run(&["remote", "add", name, url])?;
        Ok(())
    }

    pub fn remove_remote(&self, name: &str) -> Result<()> {
        self.git().run(&["remote", "remove", name])?;
        Ok(())
    }

    pub fn rename_remote(&self, old: &str, new: &str) -> Result<()> {
        self.git().run(&["remote", "rename", old, new])?;
        Ok(())
    }

    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
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
        self.git().run(&["config", "--get", key]).ok().map(|o| o.stdout_str().trim().to_string())
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
        let o = out.to_string_lossy().to_string();
        self.git().run(&["archive", "-o", &o, rev])?;
        Ok(())
    }
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
    if out.success() {
        return Ok(text);
    }
    let lower = text.to_lowercase();
    if lower.contains("conflict") || lower.contains("could not apply") {
        return Ok(text);
    }
    Err(anyhow!(text.trim().to_string()))
}

/// A tiny script git runs as its sequence editor: copies our todo over git's.
fn sequence_editor(todo: &std::path::Path) -> Result<String> {
    let t = todo.to_string_lossy().replace('\\', "/");
    // git runs the editor through sh, available with every git installation.
    Ok(format!("cp '{t}'"))
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

#[derive(Debug, Clone, Default)]
pub struct MergeOptions {
    pub ff: FastForward,
    pub squash: bool,
    pub commit: bool,
    pub message: Option<String>,
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
