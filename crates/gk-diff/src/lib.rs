//! Line diffs for display, intra-line (word) highlighting, partial patches,
//! and in-process application of selected lines (stage / unstage / discard).

mod patch;
mod select;
mod words;

pub use patch::{PatchMode, build_patch};
pub use select::{SelectMode, apply_selection};
pub use words::inline_changes;

use imara_diff::{Algorithm, Diff, InternedInput};
use std::borrow::Cow;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One diff line. Byte ranges point into [`FileDiff::text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// 1-based line number on the old side (0 for added lines).
    pub old_no: u32,
    /// 1-based line number on the new side (0 for removed lines).
    pub new_no: u32,
    /// Line bytes without `\n` (a trailing `\r` is kept). For context lines
    /// this is the new side.
    pub text: (u32, u32),
    /// Old-side bytes; differs from `text` only for context lines compared
    /// while ignoring whitespace.
    pub old_text: (u32, u32),
    /// The old-side line is the last of its file and lacks a newline.
    pub old_no_eol: bool,
    /// The new-side line is the last of its file and lacks a newline.
    pub new_no_eol: bool,
    /// Index of the hunk this line belongs to.
    pub hunk: u32,
}

impl Line {
    pub fn no_eol(&self) -> bool {
        match self.kind {
            LineKind::Added => self.new_no_eol,
            LineKind::Removed => self.old_no_eol,
            LineKind::Context => self.new_no_eol || self.old_no_eol,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// Range into [`FileDiff::lines`].
    pub lines: Range<usize>,
}

impl Hunk {
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_len, self.new_start, self.new_len
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub hunks: Vec<Hunk>,
    /// Backing store for line bytes.
    pub text: Vec<u8>,
    pub binary: bool,
    pub additions: u32,
    pub deletions: u32,
    pub old_lines: u32,
    pub new_lines: u32,
    /// Context the diff was computed with.
    pub context: u32,
}

impl FileDiff {
    /// Raw bytes of the line (new side for context lines).
    pub fn line_bytes(&self, line: &Line) -> &[u8] {
        &self.text[line.text.0 as usize..line.text.1 as usize]
    }

    pub fn old_line_bytes(&self, line: &Line) -> &[u8] {
        &self.text[line.old_text.0 as usize..line.old_text.1 as usize]
    }

    /// Display text: lossy UTF-8 without the trailing `\r`.
    pub fn line_text(&self, line: &Line) -> Cow<'_, str> {
        let b = self.line_bytes(line);
        String::from_utf8_lossy(b.strip_suffix(b"\r").unwrap_or(b))
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && !self.binary
    }

    /// Covers the whole file (every line of both sides is present).
    pub fn is_full(&self) -> bool {
        self.context == u32::MAX
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Lines of context around changes; `u32::MAX` shows the whole file.
    pub context: u32,
    pub ignore_whitespace: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            context: 3,
            ignore_whitespace: false,
        }
    }
}

impl Options {
    pub fn full() -> Self {
        Self {
            context: u32::MAX,
            ignore_whitespace: false,
        }
    }
}

/// Heuristic used by git: a NUL byte in the first 8000 bytes means binary.
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// Splits into lines without `\n`; reports whether the final newline is missing.
fn split_lines(data: &[u8]) -> (Vec<&[u8]>, bool) {
    if data.is_empty() {
        return (Vec::new(), false);
    }
    let missing_eol = !data.ends_with(b"\n");
    let body = data.strip_suffix(b"\n").unwrap_or(data);
    (body.split(|b| *b == b'\n').collect(), missing_eol)
}

fn normalize_ws(line: &[u8]) -> Vec<u8> {
    line.iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace())
        .collect()
}

/// Computes a diff between two blobs.
pub fn diff(old: &[u8], new: &[u8], opts: Options) -> FileDiff {
    if is_binary(old) || is_binary(new) {
        return FileDiff {
            binary: true,
            context: opts.context,
            ..Default::default()
        };
    }
    let (a, a_noeol) = split_lines(old);
    let (b, b_noeol) = split_lines(new);

    let (removed, added) = if opts.ignore_whitespace {
        let na: Vec<Vec<u8>> = a.iter().map(|l| normalize_ws(l)).collect();
        let nb: Vec<Vec<u8>> = b.iter().map(|l| normalize_ws(l)).collect();
        line_flags(&na, &nb)
    } else {
        let strip = |l: &&[u8]| -> Vec<u8> { l.strip_suffix(b"\r").unwrap_or(l).to_vec() };
        // Compare ignoring a trailing CR so mixed line endings don't flag every line.
        let na: Vec<Vec<u8>> = a.iter().map(strip).collect();
        let nb: Vec<Vec<u8>> = b.iter().map(strip).collect();
        line_flags(&na, &nb)
    };

    let mut out = FileDiff {
        old_lines: a.len() as u32,
        new_lines: b.len() as u32,
        context: opts.context,
        ..Default::default()
    };
    // Aligned sequence of (kind, old_idx, new_idx).
    let mut seq: Vec<(LineKind, usize, usize)> = Vec::with_capacity(a.len().max(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        if i < a.len() && removed[i] {
            seq.push((LineKind::Removed, i, usize::MAX));
            i += 1;
        } else if j < b.len() && added[j] {
            seq.push((LineKind::Added, usize::MAX, j));
            j += 1;
        } else {
            seq.push((LineKind::Context, i, j));
            i += 1;
            j += 1;
        }
    }

    let ranges: Vec<Range<usize>> = if opts.context == u32::MAX {
        if seq.is_empty() {
            Vec::new()
        } else {
            std::iter::once(0..seq.len()).collect()
        }
    } else {
        let ctx = opts.context as usize;
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (k, s) in seq.iter().enumerate() {
            if s.0 == LineKind::Context {
                continue;
            }
            let start = k.saturating_sub(ctx);
            let end = (k + 1).saturating_add(ctx).min(seq.len());
            match ranges.last_mut() {
                Some(r) if start <= r.end => r.end = r.end.max(end),
                _ => ranges.push(start..end),
            }
        }
        ranges
    };

    let push_text = |out: &mut FileDiff, bytes: &[u8]| -> (u32, u32) {
        let start = out.text.len() as u32;
        out.text.extend_from_slice(bytes);
        (start, out.text.len() as u32)
    };

    for r in ranges {
        let hunk_idx = out.hunks.len() as u32;
        let first_line = out.lines.len();
        let (oi, nj) = position_at(&seq, r.start);
        let (mut old_start, mut new_start) = (oi as u32 + 1, nj as u32 + 1);
        let (mut old_len, mut new_len) = (0u32, 0u32);
        for &(kind, ai, bj) in &seq[r.clone()] {
            let line = match kind {
                LineKind::Removed => {
                    old_len += 1;
                    out.deletions += 1;
                    let t = push_text(&mut out, a[ai]);
                    Line {
                        kind,
                        old_no: ai as u32 + 1,
                        new_no: 0,
                        text: t,
                        old_text: t,
                        old_no_eol: a_noeol && ai + 1 == a.len(),
                        new_no_eol: false,
                        hunk: hunk_idx,
                    }
                }
                LineKind::Added => {
                    new_len += 1;
                    out.additions += 1;
                    let t = push_text(&mut out, b[bj]);
                    Line {
                        kind,
                        old_no: 0,
                        new_no: bj as u32 + 1,
                        text: t,
                        old_text: t,
                        old_no_eol: false,
                        new_no_eol: b_noeol && bj + 1 == b.len(),
                        hunk: hunk_idx,
                    }
                }
                LineKind::Context => {
                    old_len += 1;
                    new_len += 1;
                    let t = push_text(&mut out, b[bj]);
                    let ot = if a[ai] == b[bj] {
                        t
                    } else {
                        push_text(&mut out, a[ai])
                    };
                    Line {
                        kind,
                        old_no: ai as u32 + 1,
                        new_no: bj as u32 + 1,
                        text: t,
                        old_text: ot,
                        old_no_eol: a_noeol && ai + 1 == a.len(),
                        new_no_eol: b_noeol && bj + 1 == b.len(),
                        hunk: hunk_idx,
                    }
                }
            };
            out.lines.push(line);
        }
        // git reports the line before an empty side.
        if old_len == 0 {
            old_start -= 1;
        }
        if new_len == 0 {
            new_start -= 1;
        }
        out.hunks.push(Hunk {
            old_start,
            old_len,
            new_start,
            new_len,
            lines: first_line..out.lines.len(),
        });
    }
    out
}

/// Number of old and new lines consumed before `seq[k]`.
fn position_at(seq: &[(LineKind, usize, usize)], k: usize) -> (usize, usize) {
    let (mut o, mut n) = (0, 0);
    for s in &seq[..k] {
        match s.0 {
            LineKind::Removed => o += 1,
            LineKind::Added => n += 1,
            LineKind::Context => {
                o += 1;
                n += 1;
            }
        }
    }
    (o, n)
}

pub(crate) struct Seq<'a, T>(pub &'a [T]);

impl<'a, T: AsRef<[u8]>> imara_diff::TokenSource for Seq<'a, T> {
    type Token = &'a [u8];
    type Tokenizer = std::iter::Map<std::slice::Iter<'a, T>, fn(&'a T) -> &'a [u8]>;
    fn tokenize(&self) -> Self::Tokenizer {
        self.0.iter().map(|s| s.as_ref())
    }
    fn estimate_tokens(&self) -> u32 {
        self.0.len() as u32
    }
}

/// Per-line removed/added flags for the two sides.
fn line_flags<T: AsRef<[u8]>>(a: &[T], b: &[T]) -> (Vec<bool>, Vec<bool>) {
    let input = InternedInput::new(Seq(a), Seq(b));
    let mut d = Diff::compute(Algorithm::Histogram, &input);
    d.postprocess_no_heuristic(&input);
    let removed = (0..a.len() as u32).map(|i| d.is_removed(i)).collect();
    let added = (0..b.len() as u32).map(|i| d.is_added(i)).collect();
    (removed, added)
}

/// Renders a diff as a unified patch (for copying or saving).
pub fn to_unified(path_old: &str, path_new: &str, d: &FileDiff) -> String {
    let mut s =
        format!("diff --git a/{path_old} b/{path_new}\n--- a/{path_old}\n+++ b/{path_new}\n");
    for h in &d.hunks {
        s.push_str(&h.header());
        s.push('\n');
        for l in &d.lines[h.lines.clone()] {
            s.push(match l.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
            });
            s.push_str(&String::from_utf8_lossy(d.line_bytes(l)));
            s.push('\n');
            if l.no_eol() {
                s.push_str("\\ No newline at end of file\n");
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_modification() {
        let d = diff(b"a\nb\nc\n", b"a\nB\nc\n", Options::default());
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].header(), "@@ -1,3 +1,3 @@");
        let kinds: Vec<_> = d.lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(kinds, [Context, Removed, Added, Context]);
        assert_eq!((d.additions, d.deletions), (1, 1));
    }

    #[test]
    fn far_apart_changes_make_two_hunks() {
        let old: String = (0..30).map(|i| format!("{i}\n")).collect();
        let new: String = (0..30)
            .map(|i| match i {
                2 => "two\n".to_string(),
                27 => "twenty-seven\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let d = diff(old.as_bytes(), new.as_bytes(), Options::default());
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].header(), "@@ -1,6 +1,6 @@");
        assert_eq!(d.hunks[1].header(), "@@ -25,6 +25,6 @@");
    }

    #[test]
    fn new_file_and_no_eol() {
        let d = diff(b"", b"x\ny", Options::default());
        assert_eq!(d.hunks[0].header(), "@@ -0,0 +1,2 @@");
        assert!(d.lines[1].no_eol());
        assert!(to_unified("f", "f", &d).contains("\\ No newline at end of file"));
    }

    #[test]
    fn whitespace_ignored() {
        let d = diff(
            b"a b\n",
            b"a  b\n",
            Options {
                ignore_whitespace: true,
                ..Default::default()
            },
        );
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn crlf_is_preserved_but_not_a_change() {
        let d = diff(b"a\r\nb\r\n", b"a\r\nc\r\n", Options::default());
        assert_eq!(d.deletions, 1);
        assert_eq!(d.line_bytes(&d.lines[0]), b"a\r");
        assert_eq!(d.line_text(&d.lines[0]), "a");
    }

    #[test]
    fn binary_detection() {
        assert!(diff(b"\0abc", b"x", Options::default()).binary);
    }
}
