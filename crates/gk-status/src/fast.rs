//! In-process re-check of a handful of paths reported by the file watcher.
//!
//! A pathspec-limited `git status` still parses the whole index (~300 ms
//! with a million entries). Here the index stays in memory and is reloaded
//! only when `.git/index` changes; each touched path costs one `stat`, and
//! a filtered hash only when the stat data differs from the index.
//! Anything unusual (conflicts, submodules, sparse index, big directories,
//! nested repositories, collapsed-untracked mode) returns [`Outcome::Fallback`].

use crate::{Entry, Kind, Untracked};
use anyhow::{Result, anyhow};
use bstr::{BStr, BString, ByteSlice};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Paths per refresh above which the regular engine is cheaper.
const MAX_PATHS: usize = 200;
/// Tracked entries below one changed directory above which we fall back.
const MAX_ENTRIES_UNDER_DIR: usize = 5000;
/// Untracked files collected from one directory walk before falling back.
const MAX_WALK: usize = 10_000;

pub enum Outcome {
    /// Unstaged entries within the given scope (replace the old ones).
    Updated(Vec<Entry>),
    Fallback,
}

struct Loaded {
    stamp: (SystemTime, u64),
    index: Arc<gix::index::File>,
}

pub struct FastStatus {
    repo: gix::ThreadSafeRepository,
    workdir: PathBuf,
    index_path: PathBuf,
    loaded: Option<Loaded>,
}

fn err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{e}")
}

impl FastStatus {
    pub fn new(workdir: &Path) -> Result<Self> {
        let repo = gix::ThreadSafeRepository::discover(workdir).map_err(err)?;
        let local = repo.to_thread_local();
        let index_path = local.index_path();
        Ok(Self { repo, workdir: workdir.to_path_buf(), index_path, loaded: None })
    }

    fn index(&mut self) -> Result<Arc<gix::index::File>> {
        let meta = std::fs::metadata(&self.index_path)?;
        let stamp = (meta.modified()?, meta.len());
        if let Some(l) = &self.loaded {
            if l.stamp == stamp {
                return Ok(l.index.clone());
            }
        }
        let index = Arc::new(self.repo.to_thread_local().open_index().map_err(err)?);
        self.loaded = Some(Loaded { stamp, index: index.clone() });
        Ok(index)
    }

    /// Re-checks `paths` (repository-relative, `/`-separated). The staged side
    /// is unaffected by worktree edits and is not recomputed.
    pub fn refresh(&mut self, paths: &[BString], untracked: Untracked) -> Result<Outcome> {
        if paths.len() > MAX_PATHS || untracked == Untracked::Normal {
            return Ok(Outcome::Fallback);
        }
        let index = self.index()?;
        if index.is_sparse() {
            return Ok(Outcome::Fallback);
        }
        let repo = self.repo.to_thread_local();
        let mut ctx = Ctx {
            repo: &repo,
            index: &index,
            workdir: &self.workdir,
            pipeline: None,
            excludes: None,
            timestamp_secs: index.timestamp().unix_seconds(),
            out: Vec::new(),
        };
        for p in paths {
            if !ctx.check(p.as_ref(), untracked)? {
                return Ok(Outcome::Fallback);
            }
        }
        Ok(Outcome::Updated(ctx.out))
    }
}

struct Ctx<'a> {
    repo: &'a gix::Repository,
    index: &'a gix::index::File,
    workdir: &'a Path,
    pipeline: Option<gix::filter::Pipeline<'a>>,
    excludes: Option<gix::AttributeStack<'a>>,
    /// When the index was written: files modified at or after it may be racily clean.
    timestamp_secs: i64,
    out: Vec<Entry>,
}

impl<'a> Ctx<'a> {
    fn abs(&self, rel: &BStr) -> PathBuf {
        let mut p = self.workdir.to_path_buf();
        for part in rel.split(|b| *b == b'/') {
            p.push(part.to_str_lossy().as_ref());
        }
        p
    }

    /// Returns false when the path needs the full engine.
    fn check(&mut self, path: &BStr, untracked: Untracked) -> Result<bool> {
        let mut tracked: Vec<usize> = Vec::new();
        if let Some(r) = self.index.entry_range(path) {
            tracked.extend(r);
        }
        let mut prefix = BString::from(path);
        prefix.push(b'/');
        if let Some(r) = self.index.prefixed_entries_range(prefix.as_ref()) {
            if r.len() > MAX_ENTRIES_UNDER_DIR {
                return Ok(false);
            }
            tracked.extend(r);
        }
        let entries = self.index.entries();
        for &i in &tracked {
            let e = &entries[i];
            if e.stage_raw() != 0 || e.mode == gix::index::entry::Mode::COMMIT {
                return Ok(false);
            }
            use gix::index::entry::Flags;
            if e.flags.intersects(Flags::SKIP_WORKTREE | Flags::ASSUME_VALID | Flags::INTENT_TO_ADD) {
                if e.flags.contains(Flags::INTENT_TO_ADD) {
                    return Ok(false);
                }
                continue;
            }
            let rel = e.path(self.index).to_owned();
            if let Some(kind) = self.tracked_change(e, rel.as_ref())? {
                self.out.push(Entry::new(rel, kind));
            }
        }
        if untracked == Untracked::No {
            return Ok(true);
        }
        let abs = self.abs(path);
        let Ok(meta) = std::fs::symlink_metadata(&abs) else { return Ok(true) };
        if meta.is_dir() {
            let mut found = 0usize;
            if !self.walk(path.to_owned(), &abs, &mut found)? {
                return Ok(false);
            }
        } else if tracked.is_empty() && !self.ignored(path, false)? {
            self.out.push(Entry::new(path, Kind::Untracked));
        }
        Ok(true)
    }

    fn tracked_change(&mut self, e: &gix::index::Entry, rel: &BStr) -> Result<Option<Kind>> {
        let abs = self.abs(rel);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(_) => return Ok(Some(Kind::Deleted)),
        };
        let is_link = e.mode == gix::index::entry::Mode::SYMLINK;
        if meta.is_dir() || (meta.is_symlink() != is_link && !cfg!(windows)) {
            return Ok(Some(Kind::TypeChange));
        }
        // Like git's ie_modified(): a different size means modified without
        // looking at the content (e.g. CRLF rewrite under autocrlf shows "M").
        if e.stat.size != 0 && meta.len() as u32 != e.stat.size && !is_link {
            return Ok(Some(Kind::Modified));
        }
        // Same mtime and size and not racily clean: unchanged.
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
        if let Some(m) = mtime {
            let same_time = m.as_secs() as u32 == e.stat.mtime.secs
                && (e.stat.mtime.nsecs == 0 || m.subsec_nanos() == e.stat.mtime.nsecs);
            let same_size = meta.len() as u32 == e.stat.size;
            let racy = m.as_secs() as i64 >= self.timestamp_secs;
            if same_time && same_size && !racy {
                return Ok(None);
            }
        }
        let data = if is_link {
            std::fs::read_link(&abs)?.to_string_lossy().replace('\\', "/").into_bytes()
        } else {
            let raw = std::fs::read(&abs)?;
            self.to_git(rel, raw)?
        };
        let id = gix::objs::compute_hash(self.repo.object_hash(), gix::object::Kind::Blob, &data).map_err(err)?;
        Ok((id != e.id).then_some(Kind::Modified))
    }

    fn to_git(&mut self, rel: &BStr, raw: Vec<u8>) -> Result<Vec<u8>> {
        if self.pipeline.is_none() {
            let (p, _) = self.repo.filter_pipeline(None).map_err(err)?;
            self.pipeline = Some(p);
        }
        let pipeline = self.pipeline.as_mut().expect("pipeline");
        let path = PathBuf::from(rel.to_str_lossy().as_ref());
        use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
        let outcome = pipeline.convert_to_git(raw.as_slice(), &path, self.index).map_err(err)?;
        Ok(match outcome {
            ToGitOutcome::Unchanged(_) => raw.clone(),
            ToGitOutcome::Buffer(b) => b.to_vec(),
            ToGitOutcome::Process(mut r) => {
                let mut v = Vec::new();
                std::io::Read::read_to_end(&mut r, &mut v)?;
                v
            }
        })
    }

    fn ignored(&mut self, rel: &BStr, is_dir: bool) -> Result<bool> {
        if self.excludes.is_none() {
            let stack = self
                .repo
                .excludes(self.index, None, gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped)
                .map_err(err)?;
            self.excludes = Some(stack);
        }
        let mode = if is_dir { Some(gix::index::entry::Mode::DIR) } else { Some(gix::index::entry::Mode::FILE) };
        let path = PathBuf::from(rel.to_str_lossy().as_ref());
        let platform = self.excludes.as_mut().expect("excludes").at_path(&path, mode).map_err(err)?;
        Ok(platform.is_excluded())
    }

    /// Lists untracked, non-ignored files below `rel`. False = fall back.
    fn walk(&mut self, rel: BString, abs: &Path, found: &mut usize) -> Result<bool> {
        if self.ignored(rel.as_ref(), true)? {
            return Ok(true);
        }
        let Ok(rd) = std::fs::read_dir(abs) else { return Ok(true) };
        for de in rd.flatten() {
            let name = de.file_name();
            let name = name.to_string_lossy();
            if name == ".git" {
                // Nested repository: git reports it collapsed.
                return Ok(false);
            }
            let mut child = rel.clone();
            child.push(b'/');
            child.extend_from_slice(name.as_bytes());
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_dir() {
                if !self.walk(child, &de.path(), found)? {
                    return Ok(false);
                }
            } else if self.index.entry_by_path(child.as_ref()).is_none() && !self.ignored(child.as_ref(), false)? {
                *found += 1;
                if *found > MAX_WALK {
                    return Ok(false);
                }
                self.out.push(Entry::new(child, Kind::Untracked));
            }
        }
        Ok(true)
    }
}
