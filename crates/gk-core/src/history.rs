use crate::{Repo, repo::gix_err};
use anyhow::Result;
use gix::ObjectId;
use gk_graph::{Layout, NONE, date_order};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Which commits the history shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    All,
    /// HEAD only.
    Current,
    /// Specific tips (branch names, tags or ids).
    Revs(Vec<String>),
}

/// The laid-out commit graph in display order.
#[derive(Default)]
pub struct History {
    /// Commit ids by display row.
    pub ids: Vec<ObjectId>,
    pub layout: Layout,
    pub row_of: HashMap<ObjectId, u32>,
    pub took: Duration,
}

impl History {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn row(&self, id: &ObjectId) -> Option<usize> {
        self.row_of.get(id).map(|r| *r as usize)
    }

    /// Builds a history from commits already in display order. `parents(row)`
    /// gives the display rows of that row's parents ([`NONE`] when outside).
    pub fn from_rows<'a>(
        ids: Vec<ObjectId>,
        mut parents: impl FnMut(usize) -> &'a [u32],
        took: Duration,
    ) -> History {
        let mut layout = Layout::new();
        for row in 0..ids.len() {
            layout.push(row as u32, parents(row));
        }
        let row_of = ids
            .iter()
            .enumerate()
            .map(|(r, id)| (*id, r as u32))
            .collect();
        History {
            ids,
            layout,
            row_of,
            took,
        }
    }
}

/// Rows walked before [`build_with_partial`] publishes a first, partial
/// history: several screens, available in a few milliseconds.
pub const PARTIAL_ROWS: usize = 2000;

/// Walks all commits reachable from `tips` and lays out the graph.
pub fn build(repo: &Repo, tips: &[ObjectId], interrupt: &AtomicBool) -> Result<History> {
    build_with_partial(repo, tips, interrupt, usize::MAX, &mut |_| {})
}

/// Like [`build`]; once `partial_rows` commits have been walked (and more
/// remain) `on_partial` gets the layout of just those, so the first screen
/// can be shown while the walk continues. The newest commits come first in
/// the walk, so the partial rows match the final ones except where commit
/// dates are skewed. Setting `interrupt` aborts with an error.
pub fn build_with_partial(
    repo: &Repo,
    tips: &[ObjectId],
    interrupt: &AtomicBool,
    partial_rows: usize,
    on_partial: &mut dyn FnMut(History),
) -> Result<History> {
    let _span = tracing::info_span!("history.build", tips = tips.len()).entered();
    let start = Instant::now();
    if tips.is_empty() {
        return Ok(History::default());
    }
    let g = repo.gix();
    let walk = g
        .rev_walk(tips.iter().copied())
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            Default::default(),
        ))
        .use_commit_graph(true)
        .all()
        .map_err(gix_err)?;

    let mut ids: Vec<ObjectId> = Vec::new();
    let mut index: HashMap<ObjectId, u32> = HashMap::new();
    let mut raw_parents: Vec<Vec<ObjectId>> = Vec::new();
    let mut time: Vec<i64> = Vec::new();
    let walk_span = tracing::info_span!("history.walk").entered();
    for (n, info) in walk.enumerate() {
        if n % 4096 == 0 && interrupt.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        if n == partial_rows {
            let _s = tracing::info_span!("history.partial", rows = n).entered();
            let parents = map_parents(&raw_parents, &index);
            on_partial(assemble(&ids, &parents, &time, start));
        }
        let info = info.map_err(gix_err)?;
        index.insert(info.id, ids.len() as u32);
        ids.push(info.id);
        raw_parents.push(info.parent_ids.iter().copied().collect());
        time.push(info.commit_time.unwrap_or(0));
    }
    drop(walk_span);
    if interrupt.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let parents = map_parents(&raw_parents, &index);
    drop(raw_parents);
    drop(index);
    let _s = tracing::info_span!("history.layout", rows = ids.len()).entered();
    Ok(assemble(&ids, &parents, &time, start))
}

fn map_parents(raw: &[Vec<ObjectId>], index: &HashMap<ObjectId, u32>) -> Vec<Vec<u32>> {
    raw.iter()
        .map(|ps| {
            ps.iter()
                .map(|p| index.get(p).copied().unwrap_or(NONE))
                .collect()
        })
        .collect()
}

/// Date-orders walked commits (walk indices) and lays them out.
fn assemble(ids: &[ObjectId], parents: &[Vec<u32>], time: &[i64], start: Instant) -> History {
    let order = date_order(parents, time);
    let mut row_of_idx = vec![0u32; ids.len()];
    for (row, &c) in order.iter().enumerate() {
        row_of_idx[c as usize] = row as u32;
    }
    let mut layout = Layout::new();
    let mut prow: Vec<u32> = Vec::with_capacity(4);
    for (row, &c) in order.iter().enumerate() {
        prow.clear();
        prow.extend(parents[c as usize].iter().map(|&p| {
            if p == NONE || p as usize >= ids.len() {
                NONE
            } else {
                row_of_idx[p as usize]
            }
        }));
        layout.push(row as u32, &prow);
    }
    let ids_ordered: Vec<ObjectId> = order.iter().map(|&c| ids[c as usize]).collect();
    let row_of = ids_ordered
        .iter()
        .enumerate()
        .map(|(r, id)| (*id, r as u32))
        .collect();
    History {
        ids: ids_ordered,
        layout,
        row_of,
        took: start.elapsed(),
    }
}

/// Resolves scope to tip ids.
pub fn resolve_tips(repo: &Repo, scope: &Scope, all_tips: &[String]) -> Vec<ObjectId> {
    let g = repo.gix();
    let mut out = Vec::new();
    match scope {
        Scope::All => {
            for t in all_tips {
                // Tags may point at trees or blobs, which cannot be walked.
                if let Ok(id) = ObjectId::from_hex(t.as_bytes())
                    && g.find_header(id)
                        .is_ok_and(|h| h.kind() == gix::object::Kind::Commit)
                {
                    out.push(id);
                }
            }
        }
        Scope::Current => {
            if let Ok(id) = g.head_id() {
                out.push(id.detach());
            }
        }
        Scope::Revs(revs) => {
            for r in revs {
                if let Ok(id) = g.rev_parse_single(r.as_str())
                    && let Ok(obj) = id.object()
                    && let Ok(c) = obj.peel_to_commit()
                {
                    out.push(c.id);
                }
            }
        }
    }
    out
}

/// Tips for `scope` read in-process (a few milliseconds, no `git` process):
/// for [`Scope::All`] the same set as [`crate::Refs::tips`] (branches,
/// remote branches, peeled tags, HEAD). Sorted and deduplicated, commits only.
pub fn tip_ids(repo: &Repo, scope: &Scope) -> Vec<ObjectId> {
    let _span = tracing::info_span!("history.tips").entered();
    let mut out = match scope {
        Scope::All => {
            let g = repo.gix();
            let mut v = Vec::new();
            if let Ok(platform) = g.references()
                && let Ok(iter) = platform.all()
            {
                for mut r in iter.flatten() {
                    let name = r.name().as_bstr();
                    let wanted = [&b"refs/heads/"[..], b"refs/remotes/", b"refs/tags/"]
                        .iter()
                        .any(|p| name.starts_with(p));
                    if wanted && let Ok(id) = r.peel_to_id() {
                        v.push(id.detach());
                    }
                }
            }
            if let Ok(id) = g.head_id() {
                v.push(id.detach());
            }
            v.sort();
            v.dedup();
            v.retain(|id| {
                g.find_header(*id)
                    .is_ok_and(|h| h.kind() == gix::object::Kind::Commit)
            });
            v
        }
        _ => resolve_tips(repo, scope, &[]),
    };
    out.sort();
    out.dedup();
    out
}

/// An interned string: clones share one allocation. Dereferences to `str`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct IStr(Arc<str>);

impl IStr {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for IStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for IStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for IStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for IStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl From<&str> for IStr {
    fn from(s: &str) -> Self {
        IStr(Arc::from(s))
    }
}

impl From<String> for IStr {
    fn from(s: String) -> Self {
        IStr(Arc::from(s))
    }
}

impl From<IStr> for String {
    fn from(s: IStr) -> Self {
        s.0.to_string()
    }
}

impl PartialEq<str> for IStr {
    fn eq(&self, o: &str) -> bool {
        &*self.0 == o
    }
}

impl PartialEq<&str> for IStr {
    fn eq(&self, o: &&str) -> bool {
        &*self.0 == *o
    }
}

impl PartialEq<String> for IStr {
    fn eq(&self, o: &String) -> bool {
        *self.0 == **o
    }
}

/// Deduplicates strings that repeat across many commits (author names and
/// emails): a 20k-row cache holds a few hundred distinct authors.
#[derive(Default)]
pub struct Interner {
    set: HashSet<IStr>,
}

impl Interner {
    /// Beyond this many distinct strings the table is reset (entries still in
    /// use stay valid; they just stop being shared with new ones).
    const MAX: usize = 50_000;

    pub fn intern(&mut self, s: &str) -> IStr {
        if let Some(v) = self.set.get(s) {
            return v.clone();
        }
        if self.set.len() >= Self::MAX {
            self.set.clear();
        }
        let v = IStr::from(s);
        self.set.insert(v.clone());
        v
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// Data shown in a history row.
#[derive(Debug, Clone)]
pub struct CommitMeta {
    pub summary: String,
    /// Interned: rows by the same author share the allocation.
    pub author: IStr,
    pub email: IStr,
    /// Author time, seconds since the epoch.
    pub time: i64,
    /// Author time zone offset in seconds.
    pub offset: i32,
}

pub fn commit_meta(g: &gix::Repository, id: ObjectId) -> Result<CommitMeta> {
    commit_meta_with(g, id, &mut |s| IStr::from(s))
}

fn commit_meta_with(
    g: &gix::Repository,
    id: ObjectId,
    intern: &mut dyn FnMut(&str) -> IStr,
) -> Result<CommitMeta> {
    let c = g.find_commit(id).map_err(gix_err)?;
    let author = c.author().map_err(gix_err)?;
    let t = author.time().unwrap_or_default();
    let msg = c.message_raw_sloppy();
    let summary = msg.split(|b| *b == b'\n').next().unwrap_or_default();
    Ok(CommitMeta {
        summary: String::from_utf8_lossy(summary).trim().to_string(),
        author: intern(author.name.to_string().trim()),
        email: intern(author.email.to_string().trim()),
        time: t.seconds,
        offset: t.offset,
    })
}

/// Thread-safe LRU of row metadata with background batch loading.
pub struct MetaCache {
    repo: Repo,
    cache: parking_lot::Mutex<lru::LruCache<ObjectId, Arc<CommitMeta>>>,
    pending: parking_lot::Mutex<std::collections::HashSet<ObjectId>>,
    interner: parking_lot::Mutex<Interner>,
}

impl MetaCache {
    pub fn new(repo: Repo) -> Self {
        Self {
            repo,
            cache: parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(20_000).unwrap(),
            )),
            pending: Default::default(),
            interner: Default::default(),
        }
    }

    fn load(&self, g: &gix::Repository, id: ObjectId) -> Result<CommitMeta> {
        commit_meta_with(g, id, &mut |s| self.interner.lock().intern(s))
    }

    /// Distinct author names and emails currently interned.
    pub fn interned_strings(&self) -> usize {
        self.interner.lock().len()
    }

    /// Returns cached metadata, scheduling a load for misses.
    /// `on_loaded` runs on a worker thread after the batch completes.
    ///
    /// Loads run on the foreground pool ([`crate::pool::foreground`]), so
    /// visible rows never wait behind searches or other background work.
    pub fn get_many(
        self: &Arc<Self>,
        ids: &[ObjectId],
        on_loaded: impl Fn() + Send + 'static,
    ) -> Vec<Option<Arc<CommitMeta>>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut missing = Vec::new();
        {
            let mut cache = self.cache.lock();
            let mut pending = self.pending.lock();
            for id in ids {
                let v = cache.get(id).cloned();
                if v.is_none() && pending.insert(*id) {
                    missing.push(*id);
                }
                out.push(v);
            }
        }
        if !missing.is_empty() {
            let this = self.clone();
            crate::pool::foreground().spawn(move || {
                let _span = tracing::info_span!("history.metas", n = missing.len()).entered();
                let g = this.repo.gix();
                let loaded: Vec<(ObjectId, Option<CommitMeta>)> = missing
                    .iter()
                    .map(|id| (*id, this.load(&g, *id).ok()))
                    .collect();
                {
                    let mut cache = this.cache.lock();
                    let mut pending = this.pending.lock();
                    for (id, m) in loaded {
                        pending.remove(&id);
                        let m = m.unwrap_or(CommitMeta {
                            summary: "<unreadable commit>".into(),
                            author: IStr::default(),
                            email: IStr::default(),
                            time: 0,
                            offset: 0,
                        });
                        cache.put(id, Arc::new(m));
                    }
                }
                on_loaded();
            });
        }
        out
    }

    pub fn get_sync(&self, id: ObjectId) -> Option<Arc<CommitMeta>> {
        if let Some(m) = self.cache.lock().get(&id).cloned() {
            return Some(m);
        }
        let m = Arc::new(self.load(&self.repo.gix(), id).ok()?);
        self.cache.lock().put(id, m.clone());
        Some(m)
    }

    pub fn clear(&self) {
        self.cache.lock().clear();
    }
}

/// Searches commit messages, authors and ids in parallel. Returns matching rows.
pub fn search(repo: &Repo, history: &History, needle: &str, interrupt: &AtomicBool) -> Vec<u32> {
    use rayon::prelude::*;
    let _span = tracing::info_span!("history.search", rows = history.len()).entered();
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let chunk = 2048;
    let mut rows: Vec<u32> = history
        .ids
        .par_chunks(chunk)
        .enumerate()
        .flat_map_iter(|(ci, ids)| {
            let g = repo.gix();
            let mut hits = Vec::new();
            if interrupt.load(Ordering::Relaxed) {
                return hits.into_iter();
            }
            for (i, id) in ids.iter().enumerate() {
                let hex = id.to_string();
                let found = hex.starts_with(&needle)
                    || g.find_commit(*id).ok().is_some_and(|c| {
                        let msg = c.message_raw_sloppy().to_string().to_lowercase();
                        msg.contains(&needle)
                            || c.author().ok().is_some_and(|a| {
                                a.name.to_string().to_lowercase().contains(&needle)
                                    || a.email.to_string().to_lowercase().contains(&needle)
                            })
                    });
                if found {
                    hits.push((ci * chunk + i) as u32);
                }
            }
            hits.into_iter()
        })
        .collect();
    rows.sort_unstable();
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interner_shares_allocations() {
        let mut i = Interner::default();
        let a = i.intern("Ann");
        let b = i.intern("Ann");
        assert!(Arc::ptr_eq(&a.0, &b.0));
        assert_eq!(i.len(), 1);
        assert_eq!(a, "Ann");
        assert_eq!(a.to_string(), "Ann");
        assert_eq!(String::from(a.clone()), "Ann");
        assert_eq!(&*a, "Ann");
        assert_eq!(format!("{a:?}"), "\"Ann\"");
    }

    #[test]
    fn from_rows_matches_layout_pushes() {
        let ids: Vec<ObjectId> = (0..3u8)
            .map(|i| ObjectId::from_bytes_or_panic(&[i; 20]))
            .collect();
        let parents: Vec<Vec<u32>> = vec![vec![1], vec![2], vec![]];
        let h = History::from_rows(ids.clone(), |r| &parents[r], Duration::ZERO);
        assert_eq!(h.len(), 3);
        assert_eq!(h.row(&ids[2]), Some(2));
        assert_eq!(h.layout.parents_of(0), &[1]);
    }
}
