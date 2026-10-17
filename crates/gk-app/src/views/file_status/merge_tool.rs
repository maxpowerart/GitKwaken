//! Launching the configured external merge tool for a conflicted file.
//! The conflict stages are extracted to temp files on a worker thread; when
//! the tool exits the UI asks whether to mark the file resolved.

use anyhow::{Context as _, Result, bail};
use gk_core::Repo;
use gk_settings::ExternalTool;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A merge tool that has exited.
pub struct ToolExit {
    pub path: String,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct MergeTools {
    done: Arc<Mutex<Vec<ToolExit>>>,
    running: Arc<Mutex<Vec<String>>>,
}

impl MergeTools {
    pub fn launch(&self, tool: ExternalTool, repo: Repo, path: String, ctx: egui::Context) {
        if self.running.lock().contains(&path) {
            return;
        }
        self.running.lock().push(path.clone());
        let done = self.done.clone();
        let running = self.running.clone();
        let spawned = std::thread::Builder::new().name("gk-mergetool".into()).spawn(move || {
            let r = run(&tool, &repo, &path);
            running.lock().retain(|p| p != &path);
            done.lock().push(ToolExit { path, error: r.err().map(|e| format!("{e:#}")) });
            ctx.request_repaint();
        });
        if spawned.is_err() {
            self.running.lock().clear();
        }
    }

    pub fn take_finished(&self) -> Vec<ToolExit> {
        std::mem::take(&mut *self.done.lock())
    }

    pub fn is_running(&self) -> bool {
        !self.running.lock().is_empty()
    }
}

fn run(tool: &ExternalTool, repo: &Repo, path: &str) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gitkwaken-merge-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let file = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (file, String::new()),
    };
    let mut sides = Vec::new();
    for (stage, tag) in [(1, "BASE"), (2, "LOCAL"), (3, "REMOTE")] {
        let spec = format!(":{stage}:{path}");
        let out = repo.git().run_raw(&["show", &spec], None)?;
        let p = dir.join(format!("{stem}.{tag}{ext}"));
        // A missing stage (added/deleted on one side) becomes an empty file.
        std::fs::write(&p, if out.success() { out.stdout.as_slice() } else { &[] })?;
        sides.push(p);
    }
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let merged = repo.abs(path);
    let args = tool.expand_args(&s(&sides[1]), &s(&sides[2]), &s(&sides[0]), &s(&merged));
    let mut cmd = std::process::Command::new(&tool.command);
    cmd.current_dir(repo.workdir());
    #[cfg(windows)]
    {
        // The template is a Windows command line already (quoted paths).
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(&args);
    }
    #[cfg(not(windows))]
    cmd.args(split_args(&args));
    let status = cmd.status().with_context(|| format!("cannot start {}", tool.command))?;
    let _ = std::fs::remove_dir_all(&dir);
    if !status.success() {
        let name = if tool.name.is_empty() { &tool.command } else { &tool.name };
        bail!("{name} exited with {status}");
    }
    Ok(())
}

/// Splits a command line into arguments, honoring double quotes.
#[cfg_attr(windows, allow(dead_code))]
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_args() {
        assert_eq!(split_args(r#"-m "a b.txt"  "" c"#), vec!["-m", "a b.txt", "", "c"]);
        let t = ExternalTool { name: "x".into(), command: "x".into(), args: "$LOCAL $REMOTE -o $MERGED".into() };
        assert_eq!(split_args(&t.expand_args("l 1", "r", "b", "m")), vec!["l 1", "r", "-o", "m"]);
    }
}
