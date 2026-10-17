//! A repository session: background refresh driven by the file watcher, a
//! serialized operation queue, and lazily computed diffs and commit details.
//! The UI only ever reads immutable snapshots.
//!
//! Threads per session:
//! - `gk-refresh`: refs and status, coalescing watcher events;
//! - `gk-history`: history walks and layouts, one at a time; a newer request
//!   interrupts the running build. It first publishes the cached snapshot
//!   (see [`crate::cache`]);
//! - `gk-ops`: the serialized write operations; the watcher is paused while
//!   one runs and a single explicit refresh follows;
//! - `gk-events` (with a watcher) and `gk-cache` (with the cache).
//!
//! Lazy loads run on [`crate::pool`]: row metadata, diffs and commit details
//! on the small foreground pool, searches on the background pool.

use crate::Repo;
use crate::cache::{self, RepoCache};
use crate::content::{self, CommitDetails, DiffTarget};
use crate::history::{self, History, MetaCache, Scope};
use crate::pool::{self, Priority};
use crate::refs::{self, Refs};
use bstr::BString;
use crossbeam_channel::{Receiver, Sender, unbounded};
use gix::ObjectId;
use gk_cli::{CancelToken, Line};
use gk_diff::FileDiff;
use gk_status::{Status, watch};
use parking_lot::{Mutex, RwLock};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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
    /// `history` holds only the newest rows ([`history::PARTIAL_ROWS`]) while
    /// the full walk continues.
    pub history_partial: bool,
    /// Some of refs, status or history still come from the on-disk cache of
    /// the previous session: shown at once, replaced as soon as the live
    /// values are loaded. Write operations are fine (they run on the real
    /// repository), but the data may be stale.
    pub from_cache: bool,
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
    /// Publish the cached snapshot on open and keep the cache up to date.
    pub cache: bool,
    /// Cache database; `None` = `<config dir>/cache.redb`.
    pub cache_file: Option<PathBuf>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            status: Default::default(),
            scope: Scope::All,
            watch: true,
            cache: true,
            cache_file: None,
        }
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
        Self {
            refs: true,
            history: true,
            status_full: true,
            paths: Vec::new(),
        }
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
        Self {
            key: None,
            value: None,
            ticket: 0,
        }
    }
}

type OpFn = Box<
    dyn FnOnce(&Repo, &CancelToken, &mut (dyn FnMut(Line) + Send)) -> anyhow::Result<String> + Send,
>;

struct OpJob {
    label: String,
    /// `None` stops the op thread.
    run: Option<OpFn>,
    refresh: Refresh,
    /// No status-bar entry, no error message, watcher left running.
    quiet: bool,
}

/// Parts that must be loaded live before `from_cache` clears.
const LIVE_REFS: u8 = 1;
const LIVE_STATUS: u8 = 2;
const LIVE_HISTORY: u8 = 4;
const LIVE_ALL: u8 = LIVE_REFS | LIVE_STATUS | LIVE_HISTORY;

/// After an own operation: late watcher events within this window are
/// dropped and the explicit refresh starts once it has passed.
const OP_GRACE: Duration = Duration::from_millis(40);

struct Shared {
    repo: Repo,
    snap: RwLock<Snapshot>,
    repaint: Box<dyn Fn() + Send + Sync>,
    refresh_tx: Sender<Refresh>,
    op_tx: Sender<OpJob>,
    history_tx: Sender<()>,
    cache_tx: Option<Sender<cache::Write>>,
    options: RwLock<SessionOptions>,
    meta: Arc<MetaCache>,
    diff: Mutex<Loader<(DiffTarget, u64, bool, u32), FileDiff>>,
    details: Mutex<Loader<ObjectId, CommitDetails>>,
    search: Mutex<Loader<(String, u64), Vec<u32>>>,
    search_interrupt: Mutex<Arc<AtomicBool>>,
    history_gen: AtomicU64,
    status_interrupt: Mutex<Arc<AtomicBool>>,
    history_interrupt: Mutex<Arc<AtomicBool>>,
    watcher: Mutex<Option<watch::Watcher>>,
    /// `LIVE_*` bits loaded from the repository (not the cache).
    live: AtomicU8,
    closed: AtomicBool,
}

/// Handle to an open repository; clone freely.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Shared>,
    _close: Arc<CloseGuard>,
}

/// Stops the worker threads when the last [`Session`] clone is dropped. The
/// threads own `Shared` (and its channel senders), so they would otherwise
/// never see their channels disconnect.
struct CloseGuard(Arc<Shared>);

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let i = &self.0;
        i.closed.store(true, Ordering::SeqCst);
        i.status_interrupt.lock().store(true, Ordering::SeqCst);
        i.history_interrupt.lock().store(true, Ordering::SeqCst);
        i.search_interrupt.lock().store(true, Ordering::SeqCst);
        // Stops the watcher thread, which ends the event loop.
        i.watcher.lock().take();
        if let Some(tx) = &i.cache_tx {
            let _ = tx.send(cache::Write::Flush);
        }
        // Wake the loops so they notice `closed`.
        let _ = i.refresh_tx.send(Refresh::default());
        let _ = i.history_tx.send(());
        let _ = i.op_tx.send(OpJob {
            label: String::new(),
            run: None,
            refresh: Refresh::default(),
            quiet: true,
        });
    }
}

impl Session {
    pub fn open(
        repo: Repo,
        options: SessionOptions,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (refresh_tx, refresh_rx) = unbounded();
        let (op_tx, op_rx) = unbounded();
        let (history_tx, history_rx) = unbounded();
        let meta = Arc::new(MetaCache::new(repo.clone()));
        let repo_cache = options
            .cache
            .then(|| RepoCache::open(options.cache_file.as_deref(), repo.workdir()))
            .flatten();
        let inner = Arc::new(Shared {
            repo: repo.clone(),
            snap: RwLock::new(Snapshot::default()),
            repaint: Box::new(repaint),
            refresh_tx,
            op_tx,
            history_tx,
            cache_tx: repo_cache.clone().map(cache::spawn_writer),
            options: RwLock::new(options.clone()),
            meta,
            diff: Mutex::new(Loader::default()),
            details: Mutex::new(Loader::default()),
            search: Mutex::new(Loader::default()),
            search_interrupt: Mutex::new(Arc::new(AtomicBool::new(false))),
            history_gen: AtomicU64::new(0),
            status_interrupt: Mutex::new(Arc::new(AtomicBool::new(false))),
            history_interrupt: Mutex::new(Arc::new(AtomicBool::new(false))),
            watcher: Mutex::new(None),
            live: AtomicU8::new(0),
            closed: AtomicBool::new(false),
        });
        {
            let i = inner.clone();
            std::thread::Builder::new()
                .name("gk-history".into())
                .spawn(move || history_loop(i, history_rx, repo_cache))
                .expect("spawn history thread");
        }
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
        if options.watch {
            let (ev_tx, ev_rx) = unbounded();
            match watch::watch(repo.workdir(), repo.git_dir(), ev_tx) {
                Ok(w) => {
                    *inner.watcher.lock() = Some(w);
                    let i = inner.clone();
                    std::thread::Builder::new()
                        .name("gk-events".into())
                        .stack_size(256 << 10)
                        .spawn(move || event_loop(i, ev_rx))
                        .expect("spawn event thread");
                }
                Err(e) => push_message(
                    &inner,
                    MessageKind::Error,
                    format!("file watcher unavailable: {e}"),
                ),
            }
        }
        let s = Self {
            _close: Arc::new(CloseGuard(inner.clone())),
            inner,
        };
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
        let _ = self.inner.refresh_tx.send(Refresh {
            history: scope_changed,
            status_full: true,
            ..Default::default()
        });
    }

    pub fn refresh_all(&self) {
        let _ = self.inner.refresh_tx.send(Refresh::all());
    }

    pub fn refresh_status(&self) {
        let _ = self.inner.refresh_tx.send(Refresh {
            status_full: true,
            ..Default::default()
        });
    }

    pub fn meta(&self) -> &Arc<MetaCache> {
        &self.inner.meta
    }

    /// Row metadata for `ids`; misses load in the background and trigger a repaint.
    /// Runs on the foreground pool: never queued behind searches.
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
    /// of everything follows. The file watcher is paused while it runs.
    pub fn run_op(
        &self,
        label: impl Into<String>,
        f: impl FnOnce(&Repo, &CancelToken, &mut (dyn FnMut(Line) + Send)) -> anyhow::Result<String>
        + Send
        + 'static,
    ) {
        let _ = self.inner.op_tx.send(OpJob {
            label: label.into(),
            run: Some(Box::new(f)),
            refresh: Refresh::all(),
            quiet: false,
        });
    }

    /// Queues a background operation (auto-fetch): serialized with the other
    /// operations, but without a status-bar entry, and failures are not shown
    /// as messages (they still appear in the command log). The watcher keeps
    /// running, since such operations can take long; refs and history are
    /// refreshed after a success.
    pub fn run_quiet_op(
        &self,
        label: impl Into<String>,
        f: impl FnOnce(&Repo, &CancelToken, &mut (dyn FnMut(Line) + Send)) -> anyhow::Result<String>
        + Send
        + 'static,
    ) {
        let _ = self.inner.op_tx.send(OpJob {
            label: label.into(),
            run: Some(Box::new(f)),
            refresh: Refresh {
                refs: true,
                history: true,
                ..Default::default()
            },
            quiet: true,
        });
    }

    /// Like [`run_op`](Self::run_op) for quick index-only operations; refreshes status only.
    pub fn run_index_op(
        &self,
        label: impl Into<String>,
        f: impl FnOnce(&Repo) -> anyhow::Result<()> + Send + 'static,
    ) {
        let _ = self.inner.op_tx.send(OpJob {
            label: label.into(),
            run: Some(Box::new(move |r, _, _| f(r).map(|_| String::new()))),
            refresh: Refresh {
                status_full: true,
                ..Default::default()
            },
            quiet: false,
        });
    }

    pub fn cancel_op(&self) {
        if let Some(op) = &self.inner.snap.read().op {
            op.cancel.cancel();
        }
    }

    /// Diff for `target`, computed in the background. `status_gen` invalidates
    /// worktree diffs when files change.
    pub fn diff(
        &self,
        target: &DiffTarget,
        status_gen: u64,
        ignore_ws: bool,
        context: u32,
    ) -> Load<FileDiff> {
        let key = (target.clone(), status_gen, ignore_ws, context);
        let repo = self.inner.repo.clone();
        let t = target.clone();
        load(
            &self.inner,
            &self.inner.diff,
            key,
            Priority::Visible,
            move || {
                let _span = tracing::info_span!("diff").entered();
                let (old, new) = content::load_sides(&repo, &t)?;
                Ok(gk_diff::diff(
                    old.as_deref().unwrap_or_default(),
                    new.as_deref().unwrap_or_default(),
                    gk_diff::Options {
                        context,
                        ignore_whitespace: ignore_ws,
                    },
                ))
            },
            |s| &s.diff,
        )
    }

    pub fn commit_details(&self, id: ObjectId) -> Load<CommitDetails> {
        let repo = self.inner.repo.clone();
        load(
            &self.inner,
            &self.inner.details,
            id,
            Priority::Visible,
            move || {
                let _span = tracing::info_span!("commit_details").entered();
                content::commit_details(&repo, id)
            },
            |s| &s.details,
        )
    }

    /// Rows of the current history matching `needle`. Runs on the background
    /// pool; a newer needle interrupts the previous search.
    pub fn search(&self, needle: &str) -> Load<Vec<u32>> {
        let hist = self.inner.snap.read().history.clone();
        let hgen = self.inner.history_gen.load(Ordering::SeqCst);
        let key = (needle.to_string(), hgen);
        {
            let l = self.inner.search.lock();
            if l.key.as_ref() != Some(&key) {
                let fresh = Arc::new(AtomicBool::new(false));
                let old = std::mem::replace(&mut *self.inner.search_interrupt.lock(), fresh);
                old.store(true, Ordering::SeqCst);
            }
        }
        let interrupt = self.inner.search_interrupt.lock().clone();
        let repo = self.inner.repo.clone();
        let n = needle.to_string();
        load(
            &self.inner,
            &self.inner.search,
            key,
            Priority::Background,
            move || Ok(history::search(&repo, &hist, &n, &interrupt)),
            |s| &s.search,
        )
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
    priority: Priority,
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
    pool::spawn(priority, move || {
        // A newer request already replaced this one: skip the work.
        if select(&i).lock().ticket != ticket {
            return;
        }
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
    s.messages.push(Message {
        kind,
        text,
        at: Instant::now(),
    });
    if s.messages.len() > 20 {
        s.messages.remove(0);
    }
    s.generation += 1;
    drop(s);
    (inner.repaint)();
}

/// Records that `bits` were loaded from the repository; clears
/// `from_cache` once refs, status and history all were. Call with the
/// snapshot write lock held (`s`), so cache publication cannot interleave.
fn mark_live(inner: &Shared, s: &mut Snapshot, bits: u8) {
    let live = inner.live.fetch_or(bits, Ordering::SeqCst) | bits;
    if live == LIVE_ALL && s.from_cache {
        s.from_cache = false;
        s.generation += 1;
    }
}

fn event_loop(inner: Arc<Shared>, rx: Receiver<watch::Event>) {
    while let Ok(ev) = rx.recv() {
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let r = match ev {
            watch::Event::Worktree(paths) => Refresh {
                paths,
                ..Default::default()
            },
            watch::Event::Overflow => Refresh {
                status_full: true,
                ..Default::default()
            },
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

/// Asks the history thread for a (re)build, interrupting the running one.
fn request_history(inner: &Shared) {
    inner.history_interrupt.lock().store(true, Ordering::SeqCst);
    let _ = inner.history_tx.send(());
}

fn refresh_loop(inner: Arc<Shared>, rx: Receiver<Refresh>) {
    let mut fast: Option<gk_status::fast::FastStatus> = None;
    while let Ok(first) = rx.recv() {
        let mut req = first;
        // Coalesce whatever else is queued.
        std::thread::sleep(Duration::from_millis(2));
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

        // The history walk reads tips in-process and runs on its own thread,
        // in parallel with refs and status.
        if req.history {
            request_history(&inner);
        }

        let mut refs_changed = false;
        if req.refs || req.history {
            match refs::load(repo) {
                Ok(r) => {
                    let mut s = inner.snap.write();
                    s.refs = Arc::new(r);
                    s.generation += 1;
                    mark_live(&inner, &mut s, LIVE_REFS);
                    refs_changed = true;
                }
                Err(e) => push_message(
                    &inner,
                    MessageKind::Error,
                    format!("reading refs failed: {e:#}"),
                ),
            }
            (inner.repaint)();
        }

        let mut status_changed = false;
        if req.status_full || !req.paths.is_empty() {
            let interrupt = Arc::new(AtomicBool::new(false));
            *inner.status_interrupt.lock() = interrupt.clone();
            {
                let mut s = inner.snap.write();
                s.status_loading = true;
                s.generation += 1;
            }
            (inner.repaint)();
            let patterns: Vec<BString> = if req.status_full {
                Vec::new()
            } else {
                req.paths.clone()
            };
            // Watcher-reported paths: the in-memory fast path avoids re-reading
            // the whole index; it declines anything unusual.
            let quick = if req.status_full {
                None
            } else {
                let _span = tracing::info_span!("status.fast", paths = patterns.len()).entered();
                let start = Instant::now();
                if fast.is_none() {
                    fast = gk_status::fast::FastStatus::new(repo.workdir()).ok();
                }
                match fast
                    .as_mut()
                    .map(|f| f.refresh(&patterns, opts.status.untracked))
                {
                    Some(Ok(gk_status::fast::Outcome::Updated(unstaged))) => {
                        Some(gk_status::Status {
                            staged: Vec::new(),
                            unstaged,
                            took: start.elapsed(),
                        })
                    }
                    _ => None,
                }
            };
            let computed = match quick {
                Some(s) => Ok(s),
                None => gk_status::compute(
                    repo.workdir(),
                    repo.git(),
                    &patterns,
                    &opts.status,
                    &interrupt,
                ),
            };
            match computed {
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
                    // A path refresh on top of a cached status is not a live status.
                    if req.status_full {
                        mark_live(&inner, &mut s, LIVE_STATUS);
                    }
                    status_changed = true;
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

        if (refs_changed || status_changed)
            && let Some(tx) = &inner.cache_tx
        {
            let live = inner.live.load(Ordering::SeqCst);
            if live & (LIVE_REFS | LIVE_STATUS) == LIVE_REFS | LIVE_STATUS {
                let s = inner.snap.read();
                let _ = tx.send(cache::Write::RefsStatus(s.refs.clone(), s.status.clone()));
            }
        }
    }
}

/// Publishes what the cache holds, unless live values arrived first.
/// Returns the tips and scope of the cached history when it can stand in
/// for a rebuild with the same tips.
fn publish_cached(inner: &Shared, c: cache::Cached) -> Option<(Vec<ObjectId>, Scope)> {
    let scope = inner.options.read().scope.clone();
    let mut s = inner.snap.write();
    // Read under the lock: live data is marked while holding it.
    let live = inner.live.load(Ordering::SeqCst);
    let mut any = false;
    if live & LIVE_REFS == 0
        && let Some(r) = c.refs
    {
        s.refs = Arc::new(r);
        any = true;
    }
    if live & LIVE_STATUS == 0
        && let Some(st) = c.status
    {
        s.status = Arc::new(st);
        s.status_gen += 1;
        any = true;
    }
    let mut valid = None;
    if live & LIVE_HISTORY == 0
        && let Some(h) = c.history
        && h.scope == scope
    {
        s.history = Arc::new(h.history);
        inner.history_gen.fetch_add(1, Ordering::SeqCst);
        any = true;
        // Shallow repositories can gain history without new tips (`fetch
        // --deepen`): always rebuild those.
        if !inner.repo.git_dir().join("shallow").exists() {
            valid = Some((h.tips, h.scope));
        }
    }
    if any {
        s.from_cache = true;
        s.generation += 1;
    }
    drop(s);
    if any {
        (inner.repaint)();
    }
    valid
}

fn history_loop(inner: Arc<Shared>, rx: Receiver<()>, cache: Option<RepoCache>) {
    // What the published history was built from.
    let mut last: Option<(Vec<ObjectId>, Scope)> = None;
    if let Some(c) = &cache {
        let cached = c.load();
        last = publish_cached(&inner, cached);
    }
    while rx.recv().is_ok() {
        while rx.try_recv().is_ok() {}
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let interrupt = Arc::new(AtomicBool::new(false));
        *inner.history_interrupt.lock() = interrupt.clone();
        let scope = inner.options.read().scope.clone();
        let tips = history::tip_ids(&inner.repo, &scope);
        if last
            .as_ref()
            .is_some_and(|(t, sc)| *t == tips && *sc == scope)
        {
            // Same tips, same graph: the published history stays.
            let mut s = inner.snap.write();
            if s.history_loading || s.history_partial {
                s.history_loading = false;
                s.history_partial = false;
                s.generation += 1;
            }
            mark_live(&inner, &mut s, LIVE_HISTORY);
            drop(s);
            (inner.repaint)();
            continue;
        }
        {
            let mut s = inner.snap.write();
            s.history_loading = true;
            s.generation += 1;
        }
        (inner.repaint)();
        let i2 = inner.clone();
        let int2 = interrupt.clone();
        let mut on_partial = move |h: History| {
            if int2.load(Ordering::SeqCst) {
                return;
            }
            let mut s = i2.snap.write();
            s.history = Arc::new(h);
            s.history_partial = true;
            s.generation += 1;
            i2.history_gen.fetch_add(1, Ordering::SeqCst);
            drop(s);
            (i2.repaint)();
        };
        match history::build_with_partial(
            &inner.repo,
            &tips,
            &interrupt,
            history::PARTIAL_ROWS,
            &mut on_partial,
        ) {
            Ok(h) => {
                let h = Arc::new(h);
                let mut s = inner.snap.write();
                s.history = h.clone();
                s.history_loading = false;
                s.history_partial = false;
                s.generation += 1;
                inner.history_gen.fetch_add(1, Ordering::SeqCst);
                mark_live(&inner, &mut s, LIVE_HISTORY);
                drop(s);
                if let Some(tx) = &inner.cache_tx {
                    let _ = tx.send(cache::Write::History(scope.clone(), tips.clone(), h));
                }
                last = Some((tips, scope));
            }
            // Superseded by a newer request (which follows in the channel) or closing.
            Err(_) if interrupt.load(Ordering::SeqCst) => {}
            Err(e) => {
                {
                    let mut s = inner.snap.write();
                    s.history_loading = false;
                    s.history_partial = false;
                    s.generation += 1;
                }
                push_message(
                    &inner,
                    MessageKind::Error,
                    format!("reading history failed: {e:#}"),
                );
            }
        }
        (inner.repaint)();
    }
}

fn op_loop(inner: Arc<Shared>, rx: Receiver<OpJob>) {
    while let Ok(job) = rx.recv() {
        // Jobs queued before closing still run; the sentinel ends the loop.
        let Some(run) = job.run else {
            return;
        };
        let _span = tracing::info_span!("op", label = %job.label).entered();
        let cancel = CancelToken::new();
        if !job.quiet {
            // Our own writes would otherwise trigger refreshes while the op
            // runs, on top of the explicit refresh after it.
            if let Some(w) = inner.watcher.lock().as_ref() {
                w.pause();
            }
            let mut s = inner.snap.write();
            s.op = Some(OpProgress {
                label: job.label.clone(),
                last_line: String::new(),
                started: Instant::now(),
                cancel: cancel.clone(),
            });
            s.generation += 1;
            drop(s);
            (inner.repaint)();
        }
        let i2 = inner.clone();
        let quiet = job.quiet;
        let mut on_line = move |l: Line| {
            if quiet {
                return;
            }
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
        let result = run(&inner.repo, &cancel, &mut on_line);
        if job.quiet {
            if result.is_ok() {
                let _ = inner.refresh_tx.send(job.refresh);
            }
            continue;
        }
        {
            let mut s = inner.snap.write();
            s.op = None;
            s.generation += 1;
        }
        match result {
            Ok(text) => {
                let t = text.trim();
                if crate::ops::stopped_on_conflict(t) {
                    push_message(
                        &inner,
                        MessageKind::Error,
                        format!(
                            "{}: conflicts — resolve them, then continue.\n{t}",
                            job.label
                        ),
                    );
                }
            }
            Err(e) => push_message(
                &inner,
                MessageKind::Error,
                format!("{} failed:\n{e:#}", job.label),
            ),
        }
        let resumed = match inner.watcher.lock().as_ref() {
            Some(w) => {
                w.resume_discarding(OP_GRACE);
                true
            }
            None => false,
        };
        if resumed {
            // Late notifications for the op's writes are dropped until then;
            // refreshing afterwards guarantees the refresh sees those changes.
            std::thread::sleep(OP_GRACE);
        }
        if job.refresh.status_full {
            inner.status_interrupt.lock().store(true, Ordering::SeqCst);
        }
        let _ = inner.refresh_tx.send(job.refresh);
        (inner.repaint)();
    }
}

#[cfg(test)]
mod tests;
