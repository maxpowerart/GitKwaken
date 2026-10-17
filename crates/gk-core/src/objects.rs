//! Object reads: gitoxide in-process first, a long-lived
//! `git cat-file --batch-command` process ([`gk_cli::CatFilePool`]) as the
//! fallback when gitoxide fails to read an object (an odd pack, an
//! extension it rejects), or for every read when forced
//! ([`ObjectReader::Cli`], `GITKWAKEN_OBJECT_READER=cli`).

use crate::{Repo, repo::gix_err};
use anyhow::{Result, anyhow};
use gix::ObjectId;
use gk_cli::{CatFilePool, ObjectKind};
use parking_lot::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Environment variable forcing the object reader (`cli` or `gix`).
pub const READER_ENV: &str = "GITKWAKEN_OBJECT_READER";

/// cat-file processes per repository: enough for the parallel readers
/// (history search, meta loading, diffs) without a process per thread.
const POOL_SIZE: usize = 4;

/// Which reader serves commit, tree and blob reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectReader {
    /// gitoxide in-process, falling back to `git cat-file` per failed read.
    #[default]
    Gix,
    /// Every read through `git cat-file` (diagnostics and tests).
    Cli,
}

impl ObjectReader {
    /// `GITKWAKEN_OBJECT_READER=cli` (or `cat-file`) forces the CLI reader.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(READER_ENV).ok().as_deref())
    }

    fn parse(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("cli" | "git" | "cat-file" | "catfile") => ObjectReader::Cli,
            _ => ObjectReader::Gix,
        }
    }
}

/// State shared by all clones of a [`Repo`].
#[derive(Default)]
pub(crate) struct Objects {
    pool: OnceLock<CatFilePool>,
    fallbacks: AtomicU64,
    last_fallback: Mutex<Option<String>>,
}

fn kind_of(k: ObjectKind) -> gix::object::Kind {
    match k {
        ObjectKind::Blob => gix::object::Kind::Blob,
        ObjectKind::Tree => gix::object::Kind::Tree,
        ObjectKind::Commit => gix::object::Kind::Commit,
        ObjectKind::Tag => gix::object::Kind::Tag,
    }
}

impl Repo {
    /// Uses `reader` for object reads of this handle and its clones made after.
    pub fn with_object_reader(mut self, reader: ObjectReader) -> Repo {
        self.reader = reader;
        self
    }

    pub fn object_reader(&self) -> ObjectReader {
        self.reader
    }

    pub(crate) fn cli_objects(&self) -> bool {
        self.reader == ObjectReader::Cli
    }

    /// The repository's `git cat-file` processes (started on first use).
    pub fn cat_file(&self) -> &CatFilePool {
        self.objects
            .pool
            .get_or_init(|| CatFilePool::new(self.git.clone(), POOL_SIZE))
    }

    /// Reads gitoxide failed and `git cat-file` served instead.
    pub fn object_fallbacks(&self) -> u64 {
        self.objects.fallbacks.load(Ordering::Relaxed)
    }

    /// One line for Optimize › Diagnostics: the active reader and fallback use.
    pub fn object_reader_status(&self) -> String {
        match self.reader {
            ObjectReader::Cli => format!("Objects: git cat-file --batch (forced by {READER_ENV})"),
            ObjectReader::Gix => match self.object_fallbacks() {
                0 => "Objects: gitoxide in-process (git cat-file fallback unused)".to_string(),
                n => format!(
                    "Objects: gitoxide in-process, git cat-file fallback served {n} read(s) (last: {})",
                    self.objects
                        .last_fallback
                        .lock()
                        .as_deref()
                        .unwrap_or_default()
                ),
            },
        }
    }

    pub(crate) fn note_fallback(&self, what: &str, err: &dyn std::fmt::Display) {
        self.objects.fallbacks.fetch_add(1, Ordering::Relaxed);
        let msg = format!("{what}: {err}");
        tracing::warn!("gitoxide read failed, using git cat-file: {msg}");
        *self.objects.last_fallback.lock() = Some(msg);
    }

    /// `id` read by `git cat-file`, attached to `g`; `None` when missing.
    fn cli_object<'g>(
        &self,
        g: &'g gix::Repository,
        id: ObjectId,
    ) -> Result<Option<gix::Object<'g>>> {
        Ok(self.cat_file().read(&id.to_string())?.map(|(kind, data)| {
            gix::ObjectDetached {
                id,
                kind: kind_of(kind),
                data,
            }
            .attach(g)
        }))
    }

    /// Reads object `id`: gitoxide, or `git cat-file` when gitoxide fails
    /// (or the CLI reader is forced).
    pub fn find_object<'g>(&self, g: &'g gix::Repository, id: ObjectId) -> Result<gix::Object<'g>> {
        if !self.cli_objects() {
            let err = match g.find_object(id) {
                Ok(o) => return Ok(o),
                Err(e) => e,
            };
            return match self.cli_object(g, id) {
                Ok(Some(o)) => {
                    self.note_fallback(&format!("object {id}"), &err);
                    Ok(o)
                }
                _ => Err(gix_err(err)),
            };
        }
        self.cli_object(g, id)?
            .ok_or_else(|| anyhow!("object {id} not found"))
    }

    /// Commit `id` (see [`find_object`](Self::find_object)).
    pub fn find_commit<'g>(&self, g: &'g gix::Repository, id: ObjectId) -> Result<gix::Commit<'g>> {
        if !self.cli_objects()
            && let Ok(c) = g.find_commit(id)
        {
            return Ok(c);
        }
        self.find_object(g, id)?.try_into_commit().map_err(gix_err)
    }

    /// Tree `id` (see [`find_object`](Self::find_object)).
    pub fn find_tree<'g>(&self, g: &'g gix::Repository, id: ObjectId) -> Result<gix::Tree<'g>> {
        if !self.cli_objects()
            && let Ok(t) = g.find_tree(id)
        {
            return Ok(t);
        }
        self.find_object(g, id)?.try_into_tree().map_err(gix_err)
    }
}

/// An object database for gitoxide's algorithms (tree diff, rename
/// detection) that reads through [`Repo::find_object`]'s rules: gitoxide
/// first unless the CLI reader is forced, `git cat-file` otherwise.
pub(crate) struct FallbackOdb<'a> {
    pub repo: &'a Repo,
    pub g: &'a gix::Repository,
}

fn find_err(e: anyhow::Error) -> gix::Error {
    gix::Error::from_error(std::io::Error::other(format!("{e:#}")))
}

impl gix::objs::Find for FallbackOdb<'_> {
    fn try_find<'b>(
        &self,
        id: &gix::oid,
        buffer: &'b mut Vec<u8>,
    ) -> gix::Result<Option<gix::objs::Data<'b>>> {
        let hash = self.g.object_hash();
        let mut kind = None;
        if !self.repo.cli_objects() {
            // Copied out so the CLI path can reuse `buffer` on failure.
            if let Ok(Some(d)) = self.g.objects.try_find(id, buffer) {
                let (k, data) = (d.kind, d.data.to_vec());
                *buffer = data;
                kind = Some(k);
            }
        }
        if kind.is_none() {
            let Some((k, data)) = self
                .repo
                .cat_file()
                .read(&id.to_string())
                .map_err(find_err)?
            else {
                return Ok(None);
            };
            if !self.repo.cli_objects() {
                self.repo
                    .note_fallback(&format!("object {id}"), &"gitoxide could not read it");
            }
            *buffer = data;
            kind = Some(kind_of(k));
        }
        Ok(kind.map(|kind| gix::objs::Data {
            kind,
            object_hash: hash,
            data: buffer.as_slice(),
        }))
    }
}

impl gix::objs::FindHeader for FallbackOdb<'_> {
    fn try_header(&self, id: &gix::oid) -> gix::Result<Option<gix::objs::Header>> {
        if !self.repo.cli_objects()
            && let Ok(Some(h)) = self.g.objects.try_header(id)
        {
            return Ok(Some(h));
        }
        Ok(self
            .repo
            .cat_file()
            .info(&id.to_string())
            .map_err(find_err)?
            .map(|i| gix::objs::Header {
                kind: kind_of(i.kind),
                size: i.size,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values() {
        assert_eq!(ObjectReader::parse(None), ObjectReader::Gix);
        assert_eq!(ObjectReader::parse(Some("gix")), ObjectReader::Gix);
        assert_eq!(ObjectReader::parse(Some(" CLI ")), ObjectReader::Cli);
        assert_eq!(ObjectReader::parse(Some("cat-file")), ObjectReader::Cli);
    }
}
