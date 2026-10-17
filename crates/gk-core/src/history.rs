use crate::{Repo, repo::gix_err};
use anyhow::Result;
use gix::ObjectId;
use gk_graph::{Layout, NONE, date_order};
use std::collections::HashMap;
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
}

/// Walks all commits reachable from `tips` and lays out the graph.
pub fn build(repo: &Repo, tips: &[ObjectId], interrupt: &AtomicBool) -> Result<History> {
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
    for (n, info) in walk.enumerate() {
        if n % 4096 == 0 && interrupt.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let info = info.map_err(gix_err)?;
        index.insert(info.id, ids.len() as u32);
        ids.push(info.id);
        raw_parents.push(info.parent_ids.iter().copied().collect());
        time.push(info.commit_time.unwrap_or(0));
    }
    let parents: Vec<Vec<u32>> = raw_parents
        .iter()
        .map(|ps| {
            ps.iter()
                .map(|p| index.get(p).copied().unwrap_or(NONE))
                .collect()
        })
        .collect();
    drop(raw_parents);
    let order = date_order(&parents, &time);

    let mut row_of_idx = vec![0u32; ids.len()];
    for (row, &c) in order.iter().enumerate() {
        row_of_idx[c as usize] = row as u32;
    }
    let mut layout = Layout::new();
    let mut prow: Vec<u32> = Vec::with_capacity(4);
    for (row, &c) in order.iter().enumerate() {
        prow.clear();
        prow.extend(parents[c as usize].iter().map(|&p| {
            if p == NONE {
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
    Ok(History {
        ids: ids_ordered,
        layout,
        row_of,
        took: start.elapsed(),
    })
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

/// Data shown in a history row.
#[derive(Debug, Clone)]
pub struct CommitMeta {
    pub summary: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    /// Author time zone offset in seconds.
    pub offset: i32,
}

pub fn commit_meta(g: &gix::Repository, id: ObjectId) -> Result<CommitMeta> {
    let c = g.find_commit(id).map_err(gix_err)?;
    let author = c.author().map_err(gix_err)?;
    let t = author.time().unwrap_or_default();
    let msg = c.message_raw_sloppy();
    let summary = msg.split(|b| *b == b'\n').next().unwrap_or_default();
    Ok(CommitMeta {
        summary: String::from_utf8_lossy(summary).trim().to_string(),
        author: author.name.to_string().trim().to_string(),
        email: author.email.to_string().trim().to_string(),
        time: t.seconds,
        offset: t.offset,
    })
}

/// Thread-safe LRU of row metadata with background batch loading.
pub struct MetaCache {
    repo: Repo,
    cache: parking_lot::Mutex<lru::LruCache<ObjectId, Arc<CommitMeta>>>,
    pending: parking_lot::Mutex<std::collections::HashSet<ObjectId>>,
}

impl MetaCache {
    pub fn new(repo: Repo) -> Self {
        Self {
            repo,
            cache: parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(20_000).unwrap(),
            )),
            pending: Default::default(),
        }
    }

    /// Returns cached metadata, scheduling a load for misses.
    /// `on_loaded` runs on a worker thread after the batch completes.
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
            rayon::spawn(move || {
                let g = this.repo.gix();
                let loaded: Vec<(ObjectId, Option<CommitMeta>)> = missing
                    .iter()
                    .map(|id| (*id, commit_meta(&g, *id).ok()))
                    .collect();
                {
                    let mut cache = this.cache.lock();
                    let mut pending = this.pending.lock();
                    for (id, m) in loaded {
                        pending.remove(&id);
                        let m = m.unwrap_or(CommitMeta {
                            summary: "<unreadable commit>".into(),
                            author: String::new(),
                            email: String::new(),
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
        let m = Arc::new(commit_meta(&self.repo.gix(), id).ok()?);
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
