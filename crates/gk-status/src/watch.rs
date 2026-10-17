//! Turns raw file-system notifications into debounced refresh requests.
//!
//! Events are split into worktree paths (re-check just those paths) and
//! `.git` changes (index, refs, HEAD...). Bursts larger than
//! [`OVERFLOW_PATHS`] collapse into a single full-refresh request.

use bstr::BString;
use crossbeam_channel::{Receiver, Sender, select, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Above this many distinct paths in one batch a full rescan is cheaper.
pub const OVERFLOW_PATHS: usize = 2000;

/// Quiet time that ends a burst. An editor save is a handful of events
/// within a few milliseconds, so a short window keeps save-to-status latency
/// low (the status itself then takes ~1 ms on the fast path).
pub const DEBOUNCE: Duration = Duration::from_millis(15);
/// Longest a burst is held back while events keep coming.
pub const MAX_DELAY: Duration = Duration::from_millis(400);
/// Burst coalescing: after [`BUSY_FLUSHES`] deliveries within
/// [`BUSY_WINDOW`] (a build, a checkout by another tool, an unpacking
/// archive) the watcher switches to [`BUSY_DEBOUNCE`] / [`BUSY_MAX_DELAY`]
/// until things calm down, so the session refreshes a few times per second
/// instead of after every gap in the event stream.
pub const BUSY_WINDOW: Duration = Duration::from_millis(1000);
pub const BUSY_FLUSHES: usize = 3;
pub const BUSY_DEBOUNCE: Duration = Duration::from_millis(150);
pub const BUSY_MAX_DELAY: Duration = Duration::from_millis(1000);

/// Debounce timing for the current activity level.
#[derive(Debug, Default)]
struct Pace {
    flushes: std::collections::VecDeque<Instant>,
}

impl Pace {
    fn busy(&mut self, now: Instant) -> bool {
        while self
            .flushes
            .front()
            .is_some_and(|t| now.duration_since(*t) > BUSY_WINDOW)
        {
            self.flushes.pop_front();
        }
        self.flushes.len() >= BUSY_FLUSHES
    }

    fn timing(&mut self, now: Instant) -> (Duration, Duration) {
        if self.busy(now) {
            (BUSY_DEBOUNCE, BUSY_MAX_DELAY)
        } else {
            (DEBOUNCE, MAX_DELAY)
        }
    }

    fn flushed(&mut self, now: Instant) {
        self.flushes.push_back(now);
        if self.flushes.len() > 16 {
            self.flushes.pop_front();
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitDirChange {
    pub index: bool,
    pub refs: bool,
    pub head: bool,
    /// MERGE_HEAD, rebase state, sequencer, stash...
    pub state: bool,
}

impl GitDirChange {
    fn any(&self) -> bool {
        self.index || self.refs || self.head || self.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Repository-relative paths that changed in the working tree.
    Worktree(Vec<BString>),
    GitDir(GitDirChange),
    /// Too many changes or the OS dropped events: rescan everything.
    Overflow,
}

pub struct Watcher {
    _watchers: Vec<RecommendedWatcher>,
    paused: Arc<AtomicBool>,
    drop_until: Arc<parking_lot::Mutex<Discard>>,
    stop_tx: Sender<()>,
}

impl Watcher {
    /// While paused, events are accumulated and delivered on resume.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    /// Resumes after one of our own operations: everything accumulated while
    /// paused, and everything that arrives within `grace` (late OS
    /// notifications for the operation's writes), is dropped. The caller
    /// refreshes explicitly *after* `grace` has passed, so a dropped event
    /// always describes a change that refresh already sees.
    pub fn resume_discarding(&self, grace: Duration) {
        let mut d = self.drop_until.lock();
        d.until = Some(Instant::now() + grace);
        d.generation += 1;
        drop(d);
        self.paused.store(false, Ordering::SeqCst);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
    }
}

/// Pending [`Watcher::resume_discarding`] request.
#[derive(Debug, Default)]
struct Discard {
    until: Option<Instant>,
    /// Bumps per request, so the loop clears its batch exactly once each.
    generation: u64,
}

impl Discard {
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| now < u)
    }
}

enum Raw {
    Path(PathBuf),
    Overflow,
}

/// Starts watching `workdir` (and `git_dir` when it lives outside of it).
pub fn watch(workdir: &Path, git_dir: &Path, out: Sender<Event>) -> anyhow::Result<Watcher> {
    let (raw_tx, raw_rx) = unbounded::<Raw>();
    let make = |tx: Sender<Raw>| {
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(ev) => {
                if ev.need_rescan() {
                    let _ = tx.send(Raw::Overflow);
                }
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for p in ev.paths {
                    let _ = tx.send(Raw::Path(p));
                }
            }
            Err(_) => {
                let _ = tx.send(Raw::Overflow);
            }
        })
    };
    let workdir = dunce(workdir);
    let git_dir = dunce(git_dir);
    let mut watchers = Vec::new();
    let mut w = make(raw_tx.clone())?;
    w.watch(&workdir, RecursiveMode::Recursive)?;
    watchers.push(w);
    if !git_dir.starts_with(&workdir) {
        let mut w = make(raw_tx.clone())?;
        w.watch(&git_dir, RecursiveMode::Recursive)?;
        watchers.push(w);
    }

    let paused = Arc::new(AtomicBool::new(false));
    let drop_until = Arc::new(parking_lot::Mutex::new(Discard::default()));
    let (stop_tx, stop_rx) = unbounded();
    let classifier = Classifier { workdir, git_dir };
    let ctl = Control {
        paused: paused.clone(),
        drop_until: drop_until.clone(),
    };
    std::thread::Builder::new()
        .name("gk-watch".into())
        .stack_size(256 << 10)
        .spawn(move || debounce_loop(raw_rx, stop_rx, out, classifier, ctl))?;
    Ok(Watcher {
        _watchers: watchers,
        paused,
        drop_until,
        stop_tx,
    })
}

struct Control {
    paused: Arc<AtomicBool>,
    drop_until: Arc<parking_lot::Mutex<Discard>>,
}

/// Removes the `\\?\` prefix canonicalize adds on Windows so prefixes compare.
fn dunce(p: &Path) -> PathBuf {
    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

struct Classifier {
    workdir: PathBuf,
    git_dir: PathBuf,
}

enum Class {
    Worktree(BString),
    GitDir(GitDirChange),
    Skip,
}

impl Classifier {
    fn classify(&self, path: &Path) -> Class {
        if let Ok(rel) = path.strip_prefix(&self.git_dir) {
            return classify_git(rel);
        }
        let Ok(rel) = path.strip_prefix(&self.workdir) else {
            return Class::Skip;
        };
        let mut s = String::new();
        for (i, c) in rel.components().enumerate() {
            if i > 0 {
                s.push('/');
            }
            s.push_str(&c.as_os_str().to_string_lossy());
        }
        if s.is_empty() || s == ".git" {
            return Class::Skip;
        }
        Class::Worktree(s.into())
    }
}

fn classify_git(rel: &Path) -> Class {
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.ends_with(".lock") || s.starts_with("objects") || s.starts_with("logs") || s.is_empty() {
        return Class::Skip;
    }
    let mut c = GitDirChange::default();
    match s.as_str() {
        "index" => c.index = true,
        "HEAD" => c.head = true,
        "packed-refs" | "FETCH_HEAD" | "ORIG_HEAD" => c.refs = true,
        _ if s.starts_with("refs/") => c.refs = true,
        "MERGE_HEAD" | "MERGE_MSG" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "BISECT_LOG"
        | "AUTO_MERGE" => c.state = true,
        _ if s.starts_with("rebase-merge")
            || s.starts_with("rebase-apply")
            || s.starts_with("sequencer") =>
        {
            c.state = true
        }
        _ if s.starts_with("modules/") || s.starts_with("worktrees/") => c.state = true,
        _ => return Class::Skip,
    }
    Class::GitDir(c)
}

fn debounce_loop(
    raw: Receiver<Raw>,
    stop: Receiver<()>,
    out: Sender<Event>,
    classifier: Classifier,
    ctl: Control,
) {
    let mut paths: BTreeSet<BString> = BTreeSet::new();
    let mut git = GitDirChange::default();
    let mut overflow = false;
    let mut first: Option<Instant> = None;
    let mut last = Instant::now();
    let mut pace = Pace::default();
    let mut seen_discard = 0u64;
    let (mut debounce, mut max_delay) = (DEBOUNCE, MAX_DELAY);
    loop {
        let timeout = if first.is_some() {
            debounce
        } else {
            Duration::from_secs(3600)
        };
        // An own operation just ended: forget what it caused.
        let dropping = {
            let d = ctl.drop_until.lock();
            if d.generation != seen_discard {
                seen_discard = d.generation;
                paths.clear();
                git = GitDirChange::default();
                overflow = false;
                first = None;
            }
            d.active(Instant::now())
        };
        select! {
            recv(stop) -> _ => return,
            recv(raw) -> msg => {
                let Ok(msg) = msg else { return };
                if dropping && ctl.drop_until.lock().active(Instant::now()) {
                    continue;
                }
                match msg {
                    Raw::Overflow => overflow = true,
                    Raw::Path(p) => match classifier.classify(&p) {
                        Class::Worktree(rel) => {
                            if paths.len() <= OVERFLOW_PATHS {
                                paths.insert(rel);
                            } else {
                                overflow = true;
                            }
                        }
                        Class::GitDir(c) => {
                            git.index |= c.index;
                            git.refs |= c.refs;
                            git.head |= c.head;
                            git.state |= c.state;
                        }
                        Class::Skip => continue,
                    },
                }
                last = Instant::now();
                if first.is_none() {
                    first = Some(last);
                    (debounce, max_delay) = pace.timing(last);
                }
                // Keep collecting while the burst continues, but never longer than max_delay.
                if first.is_some_and(|f| f.elapsed() < max_delay) {
                    continue;
                }
            }
            default(timeout) => {}
        }
        let Some(f) = first else { continue };
        if last.elapsed() < debounce && f.elapsed() < max_delay {
            continue;
        }
        if ctl.paused.load(Ordering::SeqCst) || ctl.drop_until.lock().active(Instant::now()) {
            continue;
        }
        first = None;
        pace.flushed(Instant::now());
        if overflow || paths.len() > OVERFLOW_PATHS {
            let _ = out.send(Event::Overflow);
            if git.any() {
                let _ = out.send(Event::GitDir(std::mem::take(&mut git)));
            }
            overflow = false;
            paths.clear();
            continue;
        }
        if git.any() {
            let _ = out.send(Event::GitDir(std::mem::take(&mut git)));
        }
        if !paths.is_empty() {
            let _ = out.send(Event::Worktree(collapse(std::mem::take(&mut paths))));
        }
    }
}

/// Drops paths that are already covered by a parent directory in the set.
fn collapse(paths: BTreeSet<BString>) -> Vec<BString> {
    let mut out: Vec<BString> = Vec::with_capacity(paths.len());
    for p in paths {
        if let Some(prev) = out.last()
            && crate::path_within(p.as_ref(), prev.as_ref())
        {
            continue;
        }
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_dir_classification() {
        assert!(matches!(
            classify_git(Path::new("index")),
            Class::GitDir(GitDirChange { index: true, .. })
        ));
        assert!(matches!(
            classify_git(Path::new("refs/heads/main")),
            Class::GitDir(GitDirChange { refs: true, .. })
        ));
        assert!(matches!(classify_git(Path::new("index.lock")), Class::Skip));
        assert!(matches!(
            classify_git(Path::new("objects/ab/cdef")),
            Class::Skip
        ));
    }

    #[test]
    fn collapse_parents() {
        let set: BTreeSet<BString> = ["a", "a/b", "a/c/d", "ab", "b/x"]
            .iter()
            .map(|s| BString::from(*s))
            .collect();
        let v: Vec<String> = collapse(set).iter().map(|s| s.to_string()).collect();
        assert_eq!(v, ["a", "ab", "b/x"]);
    }

    #[test]
    fn delivers_worktree_events() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        let (tx, rx) = unbounded();
        let _w = watch(dir.path(), &git_dir, tx).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.path().join("hello.txt"), "x").unwrap();
        std::fs::write(git_dir.join("index"), "x").unwrap();
        let mut saw_file = false;
        let mut saw_index = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(saw_file && saw_index) {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                match ev {
                    Event::Worktree(p) => saw_file |= p.iter().any(|p| p == "hello.txt"),
                    Event::GitDir(c) => saw_index |= c.index,
                    Event::Overflow => {}
                }
            }
        }
        assert!(saw_file && saw_index);
    }

    #[test]
    fn pace_switches_to_busy_after_frequent_flushes() {
        let mut p = Pace::default();
        let t0 = Instant::now();
        assert_eq!(p.timing(t0), (DEBOUNCE, MAX_DELAY));
        for i in 0..BUSY_FLUSHES {
            p.flushed(t0 + Duration::from_millis(100 * i as u64));
        }
        let t1 = t0 + Duration::from_millis(400);
        assert_eq!(p.timing(t1), (BUSY_DEBOUNCE, BUSY_MAX_DELAY));
        // Calm again once the flushes age out of the window.
        assert_eq!(p.timing(t1 + BUSY_WINDOW), (DEBOUNCE, MAX_DELAY));
    }

    fn next_worktree_event(rx: &Receiver<Event>, within: Duration) -> Option<Vec<BString>> {
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Event::Worktree(p)) => return Some(p),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    fn save_reaches_the_session_quickly() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        let (tx, rx) = unbounded();
        let _w = watch(dir.path(), &git_dir, tx).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut best = Duration::MAX;
        for i in 0..5 {
            let t = Instant::now();
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
            let got = next_worktree_event(&rx, Duration::from_secs(2)).expect("event");
            best = best.min(t.elapsed());
            assert!(got.iter().any(|p| *p == format!("f{i}.txt")));
            // Stay below the busy threshold.
            std::thread::sleep(BUSY_WINDOW / 2);
        }
        eprintln!("watcher save-to-event latency (best of 5): {best:?}");
        assert!(best < Duration::from_millis(100), "{best:?}");
    }

    #[test]
    fn resume_discarding_drops_own_changes() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        let (tx, rx) = unbounded();
        let w = watch(dir.path(), &git_dir, tx).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        w.pause();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("own{i}.txt")), "x").unwrap();
        }
        std::fs::write(git_dir.join("index"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        w.resume_discarding(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "events of the paused period must be dropped"
        );
        // Later changes are delivered as usual.
        std::fs::write(dir.path().join("user.txt"), "x").unwrap();
        let got = next_worktree_event(&rx, Duration::from_secs(2)).expect("event");
        assert_eq!(got, vec![BString::from("user.txt")]);
    }
}
