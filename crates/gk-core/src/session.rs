//! A repository session: background refresh driven by the file watcher, a
//! serialized operation queue, and lazily computed diffs and commit details.
//! The UI only ever reads immutable snapshots.

use crate::content::{self, CommitDetails, DiffTarget};
use crate::history::{self, History, MetaCache, Scope};
use crate::refs::{self, Refs};
use crate::Repo;
use bstr::BString;
use crossbeam_channel::{Receiver, Sender, unbounded};
use gix::ObjectId;
use gk_cli::{CancelToken, Line};
use gk_diff::FileDiff;
use gk_status::{Status, watch};
use parking_lot::{Mutex, RwLock};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub kind: MessageKind,
    pub text: String,
    pub at: Instant,
}

/// A running operation, shown in the status bar.
#[derive(Debug, Clone)]
pub struct OpProgress {
    pub label: String,
    pub last_line: String,
    pub started: Instant,
    pub cancel: CancelToken,
}

/// Everything the UI renders, cheap to clone.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub status: Arc<Status>,
    /// Bumps whenever the status changes; part of diff cache keys.
    pub status_gen: u64,
    pub status_loading: bool,
    pub refs: Arc<Refs>,
    pub history: Arc<History>,
    pub history_loading: bool,
    pub op: Option<OpProgress>,
    pub messages: Vec<Message>,
    /// Bumps on every change.
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub status: gk_status::Options,
    pub scope: Scope,
    pub watch: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self { status: Default::default(), scope: Scope::All, watch: true }
    }
}

#[derive(Debug, Clone, Default)]
struct Refresh {
    refs: bool,
    history: bool,
    /// Full status (both sides).
    status_full: bool,
    /// Worktree paths to re-check (unstaged side only).
    paths: Vec<BString>,
}

impl Refresh {
    fn all() -> Self {
        Self { refs: true, history: true, status_full: true, paths: Vec::new() }
    }
    fn merge(&mut self, o: Refresh) {
        self.refs |= o.refs;
        self.history |= o.history;
        self.status_full |= o.status_full;
        self.paths.extend(o.paths);
    }
    fn is_empty(&self) -> bool {
        !self.refs && !self.history && !self.status_full && self.paths.is_empty()
    }
}

/// Result slot of an asynchronously computed value.
pub enum Load<T> {
    Loading,
    Ready(Arc<T>),
    Failed(String),
}

impl<T> Clone for Load<T> {
    fn clone(&self) -> Self {
        match self {
            Load::Loading => Load::Loading,
            Load::Ready(v) => Load::Ready(v.clone()),
            Load::Failed(e) => Load::Failed(e.clone()),
        }
    }
}

struct Loader<K, V> {
    key: Option<K>,
    value: Option<Load<V>>,
    ticket: u64,
}

impl<K, V> Default for Loader<K, V> {
    fn default() -> Self {
        Self { key: None, value: None, ticket: 0 }
    }
}

type OpFn = Box<dyn FnOnce(&Repo, &CancelToken, &mut (dyn FnMut(Line) + Send)) -> anyhow::Result<String> + Send>;

struct OpJob {
    label: String,
    run: OpFn,
    refresh: Refresh,
}

struct Shared {
    repo: Repo,
    snap: RwLock<Snapshot>,
    repaint: Box<dyn Fn() + Send + Sync>,
    refresh_tx: Sender<Refresh>,
    op_tx: Sender<OpJob>,
    options: RwLock<SessionOptions>,
    meta: Arc<MetaCache>,
    diff: Mutex<Loader<(DiffTarget, u64, bool, u32), FileDiff>>,
    details: Mutex<Loader<ObjectId, CommitDetails>>,
    search: Mutex<Loader<(String, u64), Vec<u32>>>,
    history_gen: AtomicU64,
    status_interrupt: Mutex<Arc<AtomicBool>>,
    closed: AtomicBool,
}

/// Handle to an open repository; clone freely.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Shared>,
    _watcher: Arc<Mutex<Option<watch::Watcher>>>,
}

impl Session {
    pub fn open(repo: Repo, options: SessionOptions, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (refresh_tx, refresh_rx) = unbounded();
        let (op_tx, op_rx) = unbounded();
        let meta = Arc::new(MetaCache::new(repo.clone()));
        let inner = Arc::new(Shared {
            repo: repo.clone(),
            snap: RwLock::new(Snapshot::default()),
            repaint: Box::new(repaint),
            refresh_tx,
            op_tx,
            options: RwLock::new(options.clone()),
            meta,
            diff: Mutex::new(Loader::default()),
            details: Mutex::new(Loader::default()),
            search: Mutex::new(Loader::default()),
            history_gen: AtomicU64::new(0),
            status_interrupt: Mutex::new(Arc::new(AtomicBool::new(false))),
            closed: AtomicBool::new(false),
        });
        {
            let i = inner.clone();
            std::thread::Builder::new()
                .name("gk-refresh".into())
                .spawn(move || refresh_loop(i, refresh_rx))
                .expect("spawn refresh thread");
        }
        {
            let i = inner.clone();
            std::thread::Builder::new()
                .name("gk-ops".into())
                .spawn(move || op_loop(i, op_rx))
                .expect("spawn op thread");
        }
        let watcher = Arc::new(Mutex::new(None));
        if options.watch {
            let (ev_tx, ev_rx) = unbounded();
            match watch::watch(repo.workdir(), repo.git_dir(), ev_tx) {
                Ok(w) => {
                    *watcher.lock() = Some(w);
                    let i = inner.clone();
                    std::thread::Builder::new()
                        .name("gk-events".into())
                        .spawn(move || event_loop(i, ev_rx))
                        .expect("spawn event thread");
                }
                Err(e) => push_message(&inner, MessageKind::Error, format!("file watcher unavailable: {e}")),
            }
        }
        let s = Self { inner, _watcher: watcher };
        s.refresh_all();
        s
    }

    pub fn repo(&self) -> &Repo {
        &self.inner.repo
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.snap.read().clone()
    }

    pub fn options(&self) -> SessionOptions {
        self.inner.options.read().clone()
    }

    pub fn set_options(&self, o: SessionOptions) {
        let scope_changed = self.inner.options.read().scope != o.scope;
        *self.inner.options.write() = o;
        let _ = self.inner.refresh_tx.send(Refresh { history: scope_changed, status_full: true, ..Default::default() });
    }

    pub fn refresh_all(&self) {
        let _ = self.inner.refresh_tx.send(Refresh::all());
    }

    pub fn refresh_status(&self) {
        let _ = self.inner.refresh_tx.send(Refresh { status_full: true, ..Default::default() });
    }

    pub fn meta(&self) -> &Arc<MetaCache> {
        &self.inner.meta
    }

    /// Row metadata for `ids`; misses load in the background and trigger a repaint.
    pub fn metas(&self, ids: &[ObjectId]) -> Vec<Option<Arc<history::CommitMeta>>> {
        let i = self.inner.clone();
        self.inner.meta.get_many(ids, move || (i.repaint)())
    }

    pub fn dismiss_messages(&self) {
        let mut s = self.inner.snap.write();
        s.messages.clear();
        s.generation += 1;
    }

    pub fn push_message(&self, kind: MessageKind, text: impl Into<String>) {
        push_message(&self.inner, kind, text.into());
    }

    /// Queues an operation. It runs after previously queued ones; a refresh
    /// of everything follows.
    pub fn run_op(
        &self,
        label: impl Into<String>,
        f: impl FnOnce(&Repo, &CancelToken, &mut (dyn FnMut(Line) + Send)) -> anyhow::Result<String> + Send + 'static,
    ) {
        let _ = self.inner.op_tx.send(OpJob { label: label.into(), run: Box::new(f), refresh: Refresh::all() });
    }

    /// Like [`run_op`](Self::run_op) for quick index-only operations; refreshes status only.
    pub fn run_index_op(&self, label: impl Into<String>, f: impl FnOnce(&Repo) -> anyhow::Result<()> + Send + 'static) {
        let _ = self.inner.op_tx.send(OpJob {
            label: label.into(),
            run: Box::new(move |r, _, _| f(r).map(|_| String::new())),
            refresh: Refresh { status_full: true, ..Default::default() },
        });
    }

    pub fn cancel_op(&self) {
        if let Some(op) = &self.inner.snap.read().op {
            op.cancel.cancel();
        }
    }

    /// Diff for `target`, computed in the background. `status_gen` invalidates
    /// worktree diffs when files change.
    pub fn diff(&self, target: &DiffTarget, status_gen: u64, ignore_ws: bool, context: u32) -> Load<FileDiff> {
        let key = (target.clone(), status_gen, ignore_ws, context);
        let repo = self.inner.repo.clone();
        let t = target.clone();
        load(&self.inner, &self.inner.diff, key, move || {
            let (old, new) = content::load_sides(&repo, &t)?;
            Ok(gk_diff::diff(
                old.as_deref().unwrap_or_default(),
                new.as_deref().unwrap_or_default(),
                gk_diff::Options { context, ignore_whitespace: ignore_ws },
            ))
        }, |s| &s.diff)
    }

    pub fn commit_details(&self, id: ObjectId) -> Load<CommitDetails> {
        let repo = self.inner.repo.clone();
        load(&self.inner, &self.inner.details, id, move || content::commit_details(&repo, id), |s| &s.details)
    }

    /// Rows of the current history matching `needle`.
    pub fn search(&self, needle: &str) -> Load<Vec<u32>> {
        let hist = self.inner.snap.read().history.clone();
        let hgen = self.inner.history_gen.load(Ordering::SeqCst);
        let repo = self.inner.repo.clone();
        let n = needle.to_string();
        load(&self.inner, &self.inner.search, (needle.to_string(), hgen), move || {
            Ok(history::search(&repo, &hist, &n, &AtomicBool::new(false)))
        }, |s| &s.search)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

fn load<K, V>(
    inner: &Arc<Shared>,
    slot: &Mutex<Loader<K, V>>,
    key: K,
    compute: impl FnOnce() -> anyhow::Result<V> + Send + 'static,
    select: fn(&Shared) -> &Mutex<Loader<K, V>>,
) -> Load<V>
where
    K: PartialEq + Clone + Send + 'static,
    V: Send + Sync + 'static,
{
    let mut l = slot.lock();
    if l.key.as_ref() == Some(&key) {
        if let Some(v) = &l.value {
            return v.clone();
        }
        return Load::Loading;
    }
    l.key = Some(key.clone());
    l.value = None;
    l.ticket += 1;
    let ticket = l.ticket;
    drop(l);
    let i = inner.clone();
    rayon::spawn(move || {
        let result = match compute() {
            Ok(v) => Load::Ready(Arc::new(v)),
            Err(e) => Load::Failed(format!("{e:#}")),
        };
        let mut l = select(&i).lock();
        if l.ticket == ticket {
            l.value = Some(result);
        }
        drop(l);
        (i.repaint)();
    });
    Load::Loading
}

fn push_message(inner: &Shared, kind: MessageKind, text: String) {
    let mut s = inner.snap.write();
    s.messages.push(Message { kind, text, at: Instant::now() });
    if s.messages.len() > 20 {
        s.messages.remove(0);
    }
    s.generation += 1;
    drop(s);
    (inner.repaint)();
}

fn event_loop(inner: Arc<Shared>, rx: Receiver<watch::Event>) {
    while let Ok(ev) = rx.recv() {
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let r = match ev {
            watch::Event::Worktree(paths) => Refresh { paths, ..Default::default() },
            watch::Event::Overflow => Refresh { status_full: true, ..Default::default() },
            watch::Event::GitDir(c) => Refresh {
                refs: c.refs || c.head || c.state,
                history: c.refs || c.head,
                status_full: c.index || c.head,
                paths: Vec::new(),
            },
        };
        // A newer full status supersedes one that is still running.
        if r.status_full {
            inner.status_interrupt.lock().store(true, Ordering::SeqCst);
        }
        let _ = inner.refresh_tx.send(r);
    }
}

fn refresh_loop(inner: Arc<Shared>, rx: Receiver<Refresh>) {
    let mut last_tips: Vec<String> = Vec::new();
    let mut last_scope: Option<Scope> = None;
    while let Ok(first) = rx.recv() {
        let mut req = first;
        // Coalesce whatever else is queued.
        std::thread::sleep(Duration::from_millis(5));
        while let Ok(more) = rx.try_recv() {
            req.merge(more);
        }
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        if req.is_empty() {
            continue;
        }
        let repo = &inner.repo;
        let opts = inner.options.read().clone();

        if req.refs || req.history {
            match refs::load(repo) {
                Ok(r) => {
                    let mut s = inner.snap.write();
                    s.refs = Arc::new(r);
                    s.generation += 1;
                }
                Err(e) => push_message(&inner, MessageKind::Error, format!("reading refs failed: {e:#}")),
            }
            (inner.repaint)();
        }

        if req.status_full || !req.paths.is_empty() {
            let interrupt = Arc::new(AtomicBool::new(false));
            *inner.status_interrupt.lock() = interrupt.clone();
            {
                let mut s = inner.snap.write();
                s.status_loading = true;
                s.generation += 1;
            }
            (inner.repaint)();
            let patterns: Vec<BString> = if req.status_full { Vec::new() } else { req.paths.clone() };
            match gk_status::compute(repo.workdir(), repo.git(), &patterns, &opts.status, &interrupt) {
                Ok(new) => {
                    let mut s = inner.snap.write();
                    let merged = if req.status_full {
                        new
                    } else {
                        let mut cur = (*s.status).clone();
                        cur.merge(new, &patterns, false);
                        cur
                    };
                    s.status = Arc::new(merged);
                    s.status_gen += 1;
                    s.status_loading = false;
                    s.generation += 1;
                }
                Err(e) => {
                    let mut s = inner.snap.write();
                    s.status_loading = false;
                    drop(s);
                    if !interrupt.load(Ordering::SeqCst) {
                        push_message(&inner, MessageKind::Error, format!("status failed: {e:#}"));
                    }
                }
            }
            (inner.repaint)();
        }

        if req.history {
            let r = inner.snap.read().refs.clone();
            let tips = r.tips();
            let scope_changed = last_scope.as_ref() != Some(&opts.scope);
            let head_changed = matches!(opts.scope, Scope::Current);
            if tips != last_tips || scope_changed || head_changed || inner.snap.read().history.is_empty() {
                {
                    let mut s = inner.snap.write();
                    s.history_loading = true;
                    s.generation += 1;
                }
                (inner.repaint)();
                let tip_ids = history::resolve_tips(repo, &opts.scope, &tips);
                match history::build(repo, &tip_ids, &AtomicBool::new(false)) {
                    Ok(h) => {
                        let mut s = inner.snap.write();
                        s.history = Arc::new(h);
                        s.history_loading = false;
                        s.generation += 1;
                        inner.history_gen.fetch_add(1, Ordering::SeqCst);
                        last_tips = tips;
                        last_scope = Some(opts.scope.clone());
                    }
                    Err(e) => {
                        inner.snap.write().history_loading = false;
                        push_message(&inner, MessageKind::Error, format!("reading history failed: {e:#}"));
                    }
                }
                (inner.repaint)();
            }
        }
    }
}

fn op_loop(inner: Arc<Shared>, rx: Receiver<OpJob>) {
    while let Ok(job) = rx.recv() {
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let cancel = CancelToken::new();
        {
            let mut s = inner.snap.write();
            s.op = Some(OpProgress {
                label: job.label.clone(),
                last_line: String::new(),
                started: Instant::now(),
                cancel: cancel.clone(),
            });
            s.generation += 1;
        }
        (inner.repaint)();
        let i2 = inner.clone();
        let mut on_line = move |l: Line| {
            let text = match l {
                Line::Stdout(s) | Line::Stderr(s) => s,
            };
            let mut s = i2.snap.write();
            if let Some(op) = &mut s.op {
                op.last_line = text;
            }
            s.generation += 1;
            drop(s);
            (i2.repaint)();
        };
        let result = (job.run)(&inner.repo, &cancel, &mut on_line);
        {
            let mut s = inner.snap.write();
            s.op = None;
            s.generation += 1;
        }
        match result {
            Ok(text) => {
                let t = text.trim();
                if t.to_lowercase().contains("conflict") {
                    push_message(&inner, MessageKind::Error, format!("{}: conflicts — resolve them, then continue.\n{t}", job.label));
                }
            }
            Err(e) => push_message(&inner, MessageKind::Error, format!("{} failed:\n{e:#}", job.label)),
        }
        let _ = inner.refresh_tx.send(job.refresh);
        (inner.repaint)();
    }
}

