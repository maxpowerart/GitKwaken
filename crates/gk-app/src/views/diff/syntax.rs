//! Syntax highlighting of the visible part of loaded diffs (syntect,
//! pure-Rust regex engine).
//!
//! The syntax and theme sets are loaded once, lazily, by the first
//! background job; the UI thread never touches them. Each hunk is
//! highlighted with two parsers started at the hunk header: one over the old
//! side (removed + context lines), one over the new side (added + context
//! lines), so the parser state stays consistent within the hunk.
//!
//! Only what is on screen is highlighted. The diff body reports its visible
//! line range every frame ([`HlCache::get`]); a background worker per diff
//! highlights the windows of [`WINDOW`] lines that intersect it (plus
//! [`MARGIN`] lines either side) and hands the spans back in batches, which
//! the cache merges into the diff's [`Highlights`]. A window that starts in
//! the middle of a hunk resumes from the parser state saved at the start of
//! the window ([`Checkpoint`]); a window without one is reached by parsing
//! forward (without styling) from the nearest earlier checkpoint, saving
//! every window boundary on the way.

use super::model::display_cut;
use egui::Color32;
use gk_diff::{FileDiff, LineKind};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use syntect::highlighting::{
    Color, HighlightState, Highlighter, RangedHighlightIterator, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines per unit of work, and the distance between parser checkpoints
/// inside a hunk.
pub const WINDOW: usize = 128;
/// Lines highlighted above and below the visible ones (smooth scrolling).
pub const MARGIN: usize = 200;
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

/// Token colors of the highlighted lines of one diff. Bytes not covered by
/// a span, and lines not highlighted (yet), use the default text color.
#[derive(Debug, Default, Clone)]
pub struct Highlights {
    /// Old-side text (removed and context lines).
    old: Vec<Box<[Span]>>,
    /// New-side text (added and context lines).
    new: Vec<Box<[Span]>>,
    /// Lines whose spans are known.
    have: Vec<bool>,
}

impl Highlights {
    /// Spans of line `i` as displayed: removed lines always show the old
    /// text, context lines the old text only on the left side-by-side half.
    pub fn spans(&self, i: usize, kind: LineKind, old_side: bool) -> &[Span] {
        let old = kind == LineKind::Removed || (kind == LineKind::Context && old_side);
        let v = if old { &self.old } else { &self.new };
        v.get(i).map(|b| &**b).unwrap_or(&[])
    }

    /// Every line of `r` is highlighted.
    pub fn covers(&self, r: Range<usize>) -> bool {
        self.have.get(r).is_some_and(|h| h.iter().all(|&b| b))
    }

    /// Takes a worker's result for lines `c.start..` of an `n`-line diff.
    fn merge(&mut self, c: Chunk, n: usize) {
        if self.have.len() < n {
            self.old.resize(n, Box::default());
            self.new.resize(n, Box::default());
            self.have.resize(n, false);
        }
        let end = (c.start + c.old.len()).min(n);
        for (i, (o, w)) in (c.start..end).zip(c.old.into_iter().zip(c.new)) {
            self.old[i] = o;
            self.new[i] = w;
            self.have[i] = true;
        }
    }
}

/// Spans of lines `start..start + old.len()`.
#[derive(Debug)]
struct Chunk {
    start: usize,
    old: Vec<Box<[Span]>>,
    new: Vec<Box<[Span]>>,
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

/// Loads the syntax set and the syntax of `path` on a background thread
/// (syntect links a syntax on its first use, ~20 ms), so the first
/// highlight of a diff does not wait for it. Call while the diff loads;
/// cheap after the first call per file type.
pub fn warm(path: &str) {
    static WARMED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let name = path.rsplit('/').next().unwrap_or(path);
    let key = name.rsplit_once('.').map_or(name, |(_, e)| e);
    {
        let mut w = WARMED.lock().unwrap_or_else(|e| e.into_inner());
        if w.iter().any(|k| k == key) {
            return;
        }
        w.push(key.to_string());
    }
    let path = path.to_string();
    let _ = std::thread::Builder::new()
        .name("gk-syntax-warm".into())
        .spawn(move || {
            let a = assets();
            if let Some(s) = find_syntax(&a.syntaxes, &path, None) {
                let _ = ParseState::new(s).parse_line("\n", &a.syntaxes);
            }
        });
}

fn theme(a: &Assets, dark: bool) -> Option<&Theme> {
    a.themes
        .themes
        .get(if dark { DARK_THEME } else { LIGHT_THEME })
}

/// Syntax of a diff (the first line is only known when the diff starts at
/// the top of the file).
fn diff_syntax(a: &'static Assets, d: &FileDiff, path: &str) -> Option<&'static SyntaxReference> {
    let first = d
        .lines
        .first()
        .filter(|l| l.old_no <= 1 && l.new_no <= 1)
        .map(|l| super::line_text(d, l, l.kind == LineKind::Removed));
    find_syntax(&a.syntaxes, path, first.as_deref())
}

/// Parser state of one side of a hunk.
#[derive(Clone)]
struct Side {
    parse: ParseState,
    /// Scope stack the highlighter is in (rebuilds its styles exactly).
    stack: ScopeStack,
}

/// Parser state of both sides at the start of a window.
#[derive(Clone)]
struct Checkpoint {
    old: Side,
    new: Side,
}

/// Progress through one hunk: `cps[k]` is the state at the start of window
/// `k` (window 0 always has one), `done[k]` whether its spans were produced,
/// `prov[k]` whether provisional spans were.
#[derive(Default)]
struct HunkState {
    cps: Vec<Option<Checkpoint>>,
    done: Vec<bool>,
    prov: Vec<bool>,
}

impl HunkState {
    /// Nearest window at or before `k` with a checkpoint.
    fn nearest(&self, k: usize) -> usize {
        (0..=k).rev().find(|&j| self.cps[j].is_some()).unwrap_or(0)
    }
}

/// Windows of a hunk spanning `lines`.
fn windows(lines: &Range<usize>) -> usize {
    lines.len().div_ceil(WINDOW)
}

/// Lines of window `k` of a hunk spanning `lines`.
fn window(lines: &Range<usize>, k: usize) -> Range<usize> {
    let s = lines.start + k * WINDOW;
    s..(s + WINDOW).min(lines.end)
}

/// Windows of hunks intersecting `r`, top down: `(hunk, window)`.
fn windows_in(d: &FileDiff, r: Range<usize>) -> impl Iterator<Item = (usize, usize)> + '_ {
    let first = d.hunks.partition_point(|h| h.lines.end <= r.start);
    d.hunks[first..]
        .iter()
        .enumerate()
        .take_while(move |(_, hunk)| hunk.lines.start < r.end)
        .flat_map(move |(i, hunk)| {
            let lo = r.start.max(hunk.lines.start);
            let hi = r.end.min(hunk.lines.end);
            let ks = if lo < hi {
                (lo - hunk.lines.start) / WINDOW..(hi - 1 - hunk.lines.start) / WINDOW + 1
            } else {
                0..0
            };
            ks.map(move |k| (first + i, k))
        })
}

/// A window this many windows past its nearest checkpoint first gets
/// provisional spans (parsed from a fresh state) while the exact parse
/// catches up, so a jump into a huge hunk shows colors at once.
const FAR_WINDOWS: usize = 3;

/// Highlighting of one diff: resolved syntax and theme, progress per hunk.
/// Lives on the worker thread while it runs, parked in [`Ctl`] in between.
struct Engine {
    syntax: &'static SyntaxReference,
    hl: Highlighter<'static>,
    default_fg: Option<Color>,
    hunks: Vec<HunkState>,
}

impl Engine {
    /// `None` when `d` has no known syntax (or the theme is missing).
    fn new(d: &FileDiff, path: &str, dark: bool) -> Option<Engine> {
        let a = assets();
        let syntax = diff_syntax(a, d, path)?;
        let theme = theme(a, dark)?;
        Some(Engine {
            syntax,
            hl: Highlighter::new(theme),
            default_fg: theme.settings.foreground,
            hunks: (0..d.hunks.len()).map(|_| HunkState::default()).collect(),
        })
    }

    fn fresh(&self) -> Checkpoint {
        let side = Side {
            parse: ParseState::new(self.syntax),
            stack: ScopeStack::new(),
        };
        Checkpoint {
            old: side.clone(),
            new: side,
        }
    }

    /// First window not highlighted yet that intersects one of `ranges`
    /// (searched in order, top down): `(hunk, window)`.
    fn next(&self, d: &FileDiff, ranges: &[Range<usize>]) -> Option<(usize, usize)> {
        ranges.iter().find_map(|r| {
            windows_in(d, r.clone()).find(|&(h, k)| {
                !self
                    .hunks
                    .get(h)
                    .and_then(|s| s.done.get(k))
                    .copied()
                    .unwrap_or(false)
            })
        })
    }

    /// Progress of hunk `h`, set up on first use.
    fn state(&mut self, d: &FileDiff, h: usize) -> Option<&mut HunkState> {
        let n = windows(&d.hunks.get(h)?.lines);
        let fresh = self.fresh();
        let st = self.hunks.get_mut(h)?;
        if st.cps.len() != n {
            st.cps = vec![None; n];
            st.cps[0] = Some(fresh);
            st.done = vec![false; n];
            st.prov = vec![false; n];
        }
        Some(st)
    }

    /// Provisional spans for the windows intersecting `r` that are far from
    /// any checkpoint and have neither exact nor provisional spans yet.
    /// `None` when stopped.
    fn provisional(
        &mut self,
        d: &FileDiff,
        r: Range<usize>,
        stop: &dyn Fn() -> bool,
    ) -> Option<Vec<Chunk>> {
        let mut out = Vec::new();
        for (h, k) in windows_in(d, r) {
            {
                let Some(st) = self.state(d, h) else { continue };
                if st.done[k] || st.prov[k] || k - st.nearest(k) < FAR_WINDOWS {
                    continue;
                }
                st.prov[k] = true;
            }
            let mut cp = self.fresh();
            out.push(self.highlight(d, window(&d.hunks[h].lines, k), &mut cp, stop)?);
        }
        Some(out)
    }

    /// Highlights window `k` of hunk `h`, parsing forward from the nearest
    /// earlier checkpoint first. `None` when `stop` said so, or when
    /// `replan` did at a window boundary on the way (the checkpoints made so
    /// far are kept).
    fn run(
        &mut self,
        d: &FileDiff,
        h: usize,
        k: usize,
        stop: &dyn Fn() -> bool,
        replan: &dyn Fn() -> bool,
    ) -> Option<Chunk> {
        let hunk = d.hunks.get(h)?;
        if k >= windows(&hunk.lines) {
            return None;
        }
        let mut st = std::mem::take(self.state(d, h)?);
        let from = st.nearest(k);
        let mut cp = st.cps[from].clone().unwrap_or_else(|| self.fresh());
        let mut out = None;
        let mut reached = true;
        for w in from..k {
            if !self.skip(d, window(&hunk.lines, w), &mut cp, stop) {
                reached = false;
                break;
            }
            st.cps[w + 1] = Some(cp.clone());
            if w + 1 < k && replan() {
                reached = false;
                break;
            }
        }
        if reached && let Some(c) = self.highlight(d, window(&hunk.lines, k), &mut cp, stop) {
            if k + 1 < st.cps.len() {
                st.cps[k + 1] = Some(cp);
            }
            st.done[k] = true;
            out = Some(c);
        }
        self.hunks[h] = st;
        out
    }

    /// Advances `cp` over `lines` without styling. `false` when stopped.
    fn skip(
        &self,
        d: &FileDiff,
        lines: Range<usize>,
        cp: &mut Checkpoint,
        stop: &dyn Fn() -> bool,
    ) -> bool {
        let ss = &assets().syntaxes;
        let mut buf = String::new();
        for i in lines {
            if stop() {
                return false;
            }
            let Some(l) = d.lines.get(i) else { continue };
            for (side, old) in [(&mut cp.old, true), (&mut cp.new, false)] {
                if l.kind
                    == if old {
                        LineKind::Added
                    } else {
                        LineKind::Removed
                    }
                {
                    continue;
                }
                parser_input(&super::line_text(d, l, old), &mut buf);
                let Ok(ops) = side.parse.parse_line(&buf, ss) else {
                    continue;
                };
                // Same as the highlighter: stop at the first bad op.
                for (_, op) in &ops {
                    if side.stack.apply(op).is_err() {
                        break;
                    }
                }
            }
        }
        true
    }

    /// Spans of `lines`, advancing `cp`. `None` when cancelled.
    fn highlight(
        &self,
        d: &FileDiff,
        lines: Range<usize>,
        cp: &mut Checkpoint,
        stop: &dyn Fn() -> bool,
    ) -> Option<Chunk> {
        let ss = &assets().syntaxes;
        let mut old_hs = HighlightState::new(&self.hl, cp.old.stack.clone());
        let mut new_hs = HighlightState::new(&self.hl, cp.new.stack.clone());
        let mut c = Chunk {
            start: lines.start,
            old: Vec::with_capacity(lines.len()),
            new: Vec::with_capacity(lines.len()),
        };
        let mut buf = String::new();
        for i in lines {
            if stop() {
                return None;
            }
            let (mut o, mut w) = (Box::default(), Box::default());
            if let Some(l) = d.lines.get(i) {
                if l.kind != LineKind::Added {
                    let t = super::line_text(d, l, true);
                    o = self.line_spans(&mut cp.old.parse, &mut old_hs, ss, &t, &mut buf);
                }
                if l.kind != LineKind::Removed {
                    let t = super::line_text(d, l, false);
                    w = self.line_spans(&mut cp.new.parse, &mut new_hs, ss, &t, &mut buf);
                }
            }
            c.old.push(o);
            c.new.push(w);
        }
        cp.old.stack = old_hs.path;
        cp.new.stack = new_hs.path;
        Some(c)
    }

    /// Runs one line through the parser. Over-long lines are not colored
    /// (the parser still sees an empty line so the line count stays in step).
    fn line_spans(
        &self,
        parse: &mut ParseState,
        hs: &mut HighlightState,
        ss: &SyntaxSet,
        text: &str,
        buf: &mut String,
    ) -> Box<[Span]> {
        let long = parser_input(text, buf);
        let Ok(ops) = parse.parse_line(buf, ss) else {
            return Box::default();
        };
        let ranges = RangedHighlightIterator::new(hs, &ops, buf, &self.hl);
        if long {
            ranges.for_each(drop);
            return Box::default();
        }
        let mut spans: Vec<Span> = Vec::new();
        for (style, _, r) in ranges {
            let (start, end) = (r.start, r.end.min(text.len()));
            let fg = style.foreground;
            if start >= end || Some(fg) == self.default_fg {
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
}

/// The text the parser sees for a line: the line plus `\n`, or only `\n`
/// for over-long lines. Returns whether the line is over-long.
fn parser_input(text: &str, buf: &mut String) -> bool {
    buf.clear();
    let long = display_cut(text).1;
    if !long {
        buf.push_str(text);
    }
    buf.push('\n');
    long
}

/// What the UI wants and what the worker produced, for one diff.
struct Ctl {
    /// Visible lines (highlighted first) and visible ± [`MARGIN`].
    visible: Range<usize>,
    wanted: Range<usize>,
    /// Bumped when the ranges change; `idle_gen` is the last generation a
    /// worker finished completely.
    generation: u64,
    idle_gen: u64,
    running: bool,
    /// The file type has no syntax: nothing to do, ever.
    unsupported: bool,
    /// Parked between worker runs (`None` before the first one).
    engine: Option<Box<Engine>>,
    /// Results not merged by the UI yet.
    ready: Vec<Chunk>,
}

struct Work {
    ctl: Mutex<Ctl>,
    cancel: AtomicBool,
}

impl Work {
    fn lock(&self) -> MutexGuard<'_, Ctl> {
        self.ctl.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The background work of one diff; cancelled when dropped.
struct Job(Arc<Work>);

impl Job {
    fn new() -> Job {
        Job(Arc::new(Work {
            ctl: Mutex::new(Ctl {
                visible: 0..0,
                wanted: 0..0,
                generation: 1,
                idle_gen: 0,
                running: false,
                unsupported: false,
                engine: None,
                ready: Vec::new(),
            }),
            cancel: AtomicBool::new(false),
        }))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Relaxed);
    }
}

/// Worker thread body: highlights the windows the UI wants until there are
/// none left, then parks the engine and exits. Stops early when the job is
/// cancelled or nobody else holds the diff any more (it was replaced).
fn work(w: Arc<Work>, diff: Weak<FileDiff>, path: String, dark: bool, ctx: Option<egui::Context>) {
    let repaint = || {
        if let Some(ctx) = &ctx {
            ctx.request_repaint();
        }
    };
    let mut engine = w.lock().engine.take();
    // Every exit parks the engine and clears `running` under the lock, so
    // the UI starts a new worker whenever there is work left.
    while let Some(d) = diff.upgrade().filter(|_| !w.cancel.load(Ordering::Relaxed)) {
        let stop = || w.cancel.load(Ordering::Relaxed) || Arc::strong_count(&d) == 1;
        if engine.is_none() {
            engine = Engine::new(&d, &path, dark).map(Box::new);
            if engine.is_none() {
                w.lock().unsupported = true;
                repaint();
                break;
            }
        }
        let Some(e) = engine.as_mut() else { break };
        let (vis, want, generation) = {
            let c = w.lock();
            (c.visible.clone(), c.wanted.clone(), c.generation)
        };
        // Visible windows far into a huge hunk: approximate colors first.
        match e.provisional(&d, vis.clone(), &stop) {
            Some(p) if !p.is_empty() => {
                w.lock().ready.extend(p);
                repaint();
            }
            Some(_) => {}
            None => break,
        }
        // Long forward parses yield to a scroll at window boundaries.
        let replan = || w.lock().generation != generation;
        // A batch of about WINDOW lines per hand-over; visible lines go in a
        // batch of their own so they show without waiting for the margin.
        let ranges = if e.next(&d, std::slice::from_ref(&vis)).is_some() {
            vec![vis]
        } else {
            vec![want]
        };
        let mut batch = Vec::new();
        let mut lines = 0;
        while lines < WINDOW {
            let Some((h, k)) = e.next(&d, &ranges) else {
                break;
            };
            let Some(c) = e.run(&d, h, k, &stop, &replan) else {
                break;
            };
            lines += c.old.len();
            batch.push(c);
        }
        let stopped = stop();
        drop(d);
        let mut c = w.lock();
        if !batch.is_empty() {
            c.ready.append(&mut batch);
            drop(c);
            repaint();
        } else if c.generation == generation && !stopped {
            // Done with what is wanted; parked atomically with that check.
            return park(&mut c, engine, Some(generation));
        }
        if stopped {
            break;
        }
    }
    park(&mut w.lock(), engine, None);
}

fn park(c: &mut Ctl, engine: Option<Box<Engine>>, idle: Option<u64>) {
    if engine.is_some() {
        c.engine = engine;
    }
    if let Some(g) = idle {
        c.idle_gen = g;
    }
    c.running = false;
}

struct Entry {
    diff: Weak<FileDiff>,
    dark: bool,
    hl: Arc<Highlights>,
    job: Job,
}

/// Highlight results of recently shown diffs, keyed by the `Arc<FileDiff>`
/// pointer and the theme. Holds weak references only, so a diff dropped by
/// the session frees its entry (and the pointer cannot be reused while the
/// entry exists). Each entry has its own worker, so results of an old diff
/// can never land in a newer one's entry.
#[derive(Default)]
pub struct HlCache {
    entries: Vec<Entry>,
}

impl HlCache {
    /// Highlights of `diff` + theme so far, and asks for the lines `visible`
    /// (plus [`MARGIN`]) to be highlighted in the background; `ctx` is
    /// repainted when a batch lands. `None` when the diff is not
    /// highlightable (binary, empty, unknown file type).
    pub fn get(
        &mut self,
        diff: &Arc<FileDiff>,
        dark: bool,
        path: &str,
        visible: Range<usize>,
        ctx: Option<&egui::Context>,
    ) -> Option<Arc<Highlights>> {
        if diff.binary || diff.lines.is_empty() {
            return None;
        }
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
                    hl: Arc::default(),
                    job: Job::new(),
                });
                self.entries.len() - 1
            }
        };
        let n = diff.lines.len();
        let visible = visible.start.min(n)..visible.end.min(n);
        let wanted = visible.start.saturating_sub(MARGIN)..(visible.end + MARGIN).min(n);
        let e = &mut self.entries[i];
        let (ready, spawn, unsupported) = {
            let mut c = e.job.0.lock();
            if c.visible != visible || c.wanted != wanted {
                c.visible = visible;
                c.wanted = wanted;
                c.generation += 1;
            }
            let spawn = !c.running && !c.unsupported && c.idle_gen != c.generation;
            c.running |= spawn;
            (std::mem::take(&mut c.ready), spawn, c.unsupported)
        };
        if !ready.is_empty() {
            let h = Arc::make_mut(&mut e.hl);
            for c in ready {
                h.merge(c, n);
            }
        }
        if spawn {
            let (w, d, path, ctx) = (
                e.job.0.clone(),
                e.diff.clone(),
                path.to_string(),
                ctx.cloned(),
            );
            let started = std::thread::Builder::new()
                .name("gk-syntax".into())
                .spawn(move || work(w, d, path, dark, ctx));
            if started.is_err() {
                e.job.0.lock().running = false;
            }
        }
        (!unsupported).then(|| e.hl.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use gk_diff::{Options, diff};
    use std::time::{Duration, Instant};
    use syntect::easy::HighlightLines;

    fn ss() -> &'static SyntaxSet {
        &assets().syntaxes
    }

    /// Reference: the whole diff highlighted sequentially, hunk by hunk,
    /// with syntect's line highlighter (the pre-viewport implementation).
    fn sequential(d: &FileDiff, path: &str, dark: bool) -> Option<Highlights> {
        let a = assets();
        let syntax = diff_syntax(a, d, path)?;
        let theme = theme(a, dark)?;
        let default_fg = theme.settings.foreground;
        let n = d.lines.len();
        let mut out = Highlights {
            old: vec![Box::default(); n],
            new: vec![Box::default(); n],
            have: vec![true; n],
        };
        let mut buf = String::new();
        let mut spans = |hl: &mut HighlightLines<'_>, text: &str| -> Box<[Span]> {
            let long = parser_input(text, &mut buf);
            let Ok(ranges) = hl.highlight_line(&buf, ss()) else {
                return Box::default();
            };
            if long {
                return Box::default();
            }
            let mut v: Vec<Span> = Vec::new();
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
                match v.last_mut() {
                    Some(s) if s.end as usize == start && s.color == color => s.end = end as u32,
                    _ => v.push(Span {
                        start: start as u32,
                        end: end as u32,
                        color,
                    }),
                }
            }
            v.into_boxed_slice()
        };
        for hunk in &d.hunks {
            let mut old_hl = HighlightLines::new(syntax, theme);
            let mut new_hl = HighlightLines::new(syntax, theme);
            for i in hunk.lines.clone() {
                let l = &d.lines[i];
                if l.kind != LineKind::Added {
                    out.old[i] = spans(&mut old_hl, &super::super::line_text(d, l, true));
                }
                if l.kind != LineKind::Removed {
                    out.new[i] = spans(&mut new_hl, &super::super::line_text(d, l, false));
                }
            }
        }
        Some(out)
    }

    /// Highlights every window intersecting `r`, in the given window order.
    fn fill(e: &mut Engine, d: &FileDiff, r: Range<usize>, out: &mut Highlights) {
        while let Some((h, k)) = e.next(d, std::slice::from_ref(&r)) {
            let c = e.run(d, h, k, &|| false, &|| false).unwrap();
            out.merge(c, d.lines.len());
        }
    }

    fn assert_same(a: &Highlights, b: &Highlights, d: &FileDiff, r: Range<usize>) {
        for i in r {
            let k = d.lines[i].kind;
            for side in [true, false] {
                assert_eq!(
                    a.spans(i, k, side),
                    b.spans(i, k, side),
                    "line {i} old_side={side}"
                );
            }
        }
    }

    /// Rust source whose block comments and raw strings span many lines,
    /// so a window parsed from the wrong state would color differently.
    fn rust_source(n: usize, tag: &str) -> String {
        let mut s = String::new();
        let mut i = 0;
        while i < n {
            match i % 97 {
                0 => s.push_str("/* a block comment\n"),
                40 => s.push_str("   ends here */ fn f() {}\n"),
                50 => s.push_str("const S: &str = r#\"raw\n"),
                90 => s.push_str("still raw \"# ;\n"),
                _ => s.push_str(&format!("let v{i} = \"{tag}\" + {i}; // c\n")),
            }
            i += 1;
        }
        s
    }

    fn engine(d: &FileDiff) -> Engine {
        Engine::new(d, "x.rs", true).unwrap()
    }

    #[test]
    fn far_windows_get_provisional_spans_and_long_parses_yield() {
        let src = rust_source(5_000, "p");
        let d = diff(b"", src.as_bytes(), Options::default());
        let n = d.lines.len();
        let last = windows(&d.hunks[0].lines) - 1;
        let mut e = engine(&d);
        let no = || false;
        // Near the hunk start: exact parsing is cheap, nothing provisional.
        assert!(e.provisional(&d, 0..10, &no).unwrap().is_empty());
        // Far: one provisional chunk per window, once.
        let p = e.provisional(&d, n - 5..n, &no).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].start, window(&d.hunks[0].lines, last).start);
        assert!(e.provisional(&d, n - 5..n, &no).unwrap().is_empty());
        // A scroll interrupts the forward parse after one window; the
        // checkpoint it made is kept and the next run resumes from there.
        assert!(e.run(&d, 0, last, &no, &|| true).is_none());
        assert_eq!(e.hunks[0].nearest(last), 1);
        let mut h = Highlights::default();
        h.merge(p.into_iter().next().unwrap(), n);
        fill(&mut e, &d, n - 5..n, &mut h);
        // The exact spans replace the provisional ones.
        let reference = sequential(&d, "x.rs", true).unwrap();
        assert_same(&h, &reference, &d, n - 5..n);
        assert!(e.provisional(&d, n - 5..n, &no).unwrap().is_empty());
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
        let mut h = Highlights::default();
        fill(&mut engine(&d), &d, 0..d.lines.len(), &mut h);
        assert!(h.covers(0..d.lines.len()));
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
        // same as the sequential highlighter; light theme and unknown types
        assert_same(
            &h,
            &sequential(&d, "x.rs", true).unwrap(),
            &d,
            0..d.lines.len(),
        );
        assert!(Engine::new(&d, "x.rs", false).is_some());
        assert!(Engine::new(&d, "x.unknownext", true).is_none());
        // a cancelled window produces nothing and is not marked done
        let mut e = engine(&d);
        assert!(e.run(&d, 0, 0, &|| true, &|| false).is_none());
        assert_eq!(e.next(&d, std::slice::from_ref(&(0..1))), Some((0, 0)));
    }

    #[test]
    fn long_lines_are_not_colored_and_big_diffs_are() {
        let long = format!("let s = \"{}\";\n", "x".repeat(3000));
        let new = format!("fn a() {{}}\n{long}fn b() {{}}\n");
        let d = diff(b"", new.as_bytes(), Options::default());
        let mut h = Highlights::default();
        fill(&mut engine(&d), &d, 0..3, &mut h);
        assert!(!h.spans(0, LineKind::Added, false).is_empty());
        assert!(h.spans(1, LineKind::Added, false).is_empty());
        assert!(!h.spans(2, LineKind::Added, false).is_empty());

        // No size cutoff: the last lines of a 30k-line new file are colored.
        let big = rust_source(30_000, "big");
        let d = diff(b"", big.as_bytes(), Options::default());
        let n = d.lines.len();
        let mut h = Highlights::default();
        fill(&mut engine(&d), &d, n - 10..n, &mut h);
        assert!(h.covers(n - 10..n));
        assert!(!h.covers(0..WINDOW), "only the requested window is styled");
        assert!(!h.spans(n - 3, LineKind::Added, false).is_empty());
    }

    #[test]
    fn windows_from_checkpoints_match_a_sequential_highlight() {
        // One huge hunk (a new file) and a modified file with many hunks,
        // some longer than a window.
        let a = rust_source(3_000, "old");
        let mut b = String::new();
        for (i, l) in a.lines().enumerate() {
            if i % 33 == 7 || (1000..1400).contains(&i) {
                b.push_str(&l.replace("old", "new"));
            } else {
                b.push_str(l);
            }
            b.push('\n');
        }
        for (old, new) in [("", a.as_str()), (a.as_str(), b.as_str())] {
            let d = diff(old.as_bytes(), new.as_bytes(), Options::default());
            let n = d.lines.len();
            assert!(d.hunks.iter().any(|h| windows(&h.lines) > 3));
            let reference = sequential(&d, "x.rs", true).unwrap();
            // Out of order: the end first (parsing forward from the hunk
            // start), then the middle (from a saved checkpoint), then all.
            let mut e = engine(&d);
            let mut h = Highlights::default();
            for r in [n - 50..n, n / 2..n / 2 + 10, 0..n] {
                fill(&mut e, &d, r.clone(), &mut h);
                assert!(h.covers(r.clone()));
                assert_same(&h, &reference, &d, r);
            }
            // A fresh engine starting mid-hunk gives the same spans too.
            let mut e = engine(&d);
            let mut h = Highlights::default();
            let r = n / 3..n / 3 + 300;
            fill(&mut e, &d, r.clone(), &mut h);
            assert_same(&h, &reference, &d, r);
        }
    }

    /// Polls `c` for `diff` until the worker is idle with every line of
    /// `want` highlighted (or panics after a few seconds).
    fn wait(c: &mut HlCache, d: &Arc<FileDiff>, vis: Range<usize>) -> Arc<Highlights> {
        let t = Instant::now();
        loop {
            let h = c.get(d, true, "x.rs", vis.clone(), None).unwrap();
            let idle = {
                let e = c
                    .entries
                    .iter()
                    .find(|e| std::ptr::eq(e.diff.as_ptr(), Arc::as_ptr(d)));
                let w = e.unwrap().job.0.lock();
                !w.running && w.idle_gen == w.generation && w.ready.is_empty()
            };
            if idle && h.covers(vis.clone()) {
                return h;
            }
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "highlighting timed out"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn only_hunks_near_the_visible_lines_are_highlighted() {
        // 200 small hunks, ~7 lines each.
        let a: String = (0..6000).map(|i| format!("let a{i} = {i};\n")).collect();
        let b: String = a
            .lines()
            .enumerate()
            .map(|(i, l)| {
                if i % 30 == 0 {
                    format!("{l} // edit\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        let d = Arc::new(diff(a.as_bytes(), b.as_bytes(), Options::default()));
        let n = d.lines.len();
        assert!(d.hunks.len() > 100 && n > 4 * MARGIN);
        let mut c = HlCache::default();
        let vis = n / 2..n / 2 + 40;
        let h = wait(&mut c, &d, vis.clone());
        let wanted = vis.start - MARGIN..vis.end + MARGIN;
        for (i, hunk) in d.hunks.iter().enumerate() {
            let near = hunk.lines.start < wanted.end && hunk.lines.end > wanted.start;
            assert_eq!(h.covers(hunk.lines.clone()), near, "hunk {i}");
            if !near {
                assert!(
                    hunk.lines
                        .clone()
                        .all(|l| h.spans(l, LineKind::Context, true).is_empty())
                );
            }
        }
        let reference = sequential(&d, "x.rs", true).unwrap();
        assert_same(&h, &reference, &d, wanted);
        // Scrolling to the end requests more; earlier results stay.
        let h2 = wait(&mut c, &d, n - 30..n);
        assert!(h2.covers(vis));
        assert!(!h2.covers(0..10));
    }

    #[test]
    fn a_superseded_diff_never_writes_into_the_newer_one() {
        let mut c = HlCache::default();
        let big = rust_source(40_000, "first");
        let a = Arc::new(diff(b"", big.as_bytes(), Options::default()));
        let n = a.lines.len();
        // Start a long job (the end of a single 40k-line hunk), then replace
        // the diff before it finishes.
        assert!(c.get(&a, true, "x.rs", n - 40..n, None).is_some());
        let old_work = Arc::clone(&c.entries[0].job.0);
        let small = rust_source(500, "second");
        let b = Arc::new(diff(b"", small.as_bytes(), Options::default()));
        drop(a);
        let hb = wait(&mut c, &b, 0..60);
        // The old worker noticed it lost its diff, its entry is pruned.
        let t = Instant::now();
        while c.len() > 1 {
            assert!(t.elapsed() < Duration::from_secs(20), "old entry kept");
            std::thread::sleep(Duration::from_millis(2));
            c.get(&b, true, "x.rs", 0..60, None);
        }
        assert!(
            old_work.cancel.load(Ordering::Relaxed),
            "its job is cancelled"
        );
        let reference = sequential(&b, "x.rs", true).unwrap();
        assert_same(&hb, &reference, &b, 0..260);
        assert!(!hb.covers(0..b.lines.len()), "only the wanted lines");
        // Nothing the old worker might still produce reaches the new entry.
        std::thread::sleep(Duration::from_millis(50));
        let hb2 = c.get(&b, true, "x.rs", 0..60, None).unwrap();
        assert_eq!(hb2.have.len(), b.lines.len());
        let have: Vec<usize> = (0..b.lines.len()).filter(|&i| hb2.have[i]).collect();
        assert!(have.len() >= 260);
        for i in have {
            assert_same(&hb2, &reference, &b, i..i + 1);
        }
    }

    #[test]
    fn cache_keys_by_diff_pointer_and_theme() {
        let mut c = HlCache::default();
        let a = Arc::new(diff(b"fn a() {}\n", b"fn b() {}\n", Options::default()));
        let r1 = wait(&mut c, &a, 0..2);
        let r2 = c.get(&a, true, "x.rs", 0..2, None).unwrap();
        assert!(Arc::ptr_eq(&r1, &r2), "no new work, same result");
        // other theme: separate entry; an equal but distinct Arc is a new key
        c.get(&a, false, "x.rs", 0..2, None);
        let b = Arc::new(diff(b"fn a() {}\n", b"fn b() {}\n", Options::default()));
        c.get(&b, true, "x.rs", 0..2, None);
        assert_eq!(c.len(), 3);
        // rekey keeps the result for the replacement Arc
        c.rekey(&a, &b);
        drop(a);
        let r3 = c.get(&b, true, "x.rs", 0..2, None).unwrap();
        assert!(Arc::ptr_eq(&r1, &r3));
        // unknown types, binary and empty diffs are not highlightable
        let u = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        let t = Instant::now();
        while c.get(&u, true, "x.unknownext", 0..2, None).is_some() {
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(2));
        }
        let empty = Arc::new(diff(b"a\n", b"a\n", Options::default()));
        assert!(c.get(&empty, true, "x.rs", 0..1, None).is_none());
    }

    #[test]
    fn cache_drops_dead_diffs_and_cancels_evicted_jobs() {
        let mut c = HlCache::default();
        let first = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        c.get(&first, true, "x.rs", 0..2, None);
        let work = Arc::clone(&c.entries[0].job.0);
        let keep: Vec<Arc<FileDiff>> = (0..CACHE_MAX)
            .map(|_| Arc::new(diff(b"a\n", b"b\n", Options::default())))
            .collect();
        for d in &keep {
            c.get(d, true, "x.rs", 0..2, None);
        }
        assert_eq!(c.len(), CACHE_MAX);
        assert!(
            work.cancel.load(Ordering::Relaxed),
            "evicted job is cancelled"
        );
        drop(keep);
        let d = Arc::new(diff(b"a\n", b"b\n", Options::default()));
        // (a worker holds its diff while it works on a window)
        let t = Instant::now();
        while {
            c.get(&d, true, "x.rs", 0..2, None);
            c.len() > 1
        } {
            assert!(t.elapsed() < Duration::from_secs(20), "dropped diffs kept");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(c.len(), 1, "entries of dropped diffs are pruned");
    }
}
