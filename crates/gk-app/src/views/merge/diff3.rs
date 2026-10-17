//! Line-based three-way merge (diff3 style) used by the merge editor.
//!
//! Both sides are diffed against the base with `gk_diff`; their change
//! regions are aligned over base line ranges. Regions that overlap or touch
//! (like git, edits on adjacent lines conflict) are grouped. A group changed
//! by one side only takes that side, a group changed identically by both
//! takes either, anything else is a conflict.

use std::ops::Range;

/// One file version split into lines (without `\n` or a trailing `\r`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub lines: Vec<String>,
    /// The last line has no terminating newline.
    pub no_eol: bool,
}

impl Text {
    pub fn parse(data: &[u8]) -> Text {
        let s = String::from_utf8_lossy(data);
        if s.is_empty() {
            return Text::default();
        }
        let no_eol = !s.ends_with('\n');
        let body = s.strip_suffix('\n').unwrap_or(&s);
        let lines = body.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        Text { lines, no_eol }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Unchanged on both sides.
    Stable,
    /// Changed on our side only (auto-resolved to ours).
    Ours,
    /// Changed on their side only (auto-resolved to theirs).
    Theirs,
    /// Changed identically on both sides (auto-resolved).
    Same,
    /// Changed differently on both sides.
    Conflict,
}

/// A run of lines; ranges index the line vectors of each version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub kind: ChunkKind,
    pub base: Range<usize>,
    pub ours: Range<usize>,
    pub theirs: Range<usize>,
}

/// How a conflict is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Ours,
    Theirs,
    OursThenTheirs,
    TheirsThenOurs,
}

#[derive(Debug, Clone, Default)]
pub struct Merge3 {
    pub base: Text,
    pub ours: Text,
    pub theirs: Text,
    pub chunks: Vec<Chunk>,
}

/// A changed region of one side relative to the base.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Region {
    base: Range<usize>,
    side: Range<usize>,
}

fn regions(base: &[u8], side: &[u8]) -> Vec<Region> {
    use gk_diff::LineKind;
    let d = gk_diff::diff(base, side, gk_diff::Options::full());
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut cur: Option<Region> = None;
    for l in &d.lines {
        match l.kind {
            LineKind::Context => {
                out.extend(cur.take());
                i += 1;
                j += 1;
            }
            LineKind::Removed => {
                let r = cur.get_or_insert(Region { base: i..i, side: j..j });
                i += 1;
                r.base.end = i;
            }
            LineKind::Added => {
                let r = cur.get_or_insert(Region { base: i..i, side: j..j });
                j += 1;
                r.side.end = j;
            }
        }
    }
    out.extend(cur);
    out
}

impl Merge3 {
    /// Merges three versions. Binary input should be rejected by the caller
    /// (`gk_diff` reports no lines for binary data).
    pub fn new(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merge3 {
        let ro = regions(base, ours);
        let rt = regions(base, theirs);
        let mut m = Merge3 { base: Text::parse(base), ours: Text::parse(ours), theirs: Text::parse(theirs), chunks: Vec::new() };
        m.chunks = chunks(&m, &ro, &rt);
        m
    }

    #[cfg(test)]
    pub fn conflicts(&self) -> impl Iterator<Item = usize> + '_ {
        self.chunks.iter().enumerate().filter(|(_, c)| c.kind == ChunkKind::Conflict).map(|(i, _)| i)
    }

    /// Whether the merged file ends without a newline: a side that changed
    /// the flag relative to the base wins.
    pub fn no_eol(&self) -> bool {
        if self.ours.no_eol == self.base.no_eol { self.theirs.no_eol } else { self.ours.no_eol }
    }

    /// Text of chunk `i` in the result (each line ending with `\n`).
    pub fn chunk_text(&self, i: usize, pick: Option<Pick>, labels: (&str, &str)) -> String {
        let c = &self.chunks[i];
        let mut s = String::new();
        let push = |s: &mut String, t: &Text, r: &Range<usize>| {
            for l in &t.lines[r.clone()] {
                s.push_str(l);
                s.push('\n');
            }
        };
        match c.kind {
            ChunkKind::Stable | ChunkKind::Ours | ChunkKind::Same => push(&mut s, &self.ours, &c.ours),
            ChunkKind::Theirs => push(&mut s, &self.theirs, &c.theirs),
            ChunkKind::Conflict => match pick {
                Some(Pick::Ours) => push(&mut s, &self.ours, &c.ours),
                Some(Pick::Theirs) => push(&mut s, &self.theirs, &c.theirs),
                Some(Pick::OursThenTheirs) => {
                    push(&mut s, &self.ours, &c.ours);
                    push(&mut s, &self.theirs, &c.theirs);
                }
                Some(Pick::TheirsThenOurs) => {
                    push(&mut s, &self.theirs, &c.theirs);
                    push(&mut s, &self.ours, &c.ours);
                }
                None => {
                    s.push_str("<<<<<<< ");
                    s.push_str(labels.0);
                    s.push('\n');
                    push(&mut s, &self.ours, &c.ours);
                    s.push_str("=======\n");
                    push(&mut s, &self.theirs, &c.theirs);
                    s.push_str(">>>>>>> ");
                    s.push_str(labels.1);
                    s.push('\n');
                }
            },
        }
        s
    }

    /// The merged text with `\n` line endings, plus each chunk's start byte
    /// offset and start line. `picks` is indexed by chunk.
    pub fn render(&self, picks: &[Option<Pick>], labels: (&str, &str)) -> Rendered {
        let mut text = String::new();
        let mut offsets = Vec::with_capacity(self.chunks.len());
        let mut lines = Vec::with_capacity(self.chunks.len());
        let mut line = 0usize;
        let mut last_unresolved = false;
        for i in 0..self.chunks.len() {
            let pick = picks.get(i).copied().flatten();
            let t = self.chunk_text(i, pick, labels);
            offsets.push(text.len());
            lines.push(line);
            line += t.matches('\n').count();
            if !t.is_empty() {
                last_unresolved = self.chunks[i].kind == ChunkKind::Conflict && pick.is_none();
            }
            text.push_str(&t);
        }
        if self.no_eol() && !last_unresolved && text.ends_with('\n') {
            text.pop();
        }
        Rendered { text, offsets, lines }
    }
}

pub struct Rendered {
    pub text: String,
    pub offsets: Vec<usize>,
    pub lines: Vec<usize>,
}

fn chunks(m: &Merge3, ro: &[Region], rt: &[Region]) -> Vec<Chunk> {
    let n = m.base.lines.len();
    let mut out = Vec::new();
    let (mut a, mut b) = (0usize, 0usize);
    let (mut pb, mut po, mut pt) = (0usize, 0usize, 0usize);
    loop {
        let start = match (ro.get(a), rt.get(b)) {
            (None, None) => break,
            (Some(x), None) => x.base.start,
            (None, Some(y)) => y.base.start,
            (Some(x), Some(y)) => x.base.start.min(y.base.start),
        };
        if start > pb {
            let len = start - pb;
            out.push(Chunk { kind: ChunkKind::Stable, base: pb..start, ours: po..po + len, theirs: pt..pt + len });
            po += len;
            pt += len;
        }
        // Grow the group while a region of either side overlaps or touches it.
        let (a0, b0) = (a, b);
        let mut end = start;
        loop {
            if let Some(r) = ro.get(a).filter(|r| r.base.start <= end) {
                end = end.max(r.base.end);
                a += 1;
            } else if let Some(r) = rt.get(b).filter(|r| r.base.start <= end) {
                end = end.max(r.base.end);
                b += 1;
            } else {
                break;
            }
        }
        let span = |regs: &[Region], pos: usize| -> Range<usize> {
            match (regs.first(), regs.last()) {
                (Some(f), Some(l)) => f.side.start - (f.base.start - start)..l.side.end + (end - l.base.end),
                _ => pos..pos + (end - start),
            }
        };
        let ours = span(&ro[a0..a], po);
        let theirs = span(&rt[b0..b], pt);
        debug_assert_eq!(ours.start, po);
        debug_assert_eq!(theirs.start, pt);
        let kind = match (a > a0, b > b0) {
            (true, false) => ChunkKind::Ours,
            (false, true) => ChunkKind::Theirs,
            _ if m.ours.lines[ours.clone()] == m.theirs.lines[theirs.clone()] => ChunkKind::Same,
            _ => ChunkKind::Conflict,
        };
        pb = end;
        po = ours.end;
        pt = theirs.end;
        if kind != ChunkKind::Conflict {
            out.push(Chunk { kind, base: start..end, ours, theirs });
            continue;
        }
        // Like git's zealous merge: lines both sides share at the start or end
        // of a conflict are not part of it.
        let (ol, tl) = (&m.ours.lines[ours.clone()], &m.theirs.lines[theirs.clone()]);
        let pre = ol.iter().zip(tl).take_while(|(x, y)| x == y).count();
        let max_suf = ol.len().min(tl.len()) - pre;
        let suf = ol.iter().rev().zip(tl.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
        if pre > 0 {
            out.push(Chunk {
                kind: ChunkKind::Same,
                base: start..start,
                ours: ours.start..ours.start + pre,
                theirs: theirs.start..theirs.start + pre,
            });
        }
        out.push(Chunk {
            kind,
            base: start..end,
            ours: ours.start + pre..ours.end - suf,
            theirs: theirs.start + pre..theirs.end - suf,
        });
        if suf > 0 {
            out.push(Chunk {
                kind: ChunkKind::Same,
                base: end..end,
                ours: ours.end - suf..ours.end,
                theirs: theirs.end - suf..theirs.end,
            });
        }
    }
    if pb < n {
        let len = n - pb;
        out.push(Chunk { kind: ChunkKind::Stable, base: pb..n, ours: po..po + len, theirs: pt..pt + len });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChunkKind::*;

    const L: (&str, &str) = ("ours", "theirs");

    fn kinds(m: &Merge3) -> Vec<ChunkKind> {
        m.chunks.iter().map(|c| c.kind).collect()
    }

    fn merged(base: &str, ours: &str, theirs: &str) -> (Merge3, String) {
        let m = Merge3::new(base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        let t = m.render(&[], L).text;
        (m, t)
    }

    /// Ranges of every chunk must tile each version exactly.
    fn check_tiling(m: &Merge3) {
        let (mut b, mut o, mut t) = (0, 0, 0);
        for c in &m.chunks {
            assert_eq!(c.base.start, b);
            assert_eq!(c.ours.start, o);
            assert_eq!(c.theirs.start, t);
            b = c.base.end;
            o = c.ours.end;
            t = c.theirs.end;
        }
        assert_eq!(b, m.base.lines.len());
        assert_eq!(o, m.ours.lines.len());
        assert_eq!(t, m.theirs.lines.len());
    }

    #[test]
    fn identical_files() {
        let (m, t) = merged("a\nb\n", "a\nb\n", "a\nb\n");
        assert_eq!(kinds(&m), [Stable]);
        assert_eq!(t, "a\nb\n");
        check_tiling(&m);
    }

    #[test]
    fn non_overlapping_edits_merge_cleanly() {
        let (m, t) = merged("1\n2\n3\n4\n5\n", "ONE\n2\n3\n4\n5\n", "1\n2\n3\n4\nFIVE\n");
        assert_eq!(kinds(&m), [Ours, Stable, Theirs]);
        assert_eq!(t, "ONE\n2\n3\n4\nFIVE\n");
        assert_eq!(m.conflicts().count(), 0);
        check_tiling(&m);
    }

    #[test]
    fn same_edit_on_both_sides() {
        let (m, t) = merged("a\nb\nc\n", "a\nB\nc\n", "a\nB\nc\n");
        assert_eq!(kinds(&m), [Stable, Same, Stable]);
        assert_eq!(t, "a\nB\nc\n");
        check_tiling(&m);
    }

    #[test]
    fn overlapping_edits_conflict() {
        let (m, t) = merged("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n");
        assert_eq!(kinds(&m), [Stable, Conflict, Stable]);
        assert_eq!(t, "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nc\n");
        check_tiling(&m);
        let c = m.conflicts().next().unwrap();
        let mut picks = vec![None; m.chunks.len()];
        for (p, want) in [
            (Pick::Ours, "a\nX\nc\n"),
            (Pick::Theirs, "a\nY\nc\n"),
            (Pick::OursThenTheirs, "a\nX\nY\nc\n"),
            (Pick::TheirsThenOurs, "a\nY\nX\nc\n"),
        ] {
            picks[c] = Some(p);
            assert_eq!(m.render(&picks, L).text, want);
        }
    }

    #[test]
    fn partially_overlapping_ranges_group_into_one_conflict() {
        // ours changes 2-3, theirs changes 3-4: one conflict covering 2-4.
        let (m, t) = merged("1\n2\n3\n4\n5\n", "1\nA\nB\n4\n5\n", "1\n2\nC\nD\n5\n");
        assert_eq!(kinds(&m), [Stable, Conflict, Stable]);
        let c = &m.chunks[1];
        assert_eq!(c.base, 1..4);
        assert_eq!(&m.ours.lines[c.ours.clone()], ["A", "B", "4"]);
        assert_eq!(&m.theirs.lines[c.theirs.clone()], ["2", "C", "D"]);
        assert!(t.starts_with("1\n<<<<<<< ours\nA\nB\n4\n=======\n2\nC\nD\n>>>>>>> theirs\n5\n"));
        check_tiling(&m);
    }

    #[test]
    fn adjacent_edits_conflict_like_git() {
        let (m, _) = merged("a\nb\nc\n", "A\nb\nc\n", "a\nB\nc\n");
        assert_eq!(kinds(&m), [Conflict, Stable]);
        check_tiling(&m);
    }

    #[test]
    fn insertions_at_same_point() {
        let (m, t) = merged("a\nb\n", "a\nX\nb\n", "a\nY\nb\n");
        assert_eq!(kinds(&m), [Stable, Conflict, Stable]);
        assert_eq!(m.chunks[1].base, 1..1);
        assert_eq!(t, "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\nb\n");
        check_tiling(&m);
        // The same insertion on both sides is not a conflict.
        let (m, t) = merged("a\nb\n", "a\nX\nb\n", "a\nX\nb\n");
        assert_eq!(kinds(&m), [Stable, Same, Stable]);
        assert_eq!(t, "a\nX\nb\n");
    }

    #[test]
    fn insertions_at_different_points() {
        let (m, t) = merged("a\nb\nc\n", "X\na\nb\nc\n", "a\nb\nc\nY\n");
        assert_eq!(kinds(&m), [Ours, Stable, Theirs]);
        assert_eq!(t, "X\na\nb\nc\nY\n");
        check_tiling(&m);
    }

    #[test]
    fn deletions() {
        // One side deletes, the other leaves the line alone.
        let (m, t) = merged("a\nb\nc\nd\n", "a\nc\nd\n", "a\nb\nc\nD\n");
        assert_eq!(kinds(&m), [Stable, Ours, Stable, Theirs]);
        assert_eq!(t, "a\nc\nD\n");
        check_tiling(&m);
        // Both delete the same line.
        let (m, t) = merged("a\nb\nc\n", "a\nc\n", "a\nc\n");
        assert_eq!(kinds(&m), [Stable, Same, Stable]);
        assert_eq!(t, "a\nc\n");
        // Delete vs modify conflicts; the deleting side is empty.
        let (m, t) = merged("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(kinds(&m), [Stable, Conflict, Stable]);
        assert_eq!(t, "a\n<<<<<<< ours\n=======\nB\n>>>>>>> theirs\nc\n");
        check_tiling(&m);
    }

    #[test]
    fn missing_base_added_by_both() {
        // Shared leading/trailing lines are trimmed out of the conflict.
        let (m, t) = merged("", "x\ny\nw\n", "x\nz\nw\n");
        check_tiling(&m);
        assert_eq!(kinds(&m), [Same, Conflict, Same]);
        assert_eq!(t, "x\n<<<<<<< ours\ny\n=======\nz\n>>>>>>> theirs\nw\n");
        let (m, t) = merged("", "same\n", "same\n");
        assert_eq!(kinds(&m), [Same]);
        assert_eq!(t, "same\n");
    }

    #[test]
    fn eof_without_newline() {
        // Untouched missing newline is preserved.
        let (_, t) = merged("a\nb", "A\nb", "a\nb");
        assert_eq!(t, "A\nb");
        // Edit of the last line keeps it missing.
        let (_, t) = merged("a\nb", "a\nb", "a\nB");
        assert_eq!(t, "a\nB");
        // One side adds the newline, the other edits elsewhere.
        let (_, t) = merged("a\nb", "A\nb", "a\nb\n");
        assert_eq!(t, "A\nb\n");
        // Conflict on the last line: markers keep their newline.
        let (m, t) = merged("a\nb", "a\nX", "a\nY");
        assert_eq!(kinds(&m), [Stable, Conflict]);
        assert_eq!(t, "a\n<<<<<<< ours\nX\n=======\nY\n>>>>>>> theirs\n");
        let mut picks = vec![None; m.chunks.len()];
        picks[1] = Some(Pick::Theirs);
        assert_eq!(m.render(&picks, L).text, "a\nY");
    }

    #[test]
    fn crlf_lines_compare_equal() {
        let (m, t) = merged("a\r\nb\r\n", "a\nB\n", "a\r\nb\r\n");
        assert_eq!(kinds(&m), [Stable, Ours]);
        assert_eq!(t, "a\nB\n");
    }

    #[test]
    fn render_offsets_and_lines() {
        let m = Merge3::new(b"a\nb\nc\n", b"a\nX\nc\n", b"a\nY\nc\n");
        let r = m.render(&[], L);
        assert_eq!(r.offsets, [0, 2, r.text.len() - 2]);
        assert_eq!(r.lines, [0, 1, 6]);
    }

    #[test]
    fn randomized_invariants() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let file = |rnd: &mut dyn FnMut(u64) -> u64| -> String {
            let n = rnd(12);
            let mut s: String = (0..n).map(|_| format!("{}\n", ["a", "b", "c", "d"][rnd(4) as usize])).collect();
            if rnd(5) == 0 {
                s.pop();
            }
            s
        };
        for _ in 0..2000 {
            let (b, o, t) = (file(&mut rnd), file(&mut rnd), file(&mut rnd));
            let m = Merge3::new(b.as_bytes(), o.as_bytes(), t.as_bytes());
            check_tiling(&m);
            // Ours everywhere reproduces ours when theirs did not change anything.
            let m2 = Merge3::new(b.as_bytes(), o.as_bytes(), b.as_bytes());
            assert_eq!(m2.render(&[], L).text, o, "base {b:?} ours {o:?}");
            let m3 = Merge3::new(b.as_bytes(), o.as_bytes(), o.as_bytes());
            assert_eq!(m3.render(&[], L).text, o);
            // Resolving every conflict to one side yields text without markers.
            let picks = vec![Some(Pick::Theirs); m.chunks.len()];
            assert!(!m.render(&picks, L).text.contains("<<<<<<<"));
        }
    }

    #[test]
    fn many_conflicts_and_clean_regions() {
        let base: String = (0..30).map(|i| format!("l{i}\n")).collect();
        let mut o: Vec<String> = (0..30).map(|i| format!("l{i}")).collect();
        let mut t = o.clone();
        for i in [2, 10, 20] {
            o[i] = format!("o{i}");
            t[i] = format!("t{i}");
        }
        o[5] = "o5".into();
        t[15] = "t15".into();
        t[25] = "o25".into();
        o[25] = "o25".into();
        let join = |v: &[String]| v.iter().map(|l| format!("{l}\n")).collect::<String>();
        let m = Merge3::new(base.as_bytes(), join(&o).as_bytes(), join(&t).as_bytes());
        check_tiling(&m);
        assert_eq!(m.conflicts().count(), 3);
        assert_eq!(m.chunks.iter().filter(|c| c.kind == Ours).count(), 1);
        assert_eq!(m.chunks.iter().filter(|c| c.kind == Theirs).count(), 1);
        assert_eq!(m.chunks.iter().filter(|c| c.kind == Same).count(), 1);
    }
}
