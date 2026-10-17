//! Discard backups. Before working-tree changes are discarded, the current
//! content of the affected paths is saved in a commit recorded in the reflog
//! of [`DISCARD_REF`], so every discard can be undone later (until the reflog
//! expires: `gc.reflogExpireUnreachable`, 30 days by default).
//!
//! `git stash create` would miss untracked files and run clean filters; this
//! saves exactly the bytes on disk, and only of the paths being discarded, so
//! discarding one file costs one blob, one or two trees and one commit.
//!
//! Backup commit (no parent, never checked out):
//!
//! ```text
//! files/<path>   worktree content byte for byte (no filters); 100644/100755/120000
//! index/<path>   index entries of paths "discard all" unstages (staged content)
//! deleted        NUL-separated paths that were missing in the worktree
//! ```

use crate::Repo;
use crate::packwrite::{Compressor, PackWriter};
use crate::repo::gix_err;
use anyhow::{Context, Result, anyhow, bail};
use bstr::ByteSlice;
use gix::ObjectId;
use gix::objs::Exists;
use gix::objs::tree::EntryKind;
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Hidden ref whose reflog lists the backups.
pub const DISCARD_REF: &str = "refs/gitkwaken/discarded";

/// One saved discard, newest first in [`Repo::discard_backups`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardBackup {
    /// Backup commit id (hex).
    pub id: String,
    /// Seconds since the epoch.
    pub time: i64,
    /// Time zone offset of `time` (seconds).
    pub offset: i32,
    /// Reflog message, e.g. `gitkwaken discard: 2 paths (a.txt, b.txt)`.
    pub message: String,
    /// Saved files, sorted.
    pub files: Vec<String>,
    /// Paths that were deleted in the worktree when discarded.
    pub deleted: Vec<String>,
}

/// What [`Repo::restore_discarded`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Files written or deleted.
    pub restored: usize,
    /// Paths that already were in the saved state.
    pub unchanged: usize,
    /// Backup of the local changes that `overwrite` replaced.
    pub overwritten_backup: Option<String>,
}

/// A file to save.
struct Item {
    rel: String,
    abs: PathBuf,
    link: bool,
    exec: bool,
}

/// Above this many files, blobs go into one pack instead of loose objects.
const PACK_FROM: usize = 64;

impl Repo {
    /// Saves the worktree content of `tracked` and `untracked` (directories
    /// with a trailing `/` are saved recursively) and the index entries of
    /// `staged`, and records the backup in the reflog of [`DISCARD_REF`].
    /// Returns `None` when there was nothing to save.
    pub fn backup_discard(
        &self,
        tracked: &[String],
        untracked: &[String],
        staged: &[String],
    ) -> Result<Option<ObjectId>> {
        self.save_backup("discard", tracked, untracked, staged)
    }

    fn save_backup(
        &self,
        what: &str,
        tracked: &[String],
        untracked: &[String],
        staged: &[String],
    ) -> Result<Option<ObjectId>> {
        let _span = tracing::info_span!("backup_discard").entered();
        let mut items = Vec::new();
        let mut deleted = BTreeSet::new();
        let tracked_set: BTreeSet<&str> = tracked.iter().map(|p| clean(p)).collect();
        let all: BTreeSet<&str> = tracked
            .iter()
            .chain(untracked)
            .map(|p| clean(p))
            .filter(|p| !p.is_empty())
            .collect();
        for rel in &all {
            let abs = self.abs(rel);
            match std::fs::symlink_metadata(&abs) {
                Err(e) if missing(&e) => {
                    if tracked_set.contains(rel) {
                        deleted.insert(rel.to_string());
                    }
                }
                Err(e) => return Err(e).with_context(|| format!("reading {rel}")),
                // A tracked directory is a submodule: checkout leaves its
                // content alone, so there is nothing to save.
                Ok(md) if md.is_dir() && tracked_set.contains(rel) => {}
                Ok(md) if md.is_dir() => collect_dir(rel, &abs, &mut items)?,
                Ok(md) => items.push(Item {
                    rel: rel.to_string(),
                    abs,
                    link: md.file_type().is_symlink(),
                    exec: is_exec(&md),
                }),
            }
        }

        let repo = self.gix();
        let mut staged_entries = Vec::new();
        // Loading a large index costs tens of milliseconds: only when needed.
        let index = if staged.is_empty() {
            None
        } else {
            Some(repo.index_or_empty().map_err(gix_err)?)
        };
        for p in staged {
            let Some(index) = &index else { break };
            if let Some(e) = index.entry_by_path(clean(p).as_bytes().as_bstr()) {
                let kind = match e.mode {
                    gix::index::entry::Mode::FILE_EXECUTABLE => EntryKind::BlobExecutable,
                    gix::index::entry::Mode::SYMLINK => EntryKind::Link,
                    gix::index::entry::Mode::COMMIT => EntryKind::Commit,
                    _ => EntryKind::Blob,
                };
                staged_entries.push((clean(p).to_string(), kind, e.id));
            }
        }
        if items.is_empty() && deleted.is_empty() && staged_entries.is_empty() {
            return Ok(None);
        }

        // Blobs, in parallel; many of them go into one pack.
        let pack = (items.len() >= PACK_FROM)
            .then(|| PackWriter::new(&repo).ok())
            .flatten();
        let ids: Vec<ObjectId> = items
            .par_iter()
            .map_init(
                || {
                    let mut r = self.gix();
                    r.objects.refresh_never();
                    (r, Compressor::new())
                },
                |(r, z), it| -> Result<ObjectId> {
                    let data = if it.link {
                        let target = std::fs::read_link(&it.abs)
                            .with_context(|| format!("reading {}", it.rel))?;
                        target.to_string_lossy().replace('\\', "/").into_bytes()
                    } else {
                        std::fs::read(&it.abs).with_context(|| format!("reading {}", it.rel))?
                    };
                    let id =
                        gix::objs::compute_hash(r.object_hash(), gix::object::Kind::Blob, &data)
                            .map_err(gix_err)?;
                    if !r.objects.exists(&id) {
                        match &pack {
                            Some(p) => p.add_blob(id, &data, z),
                            None => {
                                r.write_blob(&data).map_err(gix_err)?;
                            }
                        }
                    }
                    Ok(id)
                },
            )
            .collect::<Result<_>>()?;
        if let Some(p) = pack {
            p.finish()?;
        }

        let mut ed = repo
            .edit_tree(ObjectId::empty_tree(repo.object_hash()))
            .map_err(gix_err)?;
        for (it, id) in items.iter().zip(&ids) {
            let kind = if it.link {
                EntryKind::Link
            } else if it.exec {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            };
            ed.upsert(format!("files/{}", it.rel).as_str(), kind, *id)
                .map_err(gix_err)
                .with_context(|| format!("cannot save {}", it.rel))?;
        }
        for (p, kind, id) in &staged_entries {
            ed.upsert(format!("index/{p}").as_str(), *kind, *id)
                .map_err(gix_err)
                .with_context(|| format!("cannot save {p}"))?;
        }
        if !deleted.is_empty() {
            let list = gk_cli::nul_paths(deleted.iter());
            let id = repo.write_blob(&list).map_err(gix_err)?.detach();
            ed.upsert("deleted", EntryKind::Blob, id).map_err(gix_err)?;
        }
        let tree = ed.write().map_err(gix_err)?.detach();

        let mut paths: Vec<&str> = items.iter().map(|i| i.rel.as_str()).collect();
        paths.extend(deleted.iter().map(String::as_str));
        if paths.is_empty() {
            paths.extend(staged_entries.iter().map(|(p, _, _)| p.as_str()));
        }
        paths.sort_unstable();
        paths.dedup();
        let n = paths.len();
        let mut message = format!("gitkwaken {what}: {n} path{}\n\n", plural(n));
        for p in paths.iter().take(100) {
            message.push_str(p);
            message.push('\n');
        }
        if n > 100 {
            message.push_str(&format!("… and {} more\n", n - 100));
        }
        let sig = signature(&repo);
        let commit = gix::objs::Commit {
            tree,
            parents: Default::default(),
            author: sig.clone(),
            committer: sig.clone(),
            encoding: None,
            message: message.into(),
            extra_headers: Vec::new(),
        };
        let id = repo.write_object(&commit).map_err(gix_err)?.detach();

        let shown: Vec<&str> = paths.iter().take(3).copied().collect();
        let reflog = format!(
            "gitkwaken {what}: {n} path{} ({}{})",
            plural(n),
            shown.join(", "),
            if n > 3 { ", …" } else { "" }
        )
        .replace(['\n', '\r'], " ");
        self.record_backup(&repo, id, &reflog, &sig)?;
        Ok(Some(id))
    }

    /// Points [`DISCARD_REF`] at `id`, always with a reflog entry.
    fn record_backup(
        &self,
        repo: &gix::Repository,
        id: ObjectId,
        message: &str,
        sig: &gix::actor::Signature,
    ) -> Result<()> {
        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
        if repo.refs.write_reflog == gix::refs::store::WriteReflog::Disable {
            // core.logAllRefUpdates=false: gitoxide writes no reflog at all,
            // git still does with --create-reflog.
            let hex = id.to_hex().to_string();
            self.git().run(&[
                "update-ref",
                "--create-reflog",
                "-m",
                message,
                DISCARD_REF,
                &hex,
            ])?;
            return Ok(());
        }
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: message.into(),
                },
                expected: PreviousValue::Any,
                new: gix::refs::Target::Object(id),
            },
            name: DISCARD_REF.try_into().map_err(gix_err)?,
            deref: false,
        };
        let mut buf = gix::date::parse::TimeBuf::default();
        repo.edit_references_as(Some(edit), Some(sig.to_ref(&mut buf)))
            .map_err(gix_err)?;
        Ok(())
    }

    /// Saved discards, newest first (at most `limit`).
    pub fn discard_backups(&self, limit: usize) -> Result<Vec<DiscardBackup>> {
        let repo = self.gix();
        let Ok(r) = repo.find_reference(DISCARD_REF) else {
            return Ok(Vec::new());
        };
        let mut platform = r.log_iter();
        let Some(lines) = platform.rev().map_err(gix_err)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for line in lines.take(limit) {
            let Ok(line) = line else { continue };
            let id = line.new_oid.to_owned();
            let Ok(saved) = read_backup(&repo, id) else {
                continue;
            };
            out.push(DiscardBackup {
                id: id.to_hex().to_string(),
                time: line.signature.time.seconds,
                offset: line.signature.time.offset,
                message: line.message.to_str_lossy().into_owned(),
                files: saved.files.into_iter().map(|f| f.0).collect(),
                deleted: saved.deleted,
            });
        }
        Ok(out)
    }

    /// Puts the saved worktree content of backup `id` back: saved files are
    /// written, paths that were deleted are deleted again. Paths whose
    /// current content is neither the saved one nor clean (equal to the
    /// index) are local changes: without `overwrite` nothing is restored and
    /// the error lists them; with it, they are backed up first, then replaced.
    pub fn restore_discarded(&self, id: &str, overwrite: bool) -> Result<RestoreOutcome> {
        let repo = self.gix();
        let oid = ObjectId::from_hex(id.trim().as_bytes())
            .map_err(|e| anyhow!("not an object id: {id}: {e}"))?;
        let saved = read_backup(&repo, oid)?;
        let index = repo.index_or_empty().map_err(gix_err)?;
        let (mut pipeline, _) = repo.filter_pipeline(None).map_err(gix_err)?;
        let mut clean_tracked = |rel: &str, data: &[u8]| -> bool {
            let Some(e) = index.entry_by_path(rel.as_bytes().as_bstr()) else {
                return false;
            };
            if e.stage_raw() != 0 {
                return false;
            }
            use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
            let Ok(out) = pipeline.convert_to_git(data, Path::new(rel), &index) else {
                return false;
            };
            let blob = match out {
                ToGitOutcome::Unchanged(d) => d.to_vec(),
                ToGitOutcome::Buffer(b) => b.to_vec(),
                ToGitOutcome::Process(mut r) => {
                    let mut v = Vec::new();
                    if std::io::Read::read_to_end(&mut r, &mut v).is_err() {
                        return false;
                    }
                    v
                }
            };
            gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &blob)
                .is_ok_and(|h| h == e.id)
        };

        let mut writes: Vec<(String, EntryKind, Vec<u8>)> = Vec::new();
        let mut removes: Vec<String> = Vec::new();
        let mut conflicts: Vec<String> = Vec::new();
        let mut unchanged = 0;
        for (rel, kind, blob) in saved.files {
            let want = repo.find_blob(blob).map_err(gix_err)?.data.clone();
            match current(&self.abs(&rel)) {
                Current::Missing => writes.push((rel, kind, want)),
                Current::Dir => conflicts.push(rel),
                Current::File(have) if have == want => unchanged += 1,
                Current::File(have) => {
                    if clean_tracked(&rel, &have) {
                        writes.push((rel, kind, want));
                    } else {
                        conflicts.push(rel.clone());
                        writes.push((rel, kind, want));
                    }
                }
            }
        }
        for rel in saved.deleted {
            match current(&self.abs(&rel)) {
                Current::Missing => unchanged += 1,
                Current::Dir => conflicts.push(rel),
                Current::File(have) => {
                    if !clean_tracked(&rel, &have) {
                        conflicts.push(rel.clone());
                    }
                    removes.push(rel);
                }
            }
        }
        let mut outcome = RestoreOutcome {
            unchanged,
            ..Default::default()
        };
        if !conflicts.is_empty() {
            conflicts.sort();
            let dirs: Vec<&String> = conflicts.iter().filter(|c| self.abs(c).is_dir()).collect();
            if !dirs.is_empty() {
                bail!(
                    "cannot restore: {} is a folder now",
                    dirs.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !overwrite {
                let shown: Vec<&str> = conflicts.iter().take(5).map(String::as_str).collect();
                bail!(
                    "{} file{} changed since the discard and would be overwritten: {}{}",
                    conflicts.len(),
                    plural(conflicts.len()),
                    shown.join(", "),
                    if conflicts.len() > 5 { ", …" } else { "" }
                );
            }
            // Nothing is lost: what gets overwritten becomes a backup itself.
            let (tracked, untracked): (Vec<String>, Vec<String>) = conflicts
                .iter()
                .cloned()
                .partition(|c| index.entry_by_path(c.as_bytes().as_bstr()).is_some());
            outcome.overwritten_backup = self
                .save_backup("restore", &tracked, &untracked, &[])
                .context("could not back up the local changes; nothing was restored")?
                .map(|id| id.to_hex().to_string());
        }
        for (rel, kind, data) in writes {
            write_file(&self.abs(&rel), kind, &data).with_context(|| format!("writing {rel}"))?;
            outcome.restored += 1;
        }
        for rel in removes {
            std::fs::remove_file(self.abs(&rel)).with_context(|| format!("deleting {rel}"))?;
            outcome.restored += 1;
        }
        Ok(outcome)
    }
}

/// Saved content of a backup commit.
struct Saved {
    files: Vec<(String, EntryKind, ObjectId)>,
    deleted: Vec<String>,
}

fn read_backup(repo: &gix::Repository, id: ObjectId) -> Result<Saved> {
    let commit = repo
        .find_commit(id)
        .map_err(gix_err)
        .with_context(|| format!("{id} is not a discard backup"))?;
    let tree = commit.tree().map_err(gix_err)?;
    let mut files = Vec::new();
    if let Some(e) = tree.find_entry("files") {
        let sub = e
            .object()
            .map_err(gix_err)?
            .try_into_tree()
            .map_err(gix_err)?;
        let mut rec = gix::traverse::tree::Recorder::default();
        sub.traverse().breadthfirst(&mut rec).map_err(gix_err)?;
        for r in rec.records {
            if r.mode.is_tree() {
                continue;
            }
            files.push((r.filepath.to_str_lossy().into_owned(), r.mode.kind(), r.oid));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut deleted = Vec::new();
    if let Some(e) = tree.find_entry("deleted") {
        let blob = e.object().map_err(gix_err)?;
        deleted = blob
            .data
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_str_lossy().into_owned())
            .collect();
    }
    Ok(Saved { files, deleted })
}

enum Current {
    Missing,
    Dir,
    File(Vec<u8>),
}

fn current(abs: &Path) -> Current {
    match std::fs::symlink_metadata(abs) {
        Err(_) => Current::Missing,
        Ok(md) if md.is_dir() => Current::Dir,
        Ok(md) if md.file_type().is_symlink() => match std::fs::read_link(abs) {
            Ok(t) => Current::File(t.to_string_lossy().replace('\\', "/").into_bytes()),
            Err(_) => Current::File(Vec::new()),
        },
        Ok(_) => std::fs::read(abs).map_or(Current::File(Vec::new()), Current::File),
    }
}

fn write_file(abs: &Path, kind: EntryKind, data: &[u8]) -> Result<()> {
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(abs).is_ok() {
        std::fs::remove_file(abs)?;
    }
    #[cfg(unix)]
    if kind == EntryKind::Link {
        use std::os::unix::ffi::OsStrExt;
        let target = std::ffi::OsStr::from_bytes(data);
        std::os::unix::fs::symlink(target, abs)?;
        return Ok(());
    }
    std::fs::write(abs, data)?;
    #[cfg(unix)]
    if kind == EntryKind::BlobExecutable {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(abs)?.permissions();
        p.set_mode(p.mode() | 0o111);
        std::fs::set_permissions(abs, p)?;
    }
    let _ = kind;
    Ok(())
}

/// Collects the files below an untracked directory (all of them, ignored
/// ones too: discarding deletes the whole directory). A nested repository
/// is refused rather than deleted.
fn collect_dir(rel: &str, abs: &Path, out: &mut Vec<Item>) -> Result<()> {
    for entry in std::fs::read_dir(abs).with_context(|| format!("reading {rel}/"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = format!("{rel}/{name}");
        if name.eq_ignore_ascii_case(".git") {
            bail!("{rel}/ contains a git repository; it is not deleted by a discard");
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_dir(&child_rel, &entry.path(), out)?;
        } else {
            let md = entry.metadata()?;
            out.push(Item {
                rel: child_rel,
                abs: entry.path(),
                link: ft.is_symlink(),
                exec: is_exec(&md),
            });
        }
    }
    Ok(())
}

fn clean(p: &str) -> &str {
    p.trim_end_matches('/')
}

fn missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

fn is_exec(md: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.is_file() && md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        false
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The configured committer, or a fixed fallback (a backup must not fail
/// because `user.name` is unset).
fn signature(repo: &gix::Repository) -> gix::actor::Signature {
    let now = gix::date::Time::now_local_or_utc();
    match repo.committer().and_then(Result::ok) {
        Some(s) => gix::actor::Signature {
            name: s.name.to_owned(),
            email: s.email.to_owned(),
            time: now,
        },
        None => gix::actor::Signature {
            name: "GitKwaken".into(),
            email: "gitkwaken@localhost".into(),
            time: now,
        },
    }
}
