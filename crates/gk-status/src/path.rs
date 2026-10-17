//! Compact storage for status paths.
//!
//! A full scan builds all of its paths into one shared byte buffer
//! ([`StatusBuilder`]); each [`PathStr`] is a reference-counted pointer to
//! that buffer plus an offset and a length (16 bytes, no allocation of its
//! own). Compared with one `BString` per path this saves the per-path heap
//! block and its allocator overhead, and makes cloning or dropping a status
//! a matter of reference counts. `gk-bench status-mem` measures it.

use crate::{Entry, Kind, Status};
use bstr::{BStr, BString, ByteSlice};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// An immutable, cheaply clonable repository-relative path. Dereferences to
/// [`BStr`], compares and hashes by its bytes.
#[derive(Clone)]
pub struct PathStr {
    buf: Arc<Vec<u8>>,
    off: u32,
    len: u32,
}

impl PathStr {
    /// A path with a buffer of its own (small results, tests).
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        let v: Vec<u8> = bytes.into();
        let len = u32::try_from(v.len()).expect("path longer than 4 GiB");
        Self {
            buf: Arc::new(v),
            off: 0,
            len,
        }
    }

    pub fn as_bstr(&self) -> &BStr {
        self.buf[self.off as usize..(self.off + self.len) as usize].as_bstr()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.as_bstr()
    }

    pub fn to_bstring(&self) -> BString {
        self.as_bstr().to_owned()
    }

    /// This path with `suffix` appended (a new buffer).
    pub fn with_suffix(&self, suffix: &[u8]) -> Self {
        let mut v = self.as_bytes().to_vec();
        v.extend_from_slice(suffix);
        Self::new(v)
    }
}

impl std::ops::Deref for PathStr {
    type Target = BStr;
    fn deref(&self) -> &BStr {
        self.as_bstr()
    }
}

impl AsRef<BStr> for PathStr {
    fn as_ref(&self) -> &BStr {
        self.as_bstr()
    }
}

impl AsRef<[u8]> for PathStr {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl std::borrow::Borrow<BStr> for PathStr {
    fn borrow(&self) -> &BStr {
        self.as_bstr()
    }
}

impl PartialEq for PathStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for PathStr {}

impl PartialOrd for PathStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PathStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl Hash for PathStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Same as `BStr` / `[u8]`, so `Borrow<BStr>` lookups work.
        self.as_bstr().hash(state)
    }
}

impl fmt::Debug for PathStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_bstr(), f)
    }
}

impl fmt::Display for PathStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_bstr(), f)
    }
}

macro_rules! eq_bytes {
    ($($t:ty),*) => {$(
        impl PartialEq<$t> for PathStr {
            fn eq(&self, other: &$t) -> bool {
                self.as_bytes() == AsRef::<[u8]>::as_ref(other)
            }
        }
        impl PartialEq<PathStr> for $t {
            fn eq(&self, other: &PathStr) -> bool {
                other == self
            }
        }
    )*};
}
eq_bytes!(
    str,
    &str,
    String,
    [u8],
    &[u8],
    Vec<u8>,
    BStr,
    &BStr,
    BString
);

impl From<&str> for PathStr {
    fn from(s: &str) -> Self {
        Self::new(s.as_bytes())
    }
}
impl From<String> for PathStr {
    fn from(s: String) -> Self {
        Self::new(s.into_bytes())
    }
}
impl From<&[u8]> for PathStr {
    fn from(s: &[u8]) -> Self {
        Self::new(s)
    }
}
impl From<Vec<u8>> for PathStr {
    fn from(s: Vec<u8>) -> Self {
        Self::new(s)
    }
}
impl From<&BStr> for PathStr {
    fn from(s: &BStr) -> Self {
        Self::new(s.as_bytes())
    }
}
impl From<BString> for PathStr {
    fn from(s: BString) -> Self {
        Self::new(Vec::from(s))
    }
}
impl From<&BString> for PathStr {
    fn from(s: &BString) -> Self {
        Self::new(s.as_bytes())
    }
}
impl From<PathStr> for BString {
    fn from(p: PathStr) -> Self {
        p.to_bstring()
    }
}
impl From<&PathStr> for BString {
    fn from(p: &PathStr) -> Self {
        p.to_bstring()
    }
}

/// An entry whose paths are spans of the builder's buffer.
struct Raw {
    staged: bool,
    path: (u32, u32),
    orig: Option<(u32, u32)>,
    kind: Kind,
}

/// Collects a status with all paths in one buffer; [`StatusBuilder::finish`]
/// shares it among the entries.
#[derive(Default)]
pub struct StatusBuilder {
    bytes: Vec<u8>,
    raw: Vec<Raw>,
}

impl StatusBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn span(&mut self, parts: &[&[u8]]) -> (u32, u32) {
        let off = self.bytes.len();
        for p in parts {
            self.bytes.extend_from_slice(p);
        }
        let len = self.bytes.len() - off;
        let off = u32::try_from(off).expect("status paths exceed 4 GiB");
        (off, len as u32)
    }

    /// Adds an entry; `path` is the concatenation of `path_parts`.
    pub fn push_parts(
        &mut self,
        staged: bool,
        path_parts: &[&[u8]],
        kind: Kind,
        orig: Option<&[u8]>,
    ) {
        let path = self.span(path_parts);
        let orig = orig.map(|o| self.span(&[o]));
        self.raw.push(Raw {
            staged,
            path,
            orig,
            kind,
        });
    }

    pub fn staged(&mut self, path: &[u8], kind: Kind, orig: Option<&[u8]>) {
        self.push_parts(true, &[path], kind, orig);
    }

    pub fn unstaged(&mut self, path: &[u8], kind: Kind) {
        self.push_parts(false, &[path], kind, None);
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The status (unsorted, `took` zero).
    pub fn finish(mut self) -> Status {
        self.bytes.shrink_to_fit();
        let buf = Arc::new(self.bytes);
        let at = |(off, len): (u32, u32)| PathStr {
            buf: buf.clone(),
            off,
            len,
        };
        let staged_n = self.raw.iter().filter(|r| r.staged).count();
        let mut status = Status {
            staged: Vec::with_capacity(staged_n),
            unstaged: Vec::with_capacity(self.raw.len() - staged_n),
            ..Default::default()
        };
        for r in self.raw {
            let e = Entry {
                path: at(r.path),
                kind: r.kind,
                orig_path: r.orig.map(at),
            };
            if r.staged {
                status.staged.push(e);
            } else {
                status.unstaged.push(e);
            }
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn compact_entries() {
        // 16 bytes per path, niche-optimized options: 40 bytes per entry
        // (was 56 with two `BString`s).
        assert_eq!(std::mem::size_of::<PathStr>(), 16);
        assert_eq!(std::mem::size_of::<Option<PathStr>>(), 16);
        assert!(std::mem::size_of::<Entry>() <= 40);
    }

    #[test]
    fn builder_shares_one_buffer() {
        let mut b = StatusBuilder::new();
        b.staged(b"new.rs", Kind::Renamed, Some(b"old.rs"));
        b.unstaged(b"a/b.txt", Kind::Modified);
        b.push_parts(false, &[b"dir", b"/"], Kind::Untracked, None);
        assert_eq!(b.len(), 3);
        let s = b.finish();
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].path, "new.rs");
        assert_eq!(s.staged[0].orig_path.as_ref().unwrap(), "old.rs");
        assert_eq!(s.unstaged[0].path, "a/b.txt");
        assert_eq!(s.unstaged[1].path, "dir/");
        assert!(Arc::ptr_eq(&s.staged[0].path.buf, &s.unstaged[1].path.buf));
    }

    #[test]
    fn behaves_like_a_byte_string() {
        let a = PathStr::from("src/a.rs");
        let shared = {
            let mut b = StatusBuilder::new();
            b.unstaged(b"xxsrc/a.rs", Kind::Modified);
            b.unstaged(b"src/a.rs", Kind::Modified);
            b.finish().unstaged[1].path.clone()
        };
        assert_eq!(a, shared);
        assert_eq!(a, "src/a.rs");
        assert_eq!("src/a.rs", a);
        assert_eq!(a, BString::from("src/a.rs"));
        assert_eq!(a.to_string(), "src/a.rs");
        assert_eq!(format!("{a:?}"), "\"src/a.rs\"");
        assert!(a.starts_with(b"src/"));
        let (a1, b1) = (PathStr::from("a"), PathStr::from("b"));
        assert!(a1 < b1);
        let set: HashSet<PathStr> = [shared].into_iter().collect();
        assert!(set.contains(BStr::new("src/a.rs")));
        assert_eq!(a.with_suffix(b"/"), "src/a.rs/");
        let back: BString = a.into();
        assert_eq!(back, "src/a.rs");
    }
}
