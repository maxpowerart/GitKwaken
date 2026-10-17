//! Syntax highlighting of loaded diffs (syntect, pure-Rust regex engine).
//!
//! The syntax and theme sets are loaded once, lazily, by the first
//! background highlight job; the UI thread never touches them. Each hunk is
//! highlighted with two parsers started at the hunk header: one over the old
//! side (removed + context lines), one over the new side (added + context
//! lines), so the parser state stays consistent within the hunk.

use super::model::display_cut;
use egui::Color32;
use gk_diff::{FileDiff, LineKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, OnceLock, Weak};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Diffs with more lines than this are not highlighted.
pub const MAX_LINES: usize = 20_000;
/// Highlight results kept per view.
const CACHE_MAX: usize = 8;

pub const DARK_THEME: &str = "base16-ocean.dark";
pub const LIGHT_THEME: &str = "InspiredGitHub";

struct Assets {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

/// Loads the syntax/theme sets on first use. Blocks; background threads only.
fn assets() -> &'static Assets {
    ASSETS.get_or_init(|| Assets {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

/// A colored byte range of a line's display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub color: Color32,
}

/// Token colors of every line of one diff. Bytes not covered by a span use
/// the default text color.
#[derive(Debug, Default)]
pub struct Highlights {
    /// Old-side text (removed and context lines).
    old: Vec<Box<[Span]>>,
    /// New-side text (added and context lines).
    new: Vec<Box<[Span]>>,
}

impl Highlights {
    /// Spans of line `i` as displayed: removed lines always show the old
    /// text, context lines the old text only on the left side-by-side half.
    pub fn spans(&self, i: usize, kind: LineKind, old_side: bool) -> &[Span] {
        let old = kind == LineKind::Removed || (kind == LineKind::Context && old_side);
        let v = if old { &self.old } else { &self.new };
        v.get(i).map(|b| &**b).unwrap_or(&[])
    }
}

/// Syntax for `path` (extension, then whole file name, then the first line).
/// `None` for plain text and unknown files.
pub fn find_syntax<'a>(
    ss: &'a SyntaxSet,
    path: &str,
    first_line: Option<&str>,
) -> Option<&'a SyntaxReference> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let by_ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty())
        .and_then(|e| {
            ss.find_syntax_by_extension(e)
                .or_else(|| ss.find_syntax_by_extension(&e.to_ascii_lowercase()))
        });
    by_ext
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| first_line.and_then(|l| ss.find_syntax_by_first_line(l)))
        .filter(|s| s.name != "Plain Text")
}

fn theme(a: &Assets, dark: bool) -> Option<&Theme> {
    a.themes
        .themes
        .get(if dark { DARK_THEME } else { LIGHT_THEME })
}

/// Highlights `d` (blocking). `None` when the file type is unknown, the
/// diff is too large or `cancel` was set.
pub fn highlight_diff(
    d: &FileDiff,
    path: &str,
    dark: bool,
    cancel: &AtomicBool,
) -> Option<Highlights> {
    if d.binary || d.lines.is_empty() || d.lines.len() > MAX_LINES {
        return None;
    }
    let a = assets();
    // The first line is only known when the diff starts at the top of the file.
    let first = d
        .lines
        .first()
        .filter(|l| l.old_no <= 1 && l.new_no <= 1)
        .map(|l| super::line_text(d, l, l.kind == LineKind::Removed));
    let syntax = find_syntax(&a.syntaxes, path, first.as_deref())?;
    let theme = theme(a, dark)?;
    let default_fg = theme.settings.foreground;
    let n = d.lines.len();
    let mut out = Highlights {
        old: vec![Box::default(); n],
        new: vec![Box::default(); n],
    };
    let mut buf = String::new();
    for hunk in &d.hunks {
        let mut old_hl = HighlightLines::new(syntax, theme);
        let mut new_hl = HighlightLines::new(syntax, theme);
        for i in hunk.lines.clone() {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Some(l) = d.lines.get(i) else { continue };
            if l.kind != LineKind::Added {
                let t = super::line_text(d, l, true);
                out.old[i] = line_spans(&mut old_hl, &a.syntaxes, &t, default_fg, &mut buf);
            }
            if l.kind != LineKind::Removed {
                let t = super::line_text(d, l, false);
                out.new[i] = line_spans(&mut new_hl, &a.syntaxes, &t, default_fg, &mut buf);
            }
        }
    }
    Some(out)
}

/// Runs one line through the parser. Over-long lines are not colored (the
/// parser still sees an empty line so the line count stays in step).
fn line_spans(
    hl: &mut HighlightLines<'_>,
    ss: &SyntaxSet,
    text: &str,
    default_fg: Option<syntect::highlighting::Color>,
    buf: &mut String,
) -> Box<[Span]> {
    buf.clear();
    let long = display_cut(text).1;
    if !long {
        buf.push_str(text);
    }
    buf.push('\n');
    let Ok(ranges) = hl.highlight_line(buf, ss) else {
        return Box::default();
    };
    if long {
        return Box::default();
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut pos = 0usize;
    for (style, piece) in ranges {
        let start = pos;
        pos += piece.len();
        let end = pos.min(text.len());
        let fg = style.foreground;
        if start >= end || Some(fg) == default_fg {
            continue;
        }
        let color = Color32::from_rgb(fg.r, fg.g, fg.b);
        match spans.last_mut() {
            Some(s) if s.end as usize == start && s.color == color => s.end = end as u32,
            _ => spans.push(Span {
                start: start as u32,
                end: end as u32,
                color,
            }),
        }
    }
    spans.into_boxed_slice()
}

/// A running highlight job: its result channel and cancel flag (set on drop).
pub struct Job {
    pub rx: Receiver<Option<Highlights>>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

enum State {
    Running(Job),
    /// `None`: not highlightable (unknown type, too large, failed).
    Done(Option<Arc<Highlights>>),
}

struct Entry {
    diff: Weak<FileDiff>,
    dark: bool,
    state: State,
}

/// Highlight results of recently shown diffs, keyed by the `Arc<FileDiff>`
/// pointer and the theme. Holds weak references only, so a diff dropped by
/// the session frees its entry (and the pointer cannot be reused while the
/// entry exists).
#[derive(Default)]
pub struct HlCache {
    entries: Vec<Entry>,
}

impl HlCache {
    /// Result for `diff` + theme. Starts a job via `start` when there is no
    /// entry yet; `None` while it runs or when the diff is not highlightable.
    pub fn get_or_start(
        &mut self,
        diff: &Arc<FileDiff>,
        dark: bool,
        start: impl FnOnce() -> Job,
    ) -> Option<Arc<Highlights>> {
        self.entries.retain(|e| e.diff.strong_count() > 0);
        let pos = self
            .entries
            .iter()
            .position(|e| e.dark == dark && std::ptr::eq(e.diff.as_ptr(), Arc::as_ptr(diff)));
        let i = match pos {
            Some(i) => i,
            None => {
                if self.entries.len() >= CACHE_MAX {
                    self.entries.remove(0);
                }
                self.entries.push(Entry {
                    diff: Arc::downgrade(diff),
                    dark,
                    state: State::Running(start()),
                });
                self.entries.len() - 1
            }
        };
        let e = &mut self.entries[i];
        if let State::Running(job) = &e.state {
            match job.rx.try_recv() {
                Ok(r) => e.state = State::Done(r.map(Arc::new)),
                Err(TryRecvError::Disconnected) => e.state = State::Done(None),
                Err(TryRecvError::Empty) => {}
            }
        }
        match &e.state {
            State::Done(r) => r.clone(),
            State::Running(_) => None,
        }
    }

    /// The view swapped `from` for an equal diff `to`: keep its results.
    pub fn rekey(&mut self, from: &Arc<FileDiff>, to: &Arc<FileDiff>) {
        self.entries
            .retain(|e| !std::ptr::eq(e.diff.as_ptr(), Arc::as_ptr(to)));
        for e in &mut self.entries {
            if std::ptr::eq(e.diff.as_ptr(), Arc::as_ptr(from)) {
                e.diff = Arc::downgrade(to);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Spawns a background highlight job for `diff`; requests a repaint when done.
pub fn spawn(diff: Arc<FileDiff>, path: String, dark: bool, ctx: egui::Context) -> Job {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    // If the thread cannot start, the sender is dropped and the entry ends as Done(None).
    let _ = std::thread::Builder::new()
        .name("gk-syntax".into())
        .spawn(move || {
            let r = highlight_diff(&diff, &path, dark, &flag);
            if !flag.load(Ordering::Relaxed) {
                let _ = tx.send(r);
                ctx.request_repaint();
            }
        });
    Job { rx, cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_diff::{Options, diff};

    fn ss() -> &'static SyntaxSet {
        &assets().syntaxes
    }

    #[test]
    fn syntax_by_extension_name_and_first_line() {
        let name = |p: &str, f: Option<&str>| find_syntax(ss(), p, f).map(|s| s.name.clone());
        assert_eq!(name("src/main.rs", None).as_deref(), Some("Rust"));
        assert_eq!(name("a/b/tool.PY", None).as_deref(), Some("Python"));
        assert_eq!(name("Makefile", None).as_deref(), Some("Makefile"));
        assert_eq!(name("web/x.js", None).as_deref(), Some("JavaScript"));
        assert!(
            name("bin/run", Some("#!/bin/bash"))
                .unwrap()
                .contains("bash")
        );
        assert_eq!(name("bin/run", None), None);
        assert_eq!(name("notes.txt", None), None);
        assert_eq!(name("data.unknownext", None), None);
    }

    #[test]
    fn highlights_both_sides_of_a_hunk() {
        let old = b"fn main() {\n    let a = 1;\n}\n";
        let new = b"fn main() {\n    let b = \"two\";\n}\n";
        let d = diff(old, new, Options::default());
        let h = highlight_diff(&d, "x.rs", true, &AtomicBool::new(false)).unwrap();
        // lines: fn main, -let a, +let b, }
        assert_eq!(d.lines[1].kind, LineKind::Removed);
        assert_eq!(d.lines[2].kind, LineKind::Added);
        // "fn" keyword colored on both sides of the context line
        for side in [true, false] {
            let s = h.spans(0, LineKind::Context, side);
            assert!(s.iter().any(|s| s.start == 0 && s.end >= 2), "{s:?}");
        }
        assert!(!h.spans(1, LineKind::Removed, false).is_empty());
        // the string literal on the added line has its own color
        let t = d.line_text(&d.lines[2]);
        let q = t.find('"').unwrap() as u32 + 1;
        let added = h.spans(2, LineKind::Added, false);
        assert!(
            added.iter().any(|s| s.start <= q && s.end > q),
            "{t:?} {added:?}"
        );
        // spans are sorted, non-overlapping and inside the text
        assert!(added.windows(2).all(|w| w[0].end <= w[1].start));
        assert!(added.iter().all(|s| s.end as usize <= t.len()));
        // light theme works too; unknown types and cancelled jobs give nothing
        assert!(highlight_diff(&d, "x.rs", false, &AtomicBool::new(false)).is_some());
        assert!(highlight_diff(&d, "x.unknownext", true, &AtomicBool::new(false)).is_none());
        assert!(highlight_diff(&d, "x.rs", true, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn long_lines_and_huge_diffs_are_skipped() {
        let long = format!("let s = \"{}\";\n", "x".repeat(3000));
        let new = format!("fn a() {{}}\n{long}");
        let d = diff(b"", new.as_bytes(), Options::default());
        let h = highlight_diff(&d, "x.rs", true, &AtomicBool::new(false)).unwrap();
        assert!(!h.spans(0, LineKind::Added, false).is_empty());
        assert!(h.spans(1, LineKind::Added, false).is_empty());

        let big: String = (0..MAX_LINES + 1).map(|i| format!("{i}\n")).collect();
        let d = diff(b"", big.as_bytes(), Options::default());
        assert!(highlight_diff(&d, "x.rs", true, &AtomicBool::new(false)).is_none());
    }

    fn job() -> (std::sync::mpsc::Sender<Option<Highlights>>, Job) {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        (tx, Job { rx, cancel })
    }

    #[test]
    fn cache_keys_by_diff_pointer_and_theme() {
        let mut c = HlCache::default();
        let a = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        let (tx, j) = job();
        let cancel = j.cancel.clone();
        let mut started = 0;
        let mut get = |c: &mut HlCache, d: &Arc<FileDiff>, dark: bool, j: Option<Job>| {
            c.get_or_start(d, dark, || {
                started += 1;
                j.unwrap_or_else(|| job().1)
            })
        };
        // running: nothing yet, no second job for the same key
        assert!(get(&mut c, &a, true, Some(j)).is_none());
        assert!(get(&mut c, &a, true, None).is_none());
        assert!(
            !cancel.load(Ordering::Relaxed),
            "a running job is not cancelled"
        );
        tx.send(Some(Highlights::default())).unwrap();
        let r1 = get(&mut c, &a, true, None).unwrap();
        let r2 = get(&mut c, &a, true, None).unwrap();
        assert!(Arc::ptr_eq(&r1, &r2));
        // other theme: separate entry; an equal but distinct Arc is a new key
        assert!(get(&mut c, &a, false, None).is_none());
        let b = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        assert!(get(&mut c, &b, true, None).is_none());
        assert_eq!(c.len(), 3);
        drop(get);
        assert_eq!(started, 3);

        // rekey keeps the result for the replacement Arc
        c.rekey(&a, &b);
        drop(a);
        let r3 = c.get_or_start(&b, true, || unreachable!()).unwrap();
        assert!(Arc::ptr_eq(&r1, &r3));
    }

    #[test]
    fn cache_drops_dead_diffs_and_cancels_evicted_jobs() {
        let mut c = HlCache::default();
        let (_tx, j) = job();
        let cancel = j.cancel.clone();
        let first = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        let mut j = Some(j);
        c.get_or_start(&first, true, || j.take().unwrap());
        let keep: Vec<Arc<FileDiff>> = (0..CACHE_MAX)
            .map(|_| Arc::new(diff(b"a\n", b"b\n", Options::default())))
            .collect();
        for d in &keep {
            c.get_or_start(d, true, || job().1);
        }
        assert_eq!(c.len(), CACHE_MAX);
        assert!(cancel.load(Ordering::Relaxed), "evicted job is cancelled");
        // a finished job whose sender vanished counts as "not highlightable"
        drop(keep);
        let d = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        let (tx, j) = job();
        drop(tx);
        let mut j = Some(j);
        assert!(c.get_or_start(&d, true, || j.take().unwrap()).is_none());
        assert_eq!(c.len(), 1, "entries of dropped diffs are pruned");
        assert!(c.get_or_start(&d, true, || unreachable!()).is_none());
    }
}
