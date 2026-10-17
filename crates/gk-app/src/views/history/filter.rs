//! History filters (message text, author, date range, path, pickaxe): run
//! `git log --format=%H` with the matching options on a background thread
//! and stream the commit ids into a flat row list. A change of the filter
//! or of the history cancels the running query and starts a new one.

use gk_core::ObjectId;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Delay after the last keystroke before a query runs.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pickaxe {
    /// `-S<text>`: commits changing the number of occurrences of a string.
    #[default]
    String,
    /// `-G<regex>`: commits whose diff has added/removed lines matching.
    Regex,
}

/// What the user typed into the filter fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    /// Commit message (`--grep`, case-insensitive).
    pub text: String,
    pub author: String,
    /// `--since` / `--until`, any date git understands ("2026-01-01").
    pub since: String,
    pub until: String,
    /// Limits to commits touching this path (or folder).
    pub path: String,
    pub pickaxe: String,
    pub pickaxe_mode: Pickaxe,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        self.count() > 0
    }

    /// Number of non-empty fields.
    pub fn count(&self) -> usize {
        [
            &self.text,
            &self.author,
            &self.since,
            &self.until,
            &self.path,
            &self.pickaxe,
        ]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .count()
    }

    /// Fields besides the message text (shown in the filter row).
    pub fn extra_count(&self) -> usize {
        self.count() - usize::from(!self.text.trim().is_empty())
    }

    pub fn clear(&mut self) {
        let mode = self.pickaxe_mode;
        *self = Self::default();
        self.pickaxe_mode = mode;
    }

    /// `git log` arguments for this filter over `revs` (revision arguments
    /// such as `--branches` or branch names).
    pub fn args(&self, revs: &[String]) -> Vec<String> {
        let mut a: Vec<String> = vec!["log".into(), "--format=%H".into()];
        let t = |s: &str| s.trim().to_string();
        if !self.text.trim().is_empty() || !self.author.trim().is_empty() {
            a.push("--regexp-ignore-case".into());
        }
        if !self.text.trim().is_empty() {
            a.push(format!("--grep={}", t(&self.text)));
        }
        if !self.author.trim().is_empty() {
            a.push(format!("--author={}", t(&self.author)));
        }
        if !self.since.trim().is_empty() {
            a.push(format!("--since={}", t(&self.since)));
        }
        if !self.until.trim().is_empty() {
            a.push(format!("--until={}", t(&self.until)));
        }
        if !self.pickaxe.is_empty() {
            match self.pickaxe_mode {
                Pickaxe::String => a.push(format!("-S{}", self.pickaxe)),
                Pickaxe::Regex => a.push(format!("-G{}", self.pickaxe)),
            }
        }
        a.extend(revs.iter().cloned());
        a.push("--".into());
        if !self.path.trim().is_empty() {
            a.push(t(&self.path).replace('\\', "/"));
        }
        a
    }
}

/// A running (or finished) query.
struct Stream {
    ids: Arc<Mutex<Vec<ObjectId>>>,
    done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    cancel: gk_cli::CancelToken,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
pub struct FilterState {
    /// Fields as edited.
    pub edit: Filter,
    /// Filter row visible.
    pub open: bool,
    edited_at: Option<Instant>,
    /// Query running or shown: (filter, revision args, history identity).
    key: Option<(Filter, Vec<String>, usize)>,
    stream: Option<Stream>,
    /// Ids received so far, newest first, and their positions.
    pub rows: Arc<Vec<ObjectId>>,
    index: HashMap<ObjectId, usize>,
}

impl FilterState {
    /// Call after editing a field (debounces the query).
    pub fn edited(&mut self) {
        self.edited_at = Some(Instant::now());
    }

    pub fn clear(&mut self) {
        self.edit.clear();
        self.edited_at = None;
        self.key = None;
        self.stream = None;
        self.rows = Arc::default();
        self.index.clear();
    }

    /// The filter applies (rows replace the graph).
    pub fn active(&self) -> bool {
        self.key.as_ref().is_some_and(|k| k.0.is_active())
    }

    pub fn running(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|s| !s.done.load(Ordering::SeqCst))
    }

    pub fn error(&self) -> Option<String> {
        self.stream.as_ref().and_then(|s| s.error.lock().clone())
    }

    pub fn row_of(&self, id: &ObjectId) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// Starts / restarts the query when the (debounced) filter, the revisions
    /// or the history changed, and collects streamed rows.
    pub fn sync(
        &mut self,
        ctx: &egui::Context,
        git: &gk_cli::Git,
        revs: Vec<String>,
        history_id: usize,
    ) {
        if let Some(t) = self.edited_at {
            if t.elapsed() < DEBOUNCE {
                ctx.request_repaint_after(DEBOUNCE.saturating_sub(t.elapsed()));
            } else {
                self.edited_at = None;
            }
        }
        if self.edited_at.is_none() {
            let key = (self.edit.clone(), revs, history_id);
            if self.key.as_ref() != Some(&key) {
                self.start(ctx, git, &key);
                self.key = Some(key);
            }
        }
        if let Some(s) = &self.stream {
            let ids = s.ids.lock();
            if ids.len() != self.rows.len() {
                for (i, id) in ids.iter().enumerate().skip(self.rows.len()) {
                    self.index.insert(*id, i);
                }
                self.rows = Arc::new(ids.clone());
            }
        }
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        git: &gk_cli::Git,
        key: &(Filter, Vec<String>, usize),
    ) {
        self.stream = None; // cancels the previous query
        self.rows = Arc::default();
        self.index.clear();
        if !key.0.is_active() {
            return;
        }
        let s = Stream {
            ids: Arc::default(),
            done: Arc::default(),
            error: Arc::default(),
            cancel: gk_cli::CancelToken::new(),
        };
        let (ids, done, error, cancel) = (
            s.ids.clone(),
            s.done.clone(),
            s.error.clone(),
            s.cancel.clone(),
        );
        let args = key.0.args(&key.1);
        let git = git.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("gk-history-filter".into())
            .spawn(move || {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let mut last_paint = Instant::now();
                let r = git.run_streaming(&a, None, &cancel, |l| {
                    if let gk_cli::Line::Stdout(l) = l
                        && let Ok(id) = ObjectId::from_hex(l.trim().as_bytes())
                    {
                        ids.lock().push(id);
                        if last_paint.elapsed() > Duration::from_millis(50) {
                            last_paint = Instant::now();
                            ctx.request_repaint();
                        }
                    }
                });
                match r {
                    Ok(o) if o.code != Some(0) => {
                        let e = o.stderr_str();
                        *error.lock() =
                            Some(e.lines().next().unwrap_or("git log failed").to_string());
                    }
                    Err(e) if !cancel.is_cancelled() => *error.lock() = Some(format!("{e:#}")),
                    _ => {}
                }
                done.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            })
            .ok();
        self.stream = Some(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_git_log_args() {
        let f = Filter {
            text: " fix ".into(),
            author: "ann".into(),
            since: "2026-01-01".into(),
            until: String::new(),
            path: "src\\lib.rs".into(),
            pickaxe: "f6".into(),
            pickaxe_mode: Pickaxe::Regex,
        };
        assert_eq!(f.count(), 5);
        assert_eq!(f.extra_count(), 4);
        let a = f.args(&["--branches".into()]);
        assert_eq!(
            a,
            [
                "log",
                "--format=%H",
                "--regexp-ignore-case",
                "--grep=fix",
                "--author=ann",
                "--since=2026-01-01",
                "-Gf6",
                "--branches",
                "--",
                "src/lib.rs"
            ]
        );
        let mut g = Filter {
            pickaxe: "x".into(),
            ..Default::default()
        };
        assert_eq!(g.args(&[])[2], "-Sx");
        g.clear();
        assert!(!g.is_active());
    }
}
