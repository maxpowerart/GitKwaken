//! "Open in external diff tool": both sides of a diff target are written to
//! temp files (the worktree file itself is used for the new side of an
//! unstaged diff, so edits land in the repository) and the configured tool
//! starts with `$LOCAL` / `$REMOTE` replaced by their quoted paths.

use anyhow::{Context as _, Result};
use gk_core::content::DiffTarget;
use gk_core::{MessageKind, Repo, Session};
use gk_settings::ExternalTool;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Temp folders older than this are removed when a tool starts.
const KEEP: Duration = Duration::from_secs(24 * 3600);

fn temp_root() -> PathBuf {
    std::env::temp_dir().join("gitkwaken-diff")
}

/// Removes temp folders of earlier launches (tools may still read the
/// current ones after they return, so nothing is deleted on exit).
fn prune(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Writes the sides of `target`; returns (old path, new path).
pub fn write_sides(repo: &Repo, target: &DiffTarget, dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let (old, new) = gk_core::content::load_sides(repo, target)?;
    std::fs::create_dir_all(dir)?;
    let path = target.path();
    let file = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (file, String::new()),
    };
    let old_label = match target {
        DiffTarget::Unstaged { .. } => "INDEX",
        DiffTarget::Staged { .. } => "HEAD",
        DiffTarget::Blobs { .. } => "OLD",
    };
    let old_path = dir.join(format!("{stem}.{old_label}{ext}"));
    std::fs::write(&old_path, old.unwrap_or_default())?;
    let worktree = repo.abs(path);
    let new_path = match target {
        DiffTarget::Unstaged { .. } if worktree.is_file() => worktree,
        _ => {
            let label = if matches!(target, DiffTarget::Staged { .. }) {
                "STAGED"
            } else {
                "NEW"
            };
            let p = dir.join(format!("{stem}.{label}{ext}"));
            std::fs::write(&p, new.unwrap_or_default())?;
            p
        }
    };
    Ok((old_path, new_path))
}

fn run(tool: &ExternalTool, repo: &Repo, target: &DiffTarget) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let root = temp_root();
    prune(&root);
    let dir = root.join(format!(
        "{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let (old, new) = write_sides(repo, target, &dir)?;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let args = tool.expand_args(&s(&old), &s(&new), "", &s(&new));
    #[cfg(windows)]
    let mut cmd = {
        // The template is a Windows command line already (quoted paths).
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new(&tool.command);
        c.raw_arg(&args);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(format!("\"{}\" {args}", tool.command));
        c
    };
    cmd.current_dir(repo.workdir());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot start {}", tool.command))?;
    // Diff tools exit non-zero when files differ: only start failures count.
    let _ = child.wait();
    Ok(())
}

/// Opens `target` in `settings.external_diff_tool` (on a worker thread);
/// errors go to the tab's messages.
pub fn open(session: &Session, target: &DiffTarget, tool: Option<&ExternalTool>) {
    let Some(tool) = tool.filter(|t| !t.command.trim().is_empty()).cloned() else {
        session.push_message(
            MessageKind::Error,
            "No external diff tool is configured (Settings › External tools).",
        );
        return;
    };
    let (s2, target) = (session.clone(), target.clone());
    let spawned = std::thread::Builder::new()
        .name("gk-difftool".into())
        .spawn(move || {
            if let Err(e) = run(&tool, s2.repo(), &target) {
                s2.push_message(
                    MessageKind::Error,
                    format!("External diff tool for {}: {e:#}", target.path()),
                );
            }
        });
    if let Err(e) = spawned {
        session.push_message(MessageKind::Error, format!("External diff tool: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_both_sides() {
        let root = std::env::temp_dir().join(format!("gk-ext-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo_dir = root.join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo_dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(repo_dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(repo_dir.join("a.txt"), "two\n").unwrap();
        git(&["add", "a.txt"]);
        std::fs::write(repo_dir.join("a.txt"), "three\n").unwrap();
        let repo = Repo::open(&repo_dir, gk_cli::CommandLog::default()).unwrap();

        let un = DiffTarget::Unstaged {
            path: "a.txt".into(),
        };
        let (o, n) = write_sides(&repo, &un, &root.join("t1")).unwrap();
        assert_eq!(std::fs::read_to_string(o).unwrap(), "two\n");
        assert_eq!(n, repo.abs("a.txt"), "worktree file is the new side");

        let st = DiffTarget::Staged {
            path: "a.txt".into(),
            old_path: None,
        };
        let (o, n) = write_sides(&repo, &st, &root.join("t2")).unwrap();
        assert_eq!(std::fs::read_to_string(o).unwrap(), "one\n");
        assert_eq!(std::fs::read_to_string(&n).unwrap(), "two\n");
        assert!(n.to_string_lossy().ends_with("a.STAGED.txt"));
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }
}
