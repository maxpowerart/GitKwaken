//! Writes many new blobs as one pack (plus its `.idx`) instead of one loose
//! file per object. Creating 100k loose files is slow on Windows (and makes
//! `git gc --auto` repack them later); a pack is one sequential file.
//!
//! Objects are compressed by the calling threads; appending is serialized.
//! Pack format: "PACK", version 2, count, entries (type/size header + zlib
//! data), SHA-1 trailer. Index: version 2 (fan-out, sorted ids, CRC-32s,
//! offsets, large offsets, pack and index checksums).

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

pub(crate) struct PackWriter {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    file: Option<std::io::BufWriter<std::fs::File>>,
    tmp: PathBuf,
    offset: u64,
    /// (id, crc32 of the packed entry, offset)
    entries: Vec<(ObjectId, u32, u64)>,
    seen: HashSet<ObjectId>,
    failed: Option<String>,
}

const HEADER_LEN: u64 = 12;

impl PackWriter {
    /// A writer for the repository's `objects/pack` directory (SHA-1 only).
    pub(crate) fn new(repo: &gix::Repository) -> Result<Self> {
        if repo.object_hash() != gix::hash::Kind::Sha1 {
            bail!("pack writing supports SHA-1 repositories only");
        }
        let dir = repo.objects.store_ref().path().join("pack");
        let tmp = dir.join(format!(
            "tmp_pack_gk_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        Ok(Self {
            dir,
            inner: Mutex::new(Inner {
                file: None,
                tmp,
                offset: HEADER_LEN,
                entries: Vec::new(),
                seen: HashSet::new(),
                failed: None,
            }),
        })
    }

    /// Adds blob `data` with id `id` (already computed by the caller).
    /// `z` is the calling thread's compressor (reused: setting one up
    /// allocates a few hundred KB).
    pub(crate) fn add_blob(&self, id: ObjectId, data: &[u8], z: &mut Compressor) {
        if self.inner.lock().seen.contains(&id) {
            return;
        }
        let mut entry = entry_header(3, data.len() as u64);
        if let Err(e) = z.deflate(data, &mut entry) {
            self.inner.lock().failed = Some(e.to_string());
            return;
        }
        let crc = crc32fast::hash(&entry);
        let mut g = self.inner.lock();
        if g.failed.is_some() || !g.seen.insert(id) {
            return;
        }
        if let Err(e) = g.append(&entry) {
            g.failed = Some(format!("{e:#}"));
            return;
        }
        let offset = g.offset;
        g.entries.push((id, crc, offset));
        g.offset += entry.len() as u64;
    }

    /// Completes the pack and its index and moves them into place. Nothing
    /// is left behind when no object was added or on error.
    pub(crate) fn finish(self) -> Result<()> {
        let mut g = self.inner.into_inner();
        let tmp = g.tmp.clone();
        let result = (|| -> Result<()> {
            if let Some(e) = g.failed.take() {
                bail!("writing the pack failed: {e}");
            }
            let Some(file) = g.file.take() else {
                return Ok(());
            };
            let mut file = file.into_inner().map_err(|e| e.into_error())?;
            let count = u32::try_from(g.entries.len()).context("too many objects")?;
            file.seek(SeekFrom::Start(8))?;
            file.write_all(&count.to_be_bytes())?;
            file.seek(SeekFrom::Start(0))?;
            let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            let pack_id = hasher.try_finalize().map_err(crate::repo::gix_err)?;
            file.seek(SeekFrom::End(0))?;
            file.write_all(pack_id.as_bytes())?;
            file.sync_all()?;
            drop(file);

            g.entries.sort_unstable_by_key(|e| e.0);
            let idx = index_bytes(&g.entries, &pack_id)?;
            let base = self.dir.join(format!("pack-{}", pack_id.to_hex()));
            let pack_path = base.with_extension("pack");
            let idx_path = base.with_extension("idx");
            if idx_path.exists() {
                // The very same pack exists already.
                let _ = std::fs::remove_file(&tmp);
                return Ok(());
            }
            std::fs::rename(&tmp, &pack_path)?;
            // git only uses a pack once its .idx exists: write that last.
            let idx_tmp = tmp.with_extension("idx");
            {
                let mut f = std::fs::File::create(&idx_tmp)?;
                f.write_all(&idx)?;
                f.sync_all()?;
            }
            std::fs::rename(&idx_tmp, &idx_path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(tmp.with_extension("idx"));
        }
        result
    }
}

impl Inner {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if self.file.is_none() {
            let mut f = std::io::BufWriter::with_capacity(
                1 << 20,
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&self.tmp)
                    .with_context(|| format!("create {}", self.tmp.display()))?,
            );
            // The count is patched in by `finish`.
            f.write_all(b"PACK\0\0\0\x02\0\0\0\0")?;
            self.file = Some(f);
        }
        let f = self.file.as_mut().expect("just created");
        f.write_all(bytes)?;
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // A writer dropped without `finish` leaves no temporary file.
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// A reusable zlib stream (git's default level).
pub(crate) struct Compressor(flate2::Compress);

impl Compressor {
    pub(crate) fn new() -> Self {
        Self(flate2::Compress::new(flate2::Compression::default(), true))
    }

    /// Appends the zlib stream of `data` to `out`.
    fn deflate(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        self.0.reset();
        out.reserve(data.len() + data.len() / 64 + 64);
        loop {
            let consumed = self.0.total_in() as usize;
            let status =
                self.0
                    .compress_vec(&data[consumed..], out, flate2::FlushCompress::Finish)?;
            if status == flate2::Status::StreamEnd {
                return Ok(());
            }
            out.reserve(out.capacity().max(1024));
        }
    }
}

/// Pack entry header: type in bits 4-6 of the first byte, size as a
/// little-endian base-128 number (4 bits in the first byte).
fn entry_header(kind: u8, mut size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    let mut c = (kind << 4) | (size & 0x0f) as u8;
    size >>= 4;
    while size != 0 {
        out.push(c | 0x80);
        c = (size & 0x7f) as u8;
        size >>= 7;
    }
    out.push(c);
    out
}

/// Version 2 pack index for `entries` sorted by id.
fn index_bytes(entries: &[(ObjectId, u32, u64)], pack_id: &ObjectId) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(8 + 256 * 4 + entries.len() * 28 + 40);
    out.extend_from_slice(b"\xfftOc");
    out.extend_from_slice(&2u32.to_be_bytes());
    let mut fanout = [0u32; 256];
    for (id, _, _) in entries {
        fanout[id.as_bytes()[0] as usize] += 1;
    }
    let mut total = 0u32;
    for n in fanout {
        total += n;
        out.extend_from_slice(&total.to_be_bytes());
    }
    for (id, _, _) in entries {
        out.extend_from_slice(id.as_bytes());
    }
    for (_, crc, _) in entries {
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut large = Vec::new();
    for (_, _, off) in entries {
        if *off < 0x8000_0000 {
            out.extend_from_slice(&(*off as u32).to_be_bytes());
        } else {
            let i = u32::try_from(large.len()).context("pack too large")?;
            out.extend_from_slice(&(0x8000_0000 | i).to_be_bytes());
            large.push(*off);
        }
    }
    for off in large {
        out.extend_from_slice(&off.to_be_bytes());
    }
    out.extend_from_slice(pack_id.as_bytes());
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(&out);
    let idx_id = hasher.try_finalize().map_err(crate::repo::gix_err)?;
    out.extend_from_slice(idx_id.as_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressor_roundtrip() {
        use std::io::Read;
        let mut z = Compressor::new();
        for data in [&b""[..], b"hello\n", &vec![b'x'; 300_000]] {
            let mut out = vec![1, 2];
            z.deflate(data, &mut out).unwrap();
            assert_eq!(&out[..2], &[1, 2]);
            let mut back = Vec::new();
            flate2::read::ZlibDecoder::new(&out[2..])
                .read_to_end(&mut back)
                .unwrap();
            assert_eq!(back, data);
        }
    }

    #[test]
    fn entry_headers() {
        assert_eq!(entry_header(3, 5), vec![0x35]);
        assert_eq!(entry_header(3, 15), vec![0x3f]);
        // 16 = 0b1_0000: low nibble 0, then 1.
        assert_eq!(entry_header(3, 16), vec![0xb0, 0x01]);
        assert_eq!(entry_header(3, 1000), vec![0xb8, 0x3e]);
    }
}
