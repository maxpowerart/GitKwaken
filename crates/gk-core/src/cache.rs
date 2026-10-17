//! On-disk snapshot cache for cold starts.
//!
//! One embedded [`redb`] database, `<config dir>/cache.redb`
//! (`GITKWAKEN_CONFIG_DIR` respected), shared by every session of the
//! process. Per repository (key: canonical working directory) it keeps the
//! last refs, status and history (ids, parent rows, tips and scope), so a
//! reopened repository shows its last state at once while the session
//! validates it in the background.
//!
//! The values use a small versioned binary encoding; anything that fails to
//! decode is treated as a miss. At most [`MAX_REPOS`] repositories are kept
//! (least recently written ones are dropped). When another process holds the
//! database (a second GitKwaken instance) caching is silently off.

use crate::history::{History, Scope};
use crate::refs::{Branch, Head, Refs, Remote, RemoteBranch, RepoState, Stash, Tag};
use anyhow::{Context, Result};
use gix::ObjectId;
use gk_status::{Conflict, Kind, Status, StatusBuilder};
use parking_lot::Mutex;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// Bump when the encoding (or the history algorithm) changes.
const VERSION: u8 = 1;
/// Repositories kept in the cache.
pub const MAX_REPOS: usize = 32;
/// Statuses with more entries are not cached (decoding would cost more than
/// the status itself).
const MAX_STATUS_ENTRIES: usize = 200_000;
/// Histories with more commits are not cached.
const MAX_HISTORY_ROWS: usize = 4_000_000;
/// redb page cache; the values are read once per open.
const DB_CACHE_BYTES: usize = 4 << 20;

const SNAP: TableDefinition<&str, &[u8]> = TableDefinition::new("snapshot");
const USED: TableDefinition<&str, u64> = TableDefinition::new("last_used");

const KIND_REFS: &str = "refs";
const KIND_STATUS: &str = "status";
const KIND_HISTORY: &str = "history";

/// Default database location.
pub fn default_path() -> PathBuf {
    gk_settings::Settings::config_dir().join("cache.redb")
}

/// A cached history plus what it was built from.
pub struct CachedHistory {
    pub scope: Scope,
    pub tips: Vec<ObjectId>,
    pub history: History,
}

/// Whatever the cache had for one repository.
#[derive(Default)]
pub struct Cached {
    pub refs: Option<Refs>,
    pub status: Option<Status>,
    pub history: Option<CachedHistory>,
}

/// The cache entry of one repository.
#[derive(Clone)]
pub struct RepoCache {
    db: Arc<Database>,
    key: String,
}

fn databases() -> &'static Mutex<Vec<(PathBuf, Weak<Database>)>> {
    static DBS: Mutex<Vec<(PathBuf, Weak<Database>)>> = Mutex::new(Vec::new());
    &DBS
}

fn open_db(path: &Path) -> Result<Arc<Database>> {
    let mut dbs = databases().lock();
    dbs.retain(|(_, w)| w.strong_count() > 0);
    if let Some(db) = dbs
        .iter()
        .find(|(p, _)| p == path)
        .and_then(|(_, w)| w.upgrade())
    {
        return Ok(db);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let db = Arc::new(
        Database::builder()
            .set_cache_size(DB_CACHE_BYTES)
            .create(path)
            .with_context(|| format!("opening {}", path.display()))?,
    );
    dbs.push((path.to_path_buf(), Arc::downgrade(&db)));
    Ok(db)
}

/// Cache key: the canonical working directory (case-folded on Windows).
pub fn key_for(workdir: &Path) -> String {
    let p = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

impl RepoCache {
    /// Opens the cache entry of `workdir` in the database at `file`
    /// (default: [`default_path`]). With the default location, repositories
    /// in the system temp directory are not cached unless
    /// `GITKWAKEN_CONFIG_DIR` is set: test suites open thousands of throwaway
    /// repositories there. `GITKWAKEN_NO_CACHE=1` disables caching.
    pub fn open(file: Option<&Path>, workdir: &Path) -> Option<RepoCache> {
        if std::env::var_os("GITKWAKEN_NO_CACHE").is_some_and(|v| v == "1") {
            return None;
        }
        let key = key_for(workdir);
        let path = match file {
            Some(f) => f.to_path_buf(),
            None => {
                if std::env::var_os(gk_settings::CONFIG_DIR_ENV).is_none() {
                    let tmp = key_for(&std::env::temp_dir());
                    if key.starts_with(&tmp) {
                        return None;
                    }
                }
                default_path()
            }
        };
        let db = open_db(&path).ok()?;
        Some(RepoCache { db, key })
    }

    fn k(&self, kind: &str) -> String {
        format!("{}\n{kind}", self.key)
    }

    /// Reads everything cached for this repository. Missing or undecodable
    /// parts are `None`.
    pub fn load(&self) -> Cached {
        let _span = tracing::info_span!("cache.load").entered();
        let mut out = Cached::default();
        let Ok(tx) = self.db.begin_read() else {
            return out;
        };
        let Ok(t) = tx.open_table(SNAP) else {
            return out;
        };
        let get = |kind: &str| -> Option<Vec<u8>> {
            t.get(self.k(kind).as_str())
                .ok()
                .flatten()
                .map(|v| v.value().to_vec())
        };
        out.refs = get(KIND_REFS).and_then(|b| decode_refs(&b));
        out.status = get(KIND_STATUS).and_then(|b| decode_status(&b));
        out.history = get(KIND_HISTORY).and_then(|b| decode_history(&b));
        out
    }

    /// Stores whichever parts are given in one transaction.
    pub fn store(
        &self,
        refs: Option<&Refs>,
        status: Option<&Status>,
        history: Option<(&Scope, &[ObjectId], &History)>,
    ) -> Result<()> {
        let _span = tracing::info_span!("cache.store").entered();
        let mut values: Vec<(&str, Vec<u8>)> = Vec::new();
        if let Some(r) = refs {
            values.push((KIND_REFS, encode_refs(r)));
        }
        if let Some(s) = status
            && s.staged.len() + s.unstaged.len() <= MAX_STATUS_ENTRIES
        {
            values.push((KIND_STATUS, encode_status(s)));
        }
        if let Some((scope, tips, h)) = history
            && h.len() <= MAX_HISTORY_ROWS
        {
            values.push((KIND_HISTORY, encode_history(scope, tips, h)));
        }
        if values.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(SNAP)?;
            for (kind, v) in &values {
                t.insert(self.k(kind).as_str(), v.as_slice())?;
            }
            let mut used = tx.open_table(USED)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            used.insert(self.key.as_str(), now)?;
            if used.len()? as usize > MAX_REPOS {
                let mut all: Vec<(u64, String)> = used
                    .iter()?
                    .flatten()
                    .map(|(k, v)| (v.value(), k.value().to_string()))
                    .collect();
                all.sort();
                let excess = all.len() - MAX_REPOS;
                for (_, k) in all.into_iter().filter(|(_, k)| *k != self.key).take(excess) {
                    used.remove(k.as_str())?;
                    for kind in [KIND_REFS, KIND_STATUS, KIND_HISTORY] {
                        t.remove(format!("{k}\n{kind}").as_str())?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes this repository's entries.
    pub fn clear(&self) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(SNAP)?;
            for kind in [KIND_REFS, KIND_STATUS, KIND_HISTORY] {
                t.remove(self.k(kind).as_str())?;
            }
            tx.open_table(USED)?.remove(self.key.as_str())?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// What a session hands the background writer.
pub(crate) enum Write {
    RefsStatus(Arc<Refs>, Arc<Status>),
    History(Scope, Vec<ObjectId>, Arc<History>),
    /// Write what is pending now (session closing).
    Flush,
}

/// Minimum time between two writes of the same session: status changes on
/// every save, the cache only needs a recent state.
const MIN_INTERVAL: Duration = Duration::from_secs(3);

/// Spawns the writer thread of one session. It coalesces writes (latest
/// wins), writes at most every [`MIN_INTERVAL`] and flushes on
/// [`Write::Flush`] or when the sender is dropped.
pub(crate) fn spawn_writer(cache: RepoCache) -> crossbeam_channel::Sender<Write> {
    let (tx, rx) = crossbeam_channel::unbounded::<Write>();
    let _ = std::thread::Builder::new()
        .name("gk-cache".into())
        .stack_size(256 << 10)
        .spawn(move || {
            let mut refs_status: Option<(Arc<Refs>, Arc<Status>)> = None;
            let mut history: Option<(Scope, Vec<ObjectId>, Arc<History>)> = None;
            let mut last_write: Option<Instant> = None;
            loop {
                let pending = refs_status.is_some() || history.is_some();
                let msg = if pending {
                    let wait = last_write
                        .map(|t| MIN_INTERVAL.saturating_sub(t.elapsed()))
                        .unwrap_or_default();
                    match rx.recv_timeout(wait) {
                        Ok(m) => Some(m),
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            write(&cache, &mut refs_status, &mut history);
                            return;
                        }
                    }
                } else {
                    match rx.recv() {
                        Ok(m) => Some(m),
                        Err(_) => return,
                    }
                };
                let flush = match msg {
                    Some(Write::RefsStatus(r, s)) => {
                        refs_status = Some((r, s));
                        false
                    }
                    Some(Write::History(scope, tips, h)) => {
                        history = Some((scope, tips, h));
                        false
                    }
                    Some(Write::Flush) | None => true,
                };
                let due = last_write.is_none_or(|t| t.elapsed() >= MIN_INTERVAL);
                if (flush || due) && (refs_status.is_some() || history.is_some()) {
                    write(&cache, &mut refs_status, &mut history);
                    last_write = Some(Instant::now());
                }
            }
        });
    tx
}

fn write(
    cache: &RepoCache,
    refs_status: &mut Option<(Arc<Refs>, Arc<Status>)>,
    history: &mut Option<(Scope, Vec<ObjectId>, Arc<History>)>,
) {
    let rs = refs_status.take();
    let h = history.take();
    let _ = cache.store(
        rs.as_ref().map(|(r, _)| &**r),
        rs.as_ref().map(|(_, s)| &**s),
        h.as_ref().map(|(s, t, h)| (s, t.as_slice(), &**h)),
    );
}

// ---------------------------------------------------------------- encoding

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn new() -> Self {
        W(vec![VERSION])
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn bool(&mut self, b: bool) {
        self.u8(b as u8);
    }
}

struct R<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Option<Self> {
        (b.first() == Some(&VERSION)).then_some(R { b, pos: 1 })
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn len(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        // Every element takes at least a byte: rejects corrupt huge counts.
        (n <= self.b.len() - self.pos).then_some(n)
    }
    fn done(&self) -> bool {
        self.pos == self.b.len()
    }
}

fn encode_refs(r: &Refs) -> Vec<u8> {
    let mut w = W::new();
    match &r.head {
        Head::Branch(s) => {
            w.u8(0);
            w.str(s)
        }
        Head::Detached(s) => {
            w.u8(1);
            w.str(s)
        }
        Head::Unborn(s) => {
            w.u8(2);
            w.str(s)
        }
        Head::Unknown => w.u8(3),
    }
    w.opt_str(r.head_id.as_deref());
    w.u8(match r.state {
        RepoState::Clean => 0,
        RepoState::Merging => 1,
        RepoState::Rebasing { interactive: false } => 2,
        RepoState::Rebasing { interactive: true } => 3,
        RepoState::CherryPicking => 4,
        RepoState::Reverting => 5,
        RepoState::Bisecting => 6,
        RepoState::ApplyingMailbox => 7,
    });
    w.u32(r.branches.len() as u32);
    for b in &r.branches {
        w.str(&b.name);
        w.str(&b.target);
        w.opt_str(b.upstream.as_deref());
        w.u32(b.ahead);
        w.u32(b.behind);
        w.bool(b.upstream_gone);
        w.bool(b.is_head);
    }
    w.u32(r.remote_branches.len() as u32);
    for b in &r.remote_branches {
        w.str(&b.remote);
        w.str(&b.name);
        w.str(&b.target);
    }
    w.u32(r.tags.len() as u32);
    for t in &r.tags {
        w.str(&t.name);
        w.str(&t.target);
        w.bool(t.annotated);
    }
    w.u32(r.stashes.len() as u32);
    for s in &r.stashes {
        w.u32(s.index as u32);
        w.str(&s.id);
        w.str(&s.message);
    }
    w.u32(r.remotes.len() as u32);
    for x in &r.remotes {
        w.str(&x.name);
        w.str(&x.fetch_url);
        w.str(&x.push_url);
    }
    w.0
}

fn decode_refs(b: &[u8]) -> Option<Refs> {
    let mut r = R::new(b)?;
    let mut refs = Refs {
        head: match r.u8()? {
            0 => Head::Branch(r.str()?),
            1 => Head::Detached(r.str()?),
            2 => Head::Unborn(r.str()?),
            3 => Head::Unknown,
            _ => return None,
        },
        head_id: r.opt_str()?,
        state: match r.u8()? {
            0 => RepoState::Clean,
            1 => RepoState::Merging,
            2 => RepoState::Rebasing { interactive: false },
            3 => RepoState::Rebasing { interactive: true },
            4 => RepoState::CherryPicking,
            5 => RepoState::Reverting,
            6 => RepoState::Bisecting,
            7 => RepoState::ApplyingMailbox,
            _ => return None,
        },
        ..Default::default()
    };
    for _ in 0..r.len()? {
        refs.branches.push(Branch {
            name: r.str()?,
            target: r.str()?,
            upstream: r.opt_str()?,
            ahead: r.u32()?,
            behind: r.u32()?,
            upstream_gone: r.bool()?,
            is_head: r.bool()?,
        });
    }
    for _ in 0..r.len()? {
        refs.remote_branches.push(RemoteBranch {
            remote: r.str()?,
            name: r.str()?,
            target: r.str()?,
        });
    }
    for _ in 0..r.len()? {
        refs.tags.push(Tag {
            name: r.str()?,
            target: r.str()?,
            annotated: r.bool()?,
        });
    }
    for _ in 0..r.len()? {
        refs.stashes.push(Stash {
            index: r.u32()? as usize,
            id: r.str()?,
            message: r.str()?,
        });
    }
    for _ in 0..r.len()? {
        refs.remotes.push(Remote {
            name: r.str()?,
            fetch_url: r.str()?,
            push_url: r.str()?,
        });
    }
    if !r.done() {
        return None;
    }
    refs.labels = crate::refs::labels_of(&refs);
    Some(refs)
}

const CONFLICTS: [Conflict; 7] = [
    Conflict::BothDeleted,
    Conflict::AddedByUs,
    Conflict::DeletedByThem,
    Conflict::AddedByThem,
    Conflict::DeletedByUs,
    Conflict::BothAdded,
    Conflict::BothModified,
];

fn kind_code(k: Kind) -> u8 {
    match k {
        Kind::Added => 0,
        Kind::Modified => 1,
        Kind::Deleted => 2,
        Kind::Renamed => 3,
        Kind::Copied => 4,
        Kind::TypeChange => 5,
        Kind::Untracked => 6,
        Kind::IntentToAdd => 7,
        Kind::Conflict(c) => 8 + CONFLICTS.iter().position(|x| *x == c).unwrap_or(6) as u8,
    }
}

fn kind_of(c: u8) -> Option<Kind> {
    Some(match c {
        0 => Kind::Added,
        1 => Kind::Modified,
        2 => Kind::Deleted,
        3 => Kind::Renamed,
        4 => Kind::Copied,
        5 => Kind::TypeChange,
        6 => Kind::Untracked,
        7 => Kind::IntentToAdd,
        8..=14 => Kind::Conflict(CONFLICTS[(c - 8) as usize]),
        _ => return None,
    })
}

fn encode_status(s: &Status) -> Vec<u8> {
    let mut w = W::new();
    for side in [&s.staged, &s.unstaged] {
        w.u32(side.len() as u32);
        for e in side {
            w.bytes(&e.path);
            w.u8(kind_code(e.kind));
            match &e.orig_path {
                Some(p) => {
                    w.u8(1);
                    w.bytes(p);
                }
                None => w.u8(0),
            }
        }
    }
    w.0
}

fn decode_status(b: &[u8]) -> Option<Status> {
    let mut r = R::new(b)?;
    // All paths into one shared buffer (see `gk_status::StatusBuilder`).
    let mut out = StatusBuilder::new();
    for staged in [true, false] {
        let n = r.len()?;
        for _ in 0..n {
            let path = r.bytes()?;
            let kind = kind_of(r.u8()?)?;
            let orig = match r.u8()? {
                0 => None,
                1 => Some(r.bytes()?),
                _ => return None,
            };
            out.push_parts(staged, &[path], kind, orig);
        }
    }
    if !r.done() {
        return None;
    }
    Some(out.finish())
}

fn encode_history(scope: &Scope, tips: &[ObjectId], h: &History) -> Vec<u8> {
    let hash_len = h
        .ids
        .first()
        .or(tips.first())
        .map(|id| id.as_slice().len())
        .unwrap_or(20);
    let parents: usize = (0..h.len()).map(|r| h.layout.parents_of(r).len()).sum();
    let mut w = W(Vec::with_capacity(
        64 + (tips.len() + h.len()) * hash_len + h.len() * 2 + parents * 4,
    ));
    w.u8(VERSION);
    match scope {
        Scope::All => w.u8(0),
        Scope::Current => w.u8(1),
        Scope::Local => w.u8(3),
        Scope::Revs(revs) => {
            w.u8(2);
            w.u32(revs.len() as u32);
            for r in revs {
                w.str(r);
            }
        }
    }
    w.u8(hash_len as u8);
    w.u32(tips.len() as u32);
    for t in tips {
        w.0.extend_from_slice(t.as_slice());
    }
    w.u32(h.len() as u32);
    for id in &h.ids {
        w.0.extend_from_slice(id.as_slice());
    }
    for r in 0..h.len() {
        w.u16(h.layout.parents_of(r).len() as u16);
    }
    for r in 0..h.len() {
        for &p in h.layout.parents_of(r) {
            w.u32(p);
        }
    }
    w.0
}

fn decode_history(b: &[u8]) -> Option<CachedHistory> {
    let start = Instant::now();
    let mut r = R::new(b)?;
    let scope = match r.u8()? {
        0 => Scope::All,
        1 => Scope::Current,
        3 => Scope::Local,
        2 => {
            let n = r.len()?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.str()?);
            }
            Scope::Revs(v)
        }
        _ => return None,
    };
    let hash_len = r.u8()? as usize;
    if hash_len != 20 && hash_len != 32 {
        return None;
    }
    let id = |s: &[u8]| ObjectId::try_from(s).ok();
    let n_tips = r.len()?;
    let mut tips = Vec::with_capacity(n_tips);
    for _ in 0..n_tips {
        tips.push(id(r.take(hash_len)?)?);
    }
    let n = r.len()?;
    let raw = r.take(n.checked_mul(hash_len)?)?;
    let ids: Vec<ObjectId> = raw.chunks_exact(hash_len).map(id).collect::<Option<_>>()?;
    let mut counts = Vec::with_capacity(n);
    for _ in 0..n {
        counts.push(r.u16()? as usize);
    }
    let total: usize = counts.iter().sum();
    let raw = r.take(total.checked_mul(4)?)?;
    let flat: Vec<u32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    if !r.done() {
        return None;
    }
    // Parent rows must point inside the graph.
    if flat.iter().any(|&p| p != gk_graph::NONE && p as usize >= n) {
        return None;
    }
    let mut starts = Vec::with_capacity(n + 1);
    let mut acc = 0;
    starts.push(0);
    for c in &counts {
        acc += c;
        starts.push(acc);
    }
    let history = History::from_rows(
        ids,
        |row| &flat[starts[row]..starts[row + 1]],
        start.elapsed(),
    );
    Some(CachedHistory {
        scope,
        tips,
        history,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_status::Entry;

    fn oid(i: u32) -> ObjectId {
        let mut b = [0u8; 20];
        b[..4].copy_from_slice(&i.to_le_bytes());
        ObjectId::from_bytes_or_panic(&b)
    }

    fn sample_refs() -> Refs {
        let mut r = Refs {
            head: Head::Branch("main".into()),
            head_id: Some("a".repeat(40)),
            state: RepoState::Rebasing { interactive: true },
            branches: vec![Branch {
                name: "main".into(),
                target: "a".repeat(40),
                upstream: Some("origin/main".into()),
                ahead: 2,
                behind: 3,
                upstream_gone: false,
                is_head: true,
            }],
            remote_branches: vec![RemoteBranch {
                remote: "origin".into(),
                name: "main".into(),
                target: "b".repeat(40),
            }],
            tags: vec![Tag {
                name: "v1".into(),
                target: "a".repeat(40),
                annotated: true,
            }],
            stashes: vec![Stash {
                index: 0,
                id: "c".repeat(40),
                message: "WIP on main".into(),
            }],
            remotes: vec![Remote {
                name: "origin".into(),
                fetch_url: "https://x/y.git".into(),
                push_url: "https://x/y.git".into(),
            }],
            labels: Default::default(),
        };
        r.labels = crate::refs::labels_of(&r);
        r
    }

    #[test]
    fn refs_round_trip() {
        let r = sample_refs();
        let back = decode_refs(&encode_refs(&r)).unwrap();
        assert_eq!(back.head, r.head);
        assert_eq!(back.head_id, r.head_id);
        assert_eq!(back.state, r.state);
        assert_eq!(back.branches, r.branches);
        assert_eq!(back.remote_branches, r.remote_branches);
        assert_eq!(back.tags, r.tags);
        assert_eq!(back.stashes, r.stashes);
        assert_eq!(back.remotes, r.remotes);
        assert_eq!(back.labels, r.labels);
        // Truncated or foreign data is a miss, not a panic.
        let enc = encode_refs(&r);
        for cut in [0, 1, 5, enc.len() - 1] {
            assert!(decode_refs(&enc[..cut]).is_none());
        }
        let mut bad = enc.clone();
        bad[0] = VERSION + 1;
        assert!(decode_refs(&bad).is_none());
    }

    #[test]
    fn status_round_trip() {
        let mut renamed = Entry::new("new.txt", Kind::Renamed);
        renamed.orig_path = Some("old.txt".into());
        let s = Status {
            staged: vec![renamed, Entry::new("a", Kind::Added)],
            unstaged: vec![
                Entry::new("c", Kind::Conflict(Conflict::BothModified)),
                Entry::new("d", Kind::Conflict(Conflict::BothDeleted)),
                Entry::new("u", Kind::Untracked),
            ],
            took: Duration::ZERO,
            fsmonitor: false,
        };
        let back = decode_status(&encode_status(&s)).unwrap();
        assert_eq!(back.staged, s.staged);
        assert_eq!(back.unstaged, s.unstaged);
    }

    #[test]
    fn history_round_trip() {
        // 0: merge(1, 2); 1 -> 3; 2 -> 3; 3: parent outside the graph.
        let parents: Vec<Vec<u32>> = vec![vec![1, 2], vec![3], vec![3], vec![gk_graph::NONE]];
        let ids: Vec<ObjectId> = (0..4).map(oid).collect();
        let h = History::from_rows(ids.clone(), |r| &parents[r], Duration::ZERO);
        let tips = vec![oid(0)];
        let scope = Scope::Revs(vec!["main".into()]);
        let enc = encode_history(&scope, &tips, &h);
        let back = decode_history(&enc).unwrap();
        assert_eq!(back.scope, scope);
        assert_eq!(back.tips, tips);
        assert_eq!(back.history.ids, ids);
        assert_eq!(back.history.layout.rows, h.layout.rows);
        for r in 0..4 {
            assert_eq!(back.history.layout.parents_of(r), h.layout.parents_of(r));
            assert_eq!(back.history.layout.row_edges(r), h.layout.row_edges(r));
        }
        assert_eq!(back.history.row(&oid(2)), Some(2));
        assert!(decode_history(&enc[..enc.len() - 2]).is_none());
    }

    #[test]
    fn database_store_load_and_evict() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cache.redb");
        let repo_dir = |i: usize| {
            let p = dir.path().join(format!("repo{i}"));
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let c = RepoCache::open(Some(&file), &repo_dir(0)).unwrap();
        assert!(c.load().refs.is_none());
        let refs = sample_refs();
        let status = Status {
            unstaged: vec![Entry::new("x", Kind::Modified)],
            ..Default::default()
        };
        let parents: Vec<Vec<u32>> = vec![vec![1], vec![]];
        let h = History::from_rows(vec![oid(1), oid(2)], |r| &parents[r], Duration::ZERO);
        c.store(
            Some(&refs),
            Some(&status),
            Some((&Scope::All, &[oid(1)], &h)),
        )
        .unwrap();
        // A second handle on the same file shares the database.
        let c2 = RepoCache::open(Some(&file), &repo_dir(0)).unwrap();
        assert!(Arc::ptr_eq(&c.db, &c2.db));
        let got = c2.load();
        assert_eq!(got.refs.unwrap().branches, refs.branches);
        assert_eq!(got.status.unwrap().unstaged, status.unstaged);
        let gh = got.history.unwrap();
        assert_eq!(gh.history.ids, h.ids);
        assert_eq!(gh.tips, vec![oid(1)]);

        // Fill past the limit: the oldest repository is evicted, the current kept.
        for i in 1..=MAX_REPOS {
            let ci = RepoCache::open(Some(&file), &repo_dir(i)).unwrap();
            ci.store(Some(&refs), None, None).unwrap();
        }
        let tx = c.db.begin_read().unwrap();
        assert_eq!(
            tx.open_table(USED).unwrap().len().unwrap() as usize,
            MAX_REPOS
        );
        drop(tx);
        c.clear().unwrap();
        assert!(c.load().refs.is_none());
    }

    #[test]
    fn writer_coalesces_and_flushes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cache.redb");
        let c = RepoCache::open(Some(&file), dir.path()).unwrap();
        let tx = spawn_writer(c.clone());
        let mut refs = sample_refs();
        tx.send(Write::RefsStatus(
            Arc::new(refs.clone()),
            Arc::new(Status::default()),
        ))
        .unwrap();
        refs.branches[0].ahead = 42;
        tx.send(Write::RefsStatus(
            Arc::new(refs),
            Arc::new(Status::default()),
        ))
        .unwrap();
        tx.send(Write::Flush).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if c.load().refs.is_some_and(|r| r.branches[0].ahead == 42) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "flush did not write the latest refs"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
