//! Pure helpers of the diff viewer: display rows (unified and side by side),
//! removed/added pairing, line selection and tab expansion. Everything here
//! is computed once per loaded diff, never per frame.

use gk_diff::{FileDiff, Line, LineKind};
use std::collections::{BTreeSet, HashSet};

/// Lines longer than this are cut for display.
pub const MAX_DISPLAY_CHARS: usize = 2000;
/// Marker for "no line" in a side-by-side pair.
pub const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Header of hunk `n`.
    Hunk(u32),
    /// Unified view: one line of the diff.
    Line(u32),
    /// Side by side: the old-side line and the new-side line ([`NONE`] when absent).
    Pair(u32, u32),
}

/// Pairs lines for side-by-side display. Context lines pair with themselves;
/// a run of removed lines followed by a run of added lines pairs up
/// element-wise, the longer run's rest stays alone.
pub fn pair_lines(kinds: &[LineKind]) -> Vec<(Option<usize>, Option<usize>)> {
    let n = kinds.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if kinds[i] == LineKind::Context {
            out.push((Some(i), Some(i)));
            i += 1;
            continue;
        }
        let rs = i;
        while i < n && kinds[i] == LineKind::Removed {
            i += 1;
        }
        let re = i;
        while i < n && kinds[i] == LineKind::Added {
            i += 1;
        }
        let (as_, ae) = (re, i);
        let len = (re - rs).max(ae - as_);
        for k in 0..len {
            out.push((
                (rs + k < re).then_some(rs + k),
                (as_ + k < ae).then_some(as_ + k),
            ));
        }
    }
    out
}

/// Display layout of one loaded diff.
#[derive(Debug, Default)]
pub struct Layout {
    pub unified: Vec<Row>,
    pub sbs: Vec<Row>,
    /// Row index of each hunk header, per view.
    pub unified_hunks: Vec<u32>,
    pub sbs_hunks: Vec<u32>,
    /// Removed line ↔ added line it replaces (for word highlights), or [`NONE`].
    pub partner: Vec<u32>,
    /// Widest line in columns (tabs expanded, capped).
    pub max_cols: usize,
    /// Digits of the largest line number.
    pub digits: usize,
}

impl Layout {
    pub fn build(d: &FileDiff, tab: usize) -> Layout {
        let mut l = Layout {
            unified: Vec::with_capacity(d.lines.len() + d.hunks.len()),
            partner: vec![NONE; d.lines.len()],
            digits: d.old_lines.max(d.new_lines).max(1).to_string().len(),
            ..Default::default()
        };
        let tab = tab.max(1);
        for (h, hunk) in d.hunks.iter().enumerate() {
            l.unified_hunks.push(l.unified.len() as u32);
            l.unified.push(Row::Hunk(h as u32));
            l.unified
                .extend(hunk.lines.clone().map(|i| Row::Line(i as u32)));

            l.sbs_hunks.push(l.sbs.len() as u32);
            l.sbs.push(Row::Hunk(h as u32));
            let kinds: Vec<LineKind> = d.lines[hunk.lines.clone()].iter().map(|x| x.kind).collect();
            let base = hunk.lines.start;
            for (a, b) in pair_lines(&kinds) {
                let a = a.map(|x| x + base);
                let b = b.map(|x| x + base);
                let (left, right) = match (a, b) {
                    // A context line is the same line on both sides.
                    (Some(x), Some(y)) if x == y => (x as u32, x as u32),
                    (Some(x), Some(y)) => {
                        l.partner[x] = y as u32;
                        l.partner[y] = x as u32;
                        (x as u32, y as u32)
                    }
                    (Some(x), None) => (x as u32, NONE),
                    (None, Some(y)) => (NONE, y as u32),
                    (None, None) => continue,
                };
                l.sbs.push(Row::Pair(left, right));
            }
        }
        for line in &d.lines {
            let b = d.line_bytes(line);
            let tabs = b.iter().filter(|c| **c == b'\t').count();
            let cols = (b.len() + tabs * (tab - 1)).min(MAX_DISPLAY_CHARS + 1);
            l.max_cols = l.max_cols.max(cols);
        }
        l
    }

    pub fn rows(&self, sbs: bool) -> &[Row] {
        if sbs { &self.sbs } else { &self.unified }
    }

    pub fn hunk_rows(&self, sbs: bool) -> &[u32] {
        if sbs {
            &self.sbs_hunks
        } else {
            &self.unified_hunks
        }
    }
}

/// Identifies a changed line by content position, so it can be found again
/// in the full-context diff the core recomputes: `(is_added, line number on
/// the side that carries the change)`.
pub type LineKey = (bool, u32);

pub fn line_key(l: &Line) -> Option<LineKey> {
    match l.kind {
        LineKind::Added => Some((true, l.new_no)),
        LineKind::Removed => Some((false, l.old_no)),
        LineKind::Context => None,
    }
}

/// Predicate for `stage_lines` / `discard_lines`.
pub fn picker(keys: &[LineKey]) -> impl Fn(&Line) -> bool + Send + Sync + 'static {
    let set: HashSet<LineKey> = keys.iter().copied().collect();
    move |l: &Line| line_key(l).is_some_and(|k| set.contains(&k))
}

/// Keys of every changed line of hunk `h`.
pub fn hunk_keys(d: &FileDiff, h: usize) -> Vec<LineKey> {
    d.hunks
        .get(h)
        .map(|hk| {
            d.lines[hk.lines.clone()]
                .iter()
                .filter_map(line_key)
                .collect()
        })
        .unwrap_or_default()
}

/// The hunk covering display row `top` (hunk headers at rows `hunk_rows`).
pub fn hunk_at(hunk_rows: &[u32], top: u32) -> Option<usize> {
    (!hunk_rows.is_empty()).then(|| hunk_rows.iter().rposition(|&r| r <= top).unwrap_or(0))
}

/// Whether any row of hunk `h` lies within display rows `top..=bottom`.
pub fn hunk_on_screen(hunk_rows: &[u32], total: u32, h: usize, top: u32, bottom: u32) -> bool {
    let Some(&start) = hunk_rows.get(h) else {
        return false;
    };
    let end = hunk_rows.get(h + 1).copied().unwrap_or(total);
    start <= bottom && end > top
}

/// Next (`down`) or previous changed line after `from`; without `from`,
/// the first changed line of hunk `h`.
pub fn step_changed(d: &FileDiff, from: Option<u32>, down: bool, h: Option<usize>) -> Option<u32> {
    let changed = |i: &usize| d.lines[*i].kind != LineKind::Context;
    let n = d.lines.len();
    let i = match from.filter(|&f| (f as usize) < n) {
        Some(f) if down => (f as usize + 1..n).find(changed),
        Some(f) => (0..f as usize).rev().find(changed),
        None => {
            let start = h
                .and_then(|h| d.hunks.get(h))
                .map_or(0, |hk| hk.lines.start);
            (start..n).find(changed)
        }
    };
    i.map(|i| i as u32)
}

/// Display row showing diff line `i`.
pub fn row_of_line(rows: &[Row], i: u32) -> Option<usize> {
    rows.iter().position(|r| match *r {
        Row::Line(l) => l == i,
        Row::Pair(a, b) => a == i || b == i,
        Row::Hunk(_) => false,
    })
}

/// Hunk containing diff line `i`.
pub fn hunk_of_line(d: &FileDiff, i: u32) -> Option<usize> {
    d.hunks.iter().position(|h| h.lines.contains(&(i as usize)))
}

/// Where a primary press in the diff body landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// A hunk header row (its buttons handle the click).
    Header,
    /// Line `n` of the diff.
    Line(u32),
    /// Empty space below the last row.
    Empty,
}

/// Selected changed lines (indices into `FileDiff::lines`).
#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub lines: BTreeSet<u32>,
    pub anchor: Option<u32>,
}

impl Selection {
    pub fn clear(&mut self) {
        self.lines.clear();
        self.anchor = None;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn contains(&self, i: u32) -> bool {
        self.lines.contains(&i)
    }

    pub fn single(&mut self, i: u32) {
        self.lines.clear();
        self.lines.insert(i);
        self.anchor = Some(i);
    }

    pub fn toggle(&mut self, i: u32) {
        if !self.lines.remove(&i) {
            self.lines.insert(i);
        }
        self.anchor = Some(i);
    }

    /// Applies a primary press (with Shift / Ctrl state) and returns the new
    /// drag start. Presses on hunk headers leave everything untouched, so
    /// their "Stage lines" buttons still see the selection on release.
    pub fn press(
        &mut self,
        at: Press,
        shift: bool,
        command: bool,
        drag_from: Option<u32>,
        lines: &[Line],
    ) -> Option<u32> {
        let changed = |i: u32| {
            lines
                .get(i as usize)
                .is_some_and(|l| l.kind != LineKind::Context)
        };
        match at {
            Press::Header => drag_from,
            Press::Line(i) if shift => {
                let a = self.anchor.unwrap_or(i);
                self.range(a, i, lines, command);
                self.anchor = Some(a);
                Some(a)
            }
            Press::Line(i) if command => {
                if changed(i) {
                    self.toggle(i);
                }
                None
            }
            Press::Line(i) => {
                if changed(i) {
                    self.single(i);
                } else {
                    self.clear();
                    self.anchor = Some(i);
                }
                Some(i)
            }
            Press::Empty => {
                if !shift && !command {
                    self.clear();
                }
                None
            }
        }
    }

    /// Selects the changed lines between `a` and `b` (inclusive, any order);
    /// `additive` keeps what was selected before.
    pub fn range(&mut self, a: u32, b: u32, lines: &[Line], additive: bool) {
        if !additive {
            self.lines.clear();
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let hi = hi.min(lines.len().saturating_sub(1) as u32);
        for i in lo..=hi {
            if lines
                .get(i as usize)
                .is_some_and(|l| l.kind != LineKind::Context)
            {
                self.lines.insert(i);
            }
        }
    }

    /// Selected lines of hunk `h`.
    pub fn count_in_hunk(&self, d: &FileDiff, h: usize) -> usize {
        d.hunks
            .get(h)
            .map(|hk| {
                self.lines
                    .range(hk.lines.start as u32..hk.lines.end as u32)
                    .count()
            })
            .unwrap_or(0)
    }

    pub fn keys(&self, d: &FileDiff) -> Vec<LineKey> {
        self.lines
            .iter()
            .filter_map(|&i| d.lines.get(i as usize))
            .filter_map(line_key)
            .collect()
    }

    /// Selected lines as patch-like text (`+`/`-` prefixed).
    pub fn text(&self, d: &FileDiff) -> String {
        let mut s = String::new();
        for &i in &self.lines {
            if let Some(l) = d.lines.get(i as usize) {
                s.push(if l.kind == LineKind::Added { '+' } else { '-' });
                s.push_str(&d.line_text(l));
                s.push('\n');
            }
        }
        s
    }
}

/// Appends `s` with tabs expanded to `tab`-wide stops; `col` tracks the column.
pub fn expand_tabs(s: &str, col: &mut usize, tab: usize, out: &mut String) {
    let tab = tab.max(1);
    for c in s.chars() {
        if c == '\t' {
            let n = tab - (*col % tab);
            out.extend(std::iter::repeat_n(' ', n));
            *col += n;
        } else {
            out.push(c);
            *col += 1;
        }
    }
}

/// Cuts `s` at [`MAX_DISPLAY_CHARS`] characters; true when cut.
pub fn display_cut(s: &str) -> (&str, bool) {
    match s.char_indices().nth(MAX_DISPLAY_CHARS) {
        Some((i, _)) => (&s[..i], true),
        None => (s, false),
    }
}

/// Splits `[0, len)` into segments at highlight boundaries:
/// `(range, highlighted)`. `hl` must be sorted and non-overlapping.
pub fn segments(len: usize, hl: &[std::ops::Range<usize>]) -> Vec<(std::ops::Range<usize>, bool)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for r in hl {
        let (s, e) = (r.start.min(len), r.end.min(len));
        if s < pos || s >= e {
            continue;
        }
        if s > pos {
            out.push((pos..s, false));
        }
        out.push((s..e, true));
        pos = e;
    }
    if pos < len {
        out.push((pos..len, false));
    }
    out
}

/// Range of diff line indices shown by `rows` (a hunk header counts as the
/// hunk's first line); empty when there are none.
pub fn row_lines(rows: &[Row], hunks: &[gk_diff::Hunk]) -> std::ops::Range<usize> {
    let mut lo = usize::MAX;
    let mut hi = 0;
    let mut add = |i: u32| {
        if i != NONE {
            lo = lo.min(i as usize);
            hi = hi.max(i as usize + 1);
        }
    };
    for r in rows {
        match *r {
            Row::Hunk(h) => {
                if let Some(h) = hunks.get(h as usize) {
                    add(h.lines.start as u32);
                }
            }
            Row::Line(i) => add(i),
            Row::Pair(a, b) => {
                add(a);
                add(b);
            }
        }
    }
    if lo < hi { lo..hi } else { 0..0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LineKind::*;
    use gk_diff::{Options, diff};

    #[test]
    fn pairing_runs() {
        let k = [Context, Removed, Removed, Added, Context, Added, Removed];
        let p = pair_lines(&k);
        assert_eq!(
            p,
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(3)),
                (Some(2), None),
                (Some(4), Some(4)),
                (None, Some(5)),
                (Some(6), None),
            ]
        );
    }

    #[test]
    fn pairing_more_added_than_removed() {
        let p = pair_lines(&[Removed, Added, Added, Added]);
        assert_eq!(
            p,
            vec![(Some(0), Some(1)), (None, Some(2)), (None, Some(3))]
        );
    }

    #[test]
    fn layout_rows_and_partners() {
        let d = diff(b"a\nb\nc\n", b"a\nB\nc\nd\n", Options::default());
        let l = Layout::build(&d, 4);
        // hunk header + 5 lines: a, -b, +B, c, +d
        assert_eq!(l.unified.len(), 6);
        assert_eq!(l.unified[0], Row::Hunk(0));
        assert_eq!(l.unified_hunks, vec![0]);
        // side by side: header, a|a, b|B, c|c, -|d
        assert_eq!(l.sbs.len(), 5);
        assert_eq!(l.sbs[2], Row::Pair(1, 2));
        assert_eq!(l.sbs[4], Row::Pair(NONE, 4));
        assert_eq!(l.partner[1], 2);
        assert_eq!(l.partner[2], 1);
        assert_eq!(l.partner[4], NONE);
    }

    #[test]
    fn selection_range_skips_context() {
        let d = diff(b"a\nb\nc\nd\n", b"a\nB\nc\nD\n", Options::default());
        // lines: a, -b, +B, c, -d, +D
        let mut s = Selection::default();
        s.range(0, 3, &d.lines, false);
        assert_eq!(s.lines.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
        s.range(5, 4, &d.lines, true);
        assert_eq!(s.lines.len(), 4);
        s.toggle(1);
        assert!(!s.contains(1));
        s.single(5);
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.count_in_hunk(&d, 0), 1);
        assert_eq!(s.text(&d), "+D\n");
    }

    #[test]
    fn press_on_hunk_header_keeps_selection() {
        let d = diff(b"a\nb\nc\n", b"a\nB\nc\nd\n", Options::default());
        // lines: a, -b, +B, c, +d
        let mut s = Selection::default();
        let drag = s.press(Press::Line(2), false, false, None, &d.lines);
        assert_eq!(drag, Some(2));
        assert_eq!(s.lines.iter().copied().collect::<Vec<_>>(), vec![2]);
        // pressing a header (e.g. its "Stage lines (1)" button) changes nothing
        let drag = s.press(Press::Header, false, false, drag, &d.lines);
        assert_eq!(drag, Some(2));
        assert_eq!(s.lines.iter().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(s.anchor, Some(2));
        // shift extends from the anchor, ctrl toggles, context lines just anchor
        s.press(Press::Line(4), true, false, None, &d.lines);
        assert_eq!(s.lines.len(), 2);
        s.press(Press::Line(1), false, true, None, &d.lines);
        assert_eq!(s.lines.len(), 3);
        assert_eq!(
            s.press(Press::Line(0), false, false, None, &d.lines),
            Some(0)
        );
        assert!(s.is_empty() && s.anchor == Some(0));
        // empty space clears (unless a modifier is held)
        s.single(2);
        assert_eq!(s.press(Press::Empty, false, true, Some(2), &d.lines), None);
        assert!(!s.is_empty());
        s.press(Press::Empty, false, false, None, &d.lines);
        assert!(s.is_empty());
    }

    #[test]
    fn keys_find_the_same_lines_in_a_full_diff() {
        let old: String = (0..40).map(|i| format!("{i}\n")).collect();
        let new: String = (0..40)
            .map(|i| match i {
                3 => "three\n".to_string(),
                30 => "thirty\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let shown = diff(old.as_bytes(), new.as_bytes(), Options::default());
        assert_eq!(shown.hunks.len(), 2);
        let keys = hunk_keys(&shown, 1);
        assert_eq!(keys, vec![(false, 31), (true, 31)]);
        let full = diff(old.as_bytes(), new.as_bytes(), Options::full());
        let pick = picker(&keys);
        let picked: Vec<String> = full
            .lines
            .iter()
            .filter(|l| pick(l))
            .map(|l| full.line_text(l).into_owned())
            .collect();
        assert_eq!(picked, vec!["30".to_string(), "thirty".to_string()]);
        let staged = gk_diff::apply_selection(&full, pick, gk_diff::SelectMode::Apply);
        let s = String::from_utf8(staged).unwrap();
        assert!(s.contains("thirty\n") && s.contains("\n3\n") && !s.contains("three"));
    }

    #[test]
    fn tabs_and_cuts() {
        let mut out = String::new();
        let mut col = 0;
        expand_tabs("a\tb", &mut col, 4, &mut out);
        assert_eq!(out, "a   b");
        expand_tabs("\tc", &mut col, 4, &mut out);
        assert_eq!(out, "a   b   c");
        let long = "x".repeat(MAX_DISPLAY_CHARS + 5);
        let (s, cut) = display_cut(&long);
        assert!(cut && s.len() == MAX_DISPLAY_CHARS);
        assert_eq!(display_cut("short"), ("short", false));
    }

    #[test]
    fn segment_split() {
        assert_eq!(
            segments(10, &[2..4, 6..8]),
            vec![
                (0..2, false),
                (2..4, true),
                (4..6, false),
                (6..8, true),
                (8..10, false)
            ]
        );
        assert_eq!(segments(3, &[]), vec![(0..3, false)]);
        assert_eq!(
            segments(3, std::slice::from_ref(&(0..3))),
            vec![(0..3, true)]
        );
    }

    /// Two hunks: line 2 changed, line 18 changed (20 lines, 3 context).
    fn two_hunks() -> FileDiff {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l18\n", "L18\nextra\n");
        diff(old.as_bytes(), new.as_bytes(), Options::default())
    }

    #[test]
    fn keyboard_hunk_and_line_navigation() {
        let d = two_hunks();
        assert_eq!(d.hunks.len(), 2);
        let l = Layout::build(&d, 4);
        let hr = l.hunk_rows(false);
        let total = l.rows(false).len() as u32;
        // The hunk at the top of the view.
        assert_eq!(hunk_at(hr, 0), Some(0));
        assert_eq!(hunk_at(hr, hr[1]), Some(1));
        assert_eq!(hunk_at(hr, hr[1] - 1), Some(0));
        assert_eq!(hunk_at(&[], 3), None);
        // On screen: any of its rows within top..=bottom.
        assert!(hunk_on_screen(hr, total, 0, 0, 2));
        assert!(!hunk_on_screen(hr, total, 1, 0, hr[1] - 1));
        assert!(hunk_on_screen(hr, total, 1, total - 1, total + 10));
        assert!(!hunk_on_screen(hr, total, 5, 0, total));
        // Changed lines, from the start of a hunk, then stepping.
        let first = step_changed(&d, None, true, Some(1)).unwrap();
        assert_eq!(hunk_of_line(&d, first), Some(1));
        assert_ne!(d.lines[first as usize].kind, LineKind::Context);
        let next = step_changed(&d, Some(first), true, None).unwrap();
        assert!(next > first && d.lines[next as usize].kind != LineKind::Context);
        assert_eq!(step_changed(&d, Some(next), false, None), Some(first));
        let last = (d.lines.len() - 1) as u32;
        let last_changed = (0..=last)
            .rev()
            .find(|&i| d.lines[i as usize].kind != LineKind::Context)
            .unwrap();
        assert_eq!(step_changed(&d, Some(last_changed), true, None), None);
        // Display rows of lines, unified and side by side.
        let r = row_of_line(l.rows(false), first).unwrap();
        assert_eq!(l.rows(false)[r], Row::Line(first));
        assert!(row_of_line(l.rows(true), first).is_some());
        assert_eq!(row_of_line(l.rows(false), 9999), None);
    }

    #[test]
    fn row_lines_spans_visible_rows() {
        let d = diff(b"a\nb\nc\n", b"a\nB\nc\nd\n", Options::default());
        let hunks = &d.hunks;
        assert_eq!(row_lines(&[], hunks), 0..0);
        assert_eq!(row_lines(&[Row::Hunk(0)], hunks), 0..1);
        assert_eq!(row_lines(&[Row::Line(2), Row::Line(3)], hunks), 2..4);
        assert_eq!(
            row_lines(&[Row::Pair(NONE, 4), Row::Pair(1, NONE)], hunks),
            1..5
        );
        assert_eq!(row_lines(&[Row::Pair(NONE, NONE)], hunks), 0..0);
    }
}
