//! In-process bulk staging.
//!
//! `git update-index --add --stdin` reads and hashes every file on one thread
//! (about 70 µs per file on Windows, 7 s for 100k files before any object is
//! written). Here the files are read, converted (autocrlf / `eol` / `text`
//! attributes, through gitoxide's filter pipeline) and written as loose blobs
//! in parallel, and the index is updated in one write with real stat data, so
//! the entries are clean for the next status.
//!
//! Anything this code is not sure to do exactly like `git add` is handed back
//! to git: paths with a clean/process filter driver, `ident` or
//! `working-tree-encoding`, symlinks, submodules and directories, conflicted,
//! intent-to-add, assume-unchanged or skip-worktree entries, file/directory
//! replacements, case-only clashes on case-insensitive file systems, and
//! split or sparse indexes (all of it).

use crate::Repo;
use crate::packwrite::{Compressor, PackWriter};
use anyhow::{Context, Result};
use bstr::ByteSlice;
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::{Exists, Find};
use rayon::prelude::*;
use std::collections::HashSet;

/// What to do with one path.
enum Step {
    /// New content for an existing entry (`Some(index)`) or a new entry.
    Add {
        existing: Option<usize>,
        mode: Mode,
        id: gix::ObjectId,
        stat: Stat,
    },
    /// The file is gone: remove the entry at this position.
    Remove(usize),
    /// Missing and not in the index: nothing to do.
    Skip,
    /// Leave it to git.
    Git,
}

impl Repo {
    /// Stages `paths` (files, not directories) without git where possible.
    /// Returns the paths git still has to stage. On error nothing was written
    /// to the index (blobs may have been written, which is harmless).
    pub(crate) fn stage_in_process(&self, paths: &[String]) -> Result<Vec<String>> {
        let _span = tracing::info_span!("stage_in_process", n = paths.len()).entered();
        // A fresh handle: the configuration (autocrlf, eol, filemode…) may
        // have changed since the repository was opened.
        let ts = gix::ThreadSafeRepository::discover_with_environment_overrides(self.workdir())
            .map_err(crate::repo::gix_err)?;
        let repo = ts.to_thread_local();
        let config = repo.config_snapshot();
        // git's default: case-sensitive.
        let ignore_case = config.boolean("core.ignoreCase").unwrap_or(false);
        let skip_hash = config.boolean("index.skipHash").unwrap_or(false);
        if config.boolean("core.splitIndex") == Some(true) {
            return Ok(paths.to_vec());
        }

        let index_path = repo.index_path();
        // Held until the new index is in place, like git does.
        let lock = gix::lock::File::acquire_to_update_resource(
            &index_path,
            gix::lock::acquire::Fail::AfterDurationWithBackoff(std::time::Duration::from_secs(3)),
            None,
            0,
        )
        .map_err(crate::repo::gix_err)
        .context("index.lock")?;
        let mut file = gix::index::File::at_or_default(
            &index_path,
            repo.object_hash(),
            false,
            Default::default(),
        )
        .map_err(crate::repo::gix_err)?;
        if file.link().is_some() || file.is_sparse() || file.resolve_undo().is_some() {
            // Extensions gitoxide would not write back: let git do it all.
            return Ok(paths.to_vec());
        }

        let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();

        // Contiguous chunks keep each worker's attribute stack warm.
        let threads = rayon::current_num_threads().max(1);
        let chunk = sorted.len().div_ceil(threads * 4).max(64);
        let Ok(pack) = PackWriter::new(&repo) else {
            return Ok(paths.to_vec());
        };
        let state: &gix::index::State = &file;
        let steps: Vec<Step> = sorted
            .par_chunks(chunk)
            .flat_map_iter(|chunk| {
                let out: Vec<Step> = match Worker::new(&ts, self.workdir(), state, &pack) {
                    Ok(mut w) => chunk.iter().map(|p| w.step(p)).collect(),
                    Err(e) => {
                        tracing::debug!("stage worker: {e:#}");
                        chunk.iter().map(|_| Step::Git).collect()
                    }
                };
                out
            })
            .collect();
        // The objects must be in place before the index refers to them.
        pack.finish()?;

        // Serial part: file/directory and case clashes, then the index edit.
        let removed: HashSet<usize> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Remove(i) => Some(*i),
                _ => None,
            })
            .collect();
        let lookup = (ignore_case
            && steps
                .iter()
                .any(|s| matches!(s, Step::Add { existing: None, .. })))
        .then(|| state.prepare_icase_backing());
        let mut git = Vec::new();
        let mut updates = Vec::new();
        let mut adds = Vec::new();
        let mut touched: Vec<&str> = Vec::new();
        for (path, step) in sorted.iter().zip(steps) {
            match step {
                Step::Git => git.push(path.to_string()),
                Step::Skip => {}
                Step::Remove(_) => touched.push(path),
                Step::Add {
                    existing: Some(i),
                    mode,
                    id,
                    stat,
                } => {
                    updates.push((i, mode, id, stat));
                    touched.push(path);
                }
                Step::Add {
                    existing: None,
                    mode,
                    id,
                    stat,
                } => {
                    if new_path_clashes(state, path, &removed, lookup.as_ref()) {
                        git.push(path.to_string());
                    } else {
                        adds.push((path, mode, id, stat));
                        touched.push(path);
                    }
                }
            }
        }
        drop(lookup);
        if touched.is_empty() {
            return Ok(git);
        }

        // Entries that were racily clean against the old index would look
        // clean against the new one: smudge them like git does when writing.
        let old_time = file.timestamp();
        let stat_opts = gix::index::entry::stat::Options::default();
        let updated: HashSet<usize> = updates.iter().map(|u| u.0).collect();
        for (i, e) in file.entries_mut().iter_mut().enumerate() {
            if !updated.contains(&i)
                && e.mode != Mode::COMMIT
                && e.stat.size != 0
                && e.stat.is_racy(old_time, stat_opts)
            {
                e.stat.size = 0;
            }
        }
        {
            let entries = file.entries_mut();
            for (i, mode, id, stat) in updates {
                let e = &mut entries[i];
                e.id = id;
                e.mode = mode;
                e.stat = stat;
            }
        }
        if !removed.is_empty() {
            file.remove_entries(|i, _, _| removed.contains(&i));
        }
        if !adds.is_empty() {
            for (path, mode, id, stat) in adds {
                file.dangerously_push_entry(
                    stat,
                    id,
                    Flags::empty(),
                    mode,
                    path.as_bytes().as_bstr(),
                );
            }
            file.sort_entries();
        }
        if let Some(tree) = file.tree_mut() {
            for p in &touched {
                invalidate_tree(tree, p);
            }
        }

        let mut out = std::io::BufWriter::with_capacity(1 << 20, lock);
        file.write_to(
            &mut out,
            gix::index::write::Options {
                extensions: gix::index::write::Extensions::Given {
                    tree_cache: true,
                    end_of_index_entry: true,
                },
                skip_hash,
            },
        )
        .map_err(crate::repo::gix_err)?;
        let lock = out.into_inner().map_err(|e| e.into_error())?;
        lock.commit().map_err(|e| e.error)?;
        Ok(git)
    }
}

/// Per-thread state: a repository handle, a filter pipeline without drivers
/// and an attribute stack.
struct Worker<'a> {
    repo: gix::Repository,
    pipeline: gix::filter::plumbing::Pipeline,
    attrs: gix::worktree::Stack,
    state: &'a gix::index::State,
    pack: &'a PackWriter,
    workdir: &'a std::path::Path,
    trust_exec: bool,
    z: Compressor,
}

/// Larger files go to git (which streams them) instead of being held in memory.
const MAX_IN_MEMORY: u64 = 64 << 20;

impl<'a> Worker<'a> {
    fn new(
        ts: &gix::ThreadSafeRepository,
        workdir: &'a std::path::Path,
        state: &'a gix::index::State,
        pack: &'a PackWriter,
    ) -> Result<Self> {
        let mut repo = ts.to_thread_local();
        // Most blobs are new: never rescan the pack directory on a miss.
        repo.objects.refresh_never();
        let mut options = gix::filter::Pipeline::options(&repo).map_err(crate::repo::gix_err)?;
        // Paths with any `filter` attribute go to git (checked in `step`).
        options.drivers.clear();
        let pipeline = gix::filter::plumbing::Pipeline::new(
            repo.command_context().map_err(crate::repo::gix_err)?,
            repo.object_hash(),
            options,
        );
        let attrs = repo
            .attributes_only(
                state,
                gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
            )
            .map_err(crate::repo::gix_err)?
            .detach();
        let trust_exec = repo
            .config_snapshot()
            .boolean("core.fileMode")
            .unwrap_or(true);
        Ok(Self {
            repo,
            pipeline,
            attrs,
            state,
            pack,
            workdir,
            trust_exec,
            z: Compressor::new(),
        })
    }

    fn step(&mut self, path: &str) -> Step {
        if !sane_path(path) {
            return Step::Git;
        }
        let bpath = path.as_bytes().as_bstr();
        let state = self.state;
        let existing = match state.entry_range(bpath) {
            None => None,
            Some(r) if r.len() == 1 => {
                let e = state.entry(r.start);
                let odd = Flags::INTENT_TO_ADD | Flags::ASSUME_VALID | Flags::SKIP_WORKTREE;
                if e.stage() != Stage::Unconflicted
                    || e.flags.intersects(odd)
                    || !(e.mode == Mode::FILE || e.mode == Mode::FILE_EXECUTABLE)
                {
                    return Step::Git;
                }
                Some(r.start)
            }
            // Conflict stages.
            Some(_) => return Step::Git,
        };
        let mut abs = self.workdir.to_path_buf();
        abs.extend(path.split('/'));
        let md = match gix::index::fs::Metadata::from_path_no_follow(&abs) {
            Ok(md) => md,
            Err(e) if is_missing(&e) => {
                return match existing {
                    Some(i) => Step::Remove(i),
                    None => Step::Skip,
                };
            }
            Err(_) => return Step::Git,
        };
        if !md.is_file() || md.len() > MAX_IN_MEMORY {
            return Step::Git;
        }
        let Ok(stat) = Stat::from_fs(&md) else {
            return Step::Git;
        };
        let Ok(data) = std::fs::read(&abs) else {
            return Step::Git;
        };
        if data.len() as u64 != md.len() {
            // Changed while we looked: git sorts it out.
            return Step::Git;
        }
        let mode = if self.trust_exec {
            if md.is_executable() {
                Mode::FILE_EXECUTABLE
            } else {
                Mode::FILE
            }
        } else {
            existing.map_or(Mode::FILE, |i| state.entry(i).mode)
        };

        let mut special = false;
        let attrs = &mut self.attrs;
        let objects = &self.repo.objects;
        let converted = self.pipeline.convert_to_git(
            &data[..],
            std::path::Path::new(path),
            &mut |p, out| {
                match attrs.at_entry(p, None, objects) {
                    Ok(platform) => {
                        platform.matching_attributes(out);
                    }
                    Err(_) => special = true,
                }
                for m in out.iter_selected() {
                    use gix::attrs::StateRef;
                    match (m.assignment.name.as_str(), m.assignment.state) {
                        // Whether a driver is configured is git's business.
                        ("filter" | "working-tree-encoding", StateRef::Value(_))
                        | ("ident", StateRef::Set) => special = true,
                        _ => {}
                    }
                }
            },
            &mut |buf| {
                // autocrlf leaves files alone whose index version has CRs.
                let Some(e) = state.entry_by_path(bpath) else {
                    return Ok(None);
                };
                let obj = objects.try_find(&e.id, buf)?;
                Ok(obj
                    .filter(|o| o.kind == gix::object::Kind::Blob)
                    .map(|_| ()))
            },
        );
        use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
        let blob: &[u8] = match &converted {
            _ if special => return Step::Git,
            Err(_) | Ok(ToGitOutcome::Process(_)) => return Step::Git,
            Ok(ToGitOutcome::Unchanged(_)) => &data,
            Ok(ToGitOutcome::Buffer(b)) => b,
        };
        let Ok(id) =
            gix::objs::compute_hash(self.repo.object_hash(), gix::object::Kind::Blob, blob)
        else {
            return Step::Git;
        };
        if !self.repo.objects.exists(&id) {
            self.pack.add_blob(id, blob, &mut self.z);
        }
        Step::Add {
            existing,
            mode,
            id,
            stat,
        }
    }
}

/// ENOENT, or ENOTDIR when a parent became a file.
fn is_missing(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::NotADirectory
}

/// Plain repository-relative paths only; anything odd goes through git's checks.
fn sane_path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains(['\\', '\0', ':'])
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

/// A new entry at `path` would replace a file entry by a directory (or the
/// other way round), or clash by case on a case-insensitive file system.
fn new_path_clashes(
    state: &gix::index::State,
    path: &str,
    removed: &HashSet<usize>,
    icase: Option<&gix::index::AccelerateLookup<'_>>,
) -> bool {
    let b = path.as_bytes().as_bstr();
    if state.path_is_directory(b) {
        return true;
    }
    let mut end = 0;
    while let Some(slash) = path[end..].find('/') {
        end += slash;
        let dir = path.as_bytes()[..end].as_bstr();
        if let Ok(i) = state.entry_index_by_path(dir)
            && !removed.contains(&i)
        {
            return true;
        }
        if let Some(l) = icase
            && !state.path_is_directory(dir)
            && state.path_is_directory_icase(dir, true, l)
        {
            return true;
        }
        end += 1;
    }
    icase.is_some_and(|l| state.entry_by_path_icase(b, true, l).is_some())
}

/// Marks the cached trees containing `path` as invalid (git's
/// `cache_tree_invalidate_path`).
fn invalidate_tree(tree: &mut gix::index::extension::Tree, path: &str) {
    tree.num_entries = None;
    let mut cur = tree;
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    for part in parts {
        match cur
            .children
            .iter_mut()
            .find(|c| c.name.as_slice() == part.as_bytes())
        {
            Some(c) => {
                c.num_entries = None;
                cur = c;
            }
            None => break,
        }
    }
}
