//! Search commits by message, author or id, and per-file history (agent: history).

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use crate::views::history::selection::{self, Selection};
use crate::views::history::table::{self, Columns};
use crate::views::history::task::{Task, TaskState};
use crate::views::history::{self as hv, in_rect, split};
use egui::{Id, Ui};
use gk_core::{Load, ObjectId, Snapshot};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(250);
const PREFETCH: usize = 40;

/// Commits touching one file, newest first, with the file's path in each.
#[derive(Default)]
pub struct FileHist {
    pub entries: Vec<(ObjectId, String)>,
    index: HashMap<ObjectId, usize>,
}

impl FileHist {
    pub fn new(entries: Vec<(ObjectId, String)>) -> Self {
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();
        Self { entries, index }
    }
}

struct FileMode {
    path: String,
    /// Bumped by "Refresh" to re-run git.
    run: u64,
    task: Task<(String, u64), FileHist>,
}

#[derive(Default)]
pub struct SearchState {
    pub text: String,
    applied: String,
    edited_at: Option<Instant>,
    focus_box: bool,
    file: Option<FileMode>,
    pub sel: Selection<ObjectId>,
    scroll_to: Option<ObjectId>,
    menu_ids: Vec<ObjectId>,
    page_rows: usize,
    cols: Columns,
    /// Results currently listed (used for selection lookups).
    pub(crate) rows: Option<Arc<Vec<u32>>>,
    pub(crate) file_result: Option<Arc<FileHist>>,
    source_ptr: usize,
}

/// What the list shows this frame.
#[derive(Clone)]
enum Source {
    Rows(Arc<Vec<u32>>),
    File(Arc<FileHist>),
}

impl SearchState {
    /// Switches to text search for `text` (debounced like typing).
    pub fn start_query(&mut self, text: String) {
        self.file = None;
        self.file_result = None;
        self.text = text;
        self.edited_at = Some(Instant::now());
        self.focus_box = true;
    }

    /// Lists the commits touching `path` (`git log --follow`), run in the background.
    pub fn file_history(&mut self, path: String) {
        self.file = Some(FileMode {
            path,
            run: 0,
            task: Task::default(),
        });
        self.file_result = None;
        self.sel.clear();
    }

    pub fn select_commit(&mut self, id: ObjectId) {
        self.sel.select_only(id);
        self.scroll_to = Some(id);
    }

    /// Position of `id` in the listed results.
    pub fn row_of(&self, snap: &Snapshot, id: &ObjectId) -> Option<usize> {
        if self.file.is_some() {
            return self.file_result.as_ref()?.index.get(id).copied();
        }
        let r = snap.history.row(id)? as u32;
        self.rows.as_ref()?.binary_search(&r).ok()
    }

    /// In file history mode: the file's path at commit `id`.
    pub fn path_at(&self, id: &ObjectId) -> Option<String> {
        self.file.as_ref()?;
        let fh = self.file_result.as_ref()?;
        fh.index.get(id).map(|&i| fh.entries[i].1.clone())
    }
}

/// Shows the history of `path` in the search view.
pub fn open_file_history(tab: &mut RepoTab, path: String) {
    tab.view = View::Search;
    tab.search.file_history(path);
}

/// Parses `git log --follow --format=%x01%H --name-status -z -- <path>`.
/// Returns (commit, path of the file in that commit), newest first.
pub fn parse_file_log(out: &str, path: &str) -> Vec<(ObjectId, String)> {
    let mut v = Vec::new();
    let mut cur = path.to_string();
    for rec in out.split('\u{1}') {
        let mut parts = rec.split('\0');
        let Some(hash) = parts.next().map(str::trim) else {
            continue;
        };
        let Ok(id) = ObjectId::from_hex(hash.as_bytes()) else {
            continue;
        };
        let mut toks = parts
            .map(|t| t.trim_matches(['\n', '\r']))
            .filter(|t| !t.is_empty());
        let mut here = cur.clone();
        let mut older = None;
        while let Some(status) = toks.next() {
            if status.starts_with('R') || status.starts_with('C') {
                let old = toks.next();
                if let Some(new) = toks.next() {
                    here = new.to_string();
                }
                if status.starts_with('R') {
                    older = old.map(str::to_string);
                }
            } else if let Some(p) = toks.next() {
                here = p.to_string();
            }
        }
        cur = older.unwrap_or_else(|| here.clone());
        v.push((id, here));
    }
    v
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    hv::poll_clipboard(ui, tab);
    let source = top_bar(ui, tab);
    ui.add_space(2.0);
    let Some(source) = source else { return };

    // Keep lookups in sync with what is listed.
    let ptr = match &source {
        Source::Rows(r) => Arc::as_ptr(r) as usize,
        Source::File(f) => Arc::as_ptr(f) as usize,
    };
    match &source {
        Source::Rows(r) => tab.search.rows = Some(r.clone()),
        Source::File(f) => tab.search.file_result = Some(f.clone()),
    }
    if ptr != tab.search.source_ptr {
        tab.search.source_ptr = ptr;
        let s = &tab.search;
        let gone: std::collections::HashSet<ObjectId> = s
            .sel
            .iter()
            .filter(|id| s.row_of(snap, id).is_none())
            .copied()
            .collect();
        if !gone.is_empty() {
            tab.search
                .sel
                .retain(|id| (!gone.contains(id)).then_some(0));
        }
    }

    let mut frac = tab.state.layout.list_split;
    let (top, bottom) = split(
        ui,
        Id::new(("search-split", &tab.path)),
        &mut frac,
        true,
        80.0,
    );
    tab.state.layout.list_split = frac;
    in_rect(ui, top, "search-table", |ui| {
        results_table(ui, tab, snap, shared, &source)
    });
    in_rect(ui, bottom, "search-details", |ui| {
        crate::views::details::show(ui, tab, snap, shared)
    });
}

/// Search box / file history header. Returns the list to show, if any.
fn top_bar(ui: &mut Ui, tab: &mut RepoTab) -> Option<Source> {
    let ctx = ui.ctx().clone();
    let mut out = None;
    ui.horizontal(|ui| {
        let s = &mut tab.search;
        if let Some(fm) = &mut s.file {
            ui.label(egui::RichText::new("File history:").strong());
            ui.label(egui::RichText::new(&fm.path).monospace());
            if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                fm.run += 1;
            }
            let key = (fm.path.clone(), fm.run);
            let git = tab.session.repo().git().clone();
            let all = tab.state.history_scope != "current";
            let path = fm.path.clone();
            let state = fm.task.get(&key, &ctx, move || {
                let mut args = vec!["log", "--follow", "--format=%x01%H", "--name-status", "-z"];
                if all {
                    args.push("--all");
                }
                args.extend(["--", path.as_str()]);
                let o = git.run(&args)?;
                Ok(FileHist::new(parse_file_log(&o.stdout_str(), &path)))
            });
            match state {
                TaskState::Done(f) => {
                    let n = f.entries.len();
                    let s = if n == 1 { "" } else { "s" };
                    ui.label(egui::RichText::new(format!("{n} commit{s}")).weak());
                    out = Some(Source::File(f));
                }
                TaskState::Failed(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                _ => {
                    ui.spinner();
                }
            }
            if ui.button("Back to search").clicked() {
                s.file = None;
                s.file_result = None;
                s.sel.clear();
                s.focus_box = true;
            }
            return;
        }
        let r = ui.add(
            egui::TextEdit::singleline(&mut s.text)
                .hint_text("Search messages, authors, SHAs…")
                .desired_width(320.0),
        );
        if std::mem::take(&mut s.focus_box) {
            r.request_focus();
        }
        if r.changed() {
            s.edited_at = Some(Instant::now());
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        match s.edited_at {
            Some(t) if enter || t.elapsed() >= DEBOUNCE => {
                s.applied = s.text.trim().to_string();
                s.edited_at = None;
            }
            Some(t) => ctx.request_repaint_after(DEBOUNCE.saturating_sub(t.elapsed())),
            None => {}
        }
        if !s.text.is_empty() && ui.small_button("✖").on_hover_text("Clear").clicked() {
            s.text.clear();
            s.applied.clear();
            s.edited_at = None;
        }
        if s.applied.is_empty() {
            s.rows = None;
            return;
        }
        match tab.session.search(&s.applied) {
            Load::Ready(rows) => {
                let n = rows.len();
                let s = if n == 1 { "" } else { "s" };
                ui.label(egui::RichText::new(format!("{n} result{s}")).weak());
                out = Some(Source::Rows(rows));
            }
            Load::Failed(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            Load::Loading => {
                ui.spinner();
                // Keep showing the previous results while searching.
                out = s.rows.clone().map(Source::Rows);
            }
        }
    });
    if out.is_none() && tab.search.file.is_none() && tab.search.applied.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            ui.label(
                egui::RichText::new("Type to search commit messages, authors and SHAs.").weak(),
            );
        });
    }
    out
}

fn results_table(
    ui: &mut Ui,
    tab: &mut RepoTab,
    snap: &Snapshot,
    shared: &mut Shared,
    src: &Source,
) {
    let hist = snap.history.clone();
    let row_h = shared.settings.history.row_height.clamp(14.0, 60.0);
    let n = match src {
        Source::Rows(r) => r.len(),
        Source::File(f) => f.entries.len(),
    };
    let key_at = |i: usize| -> Option<ObjectId> {
        match src {
            Source::Rows(r) => r
                .get(i)
                .and_then(|&row| hist.ids.get(row as usize).copied()),
            Source::File(f) => f.entries.get(i).map(|e| e.0),
        }
    };
    let row_of = |id: &ObjectId| -> Option<usize> {
        match src {
            Source::Rows(r) => hist
                .row(id)
                .and_then(|row| r.binary_search(&(row as u32)).ok()),
            Source::File(f) => f.index.get(id).copied(),
        }
    };
    let id = Id::new(("search-list", &tab.path));

    if ui.memory(|m| m.has_focus(id)) {
        let st = &mut tab.search;
        for (key, shift) in table::nav_keys(ui) {
            let cur = st.sel.cursor_row(row_of);
            if let Some(t) = selection::nav_target(key, cur, n, st.page_rows.saturating_sub(1)) {
                st.sel.move_to(t, shift, key_at, row_of);
                st.scroll_to = key_at(t);
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C))
            && !st.sel.is_empty()
        {
            let ids = st.sel.sorted(row_of);
            ui.ctx().copy_text(
                ids.iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    let scroll_row = tab
        .search
        .scroll_to
        .take()
        .and_then(|k| row_of(&k))
        .map(table::ScrollTo::Reveal);

    let left = ui.max_rect().left();
    let right = left + ui.available_width();
    let rects = table::layout_columns(left, right, 0.0, &tab.search.cols);
    table::header(ui, id.with("header"), &mut tab.search.cols, &rects, false);

    let st = hv::cell_style(ui, shared);
    let cols = tab.search.cols;
    let session = &tab.session;
    let sel = &tab.search.sel;
    let focused = ui.memory(|m| m.has_focus(id));
    let visuals = ui.visuals().clone();
    let head = hv::head_oid(snap);
    let out = table::virtual_list(ui, id, n, row_h, scroll_row, |f, range| {
        let pre_a = range.start.saturating_sub(PREFETCH);
        let pre_b = (range.end + PREFETCH).min(n);
        let ids: Vec<ObjectId> = (pre_a..pre_b).filter_map(key_at).collect();
        let metas = session.metas(&ids);
        let rects = table::layout_columns(f.x.min, f.x.max, 0.0, &cols);
        for r in range {
            let Some(cid) = key_at(r) else { continue };
            let rect = f.row_rect(r);
            table::paint_row_bg(
                f,
                r,
                rect,
                sel.contains(&cid),
                focused,
                st.palette,
                &visuals,
            );
            let meta = metas.get(r - pre_a).and_then(|m| m.as_deref());
            let labels = table::labels_of(&snap.refs.labels, &cid);
            table::paint_commit_cells(
                f.painter,
                rect,
                &rects,
                &st,
                &cid,
                meta,
                labels,
                head == Some(cid),
            );
        }
    });
    tab.search.page_rows = out.visible.len().max(1);

    if let Some((r, m)) = out.clicked {
        tab.search.sel.click(r, m.command, m.shift, key_at, row_of);
    }
    if let Some(cid) = out.double_clicked.and_then(key_at) {
        tab.dialog = Some(Dialog::checkout(hv::checkout_rev(snap, &cid)));
    }
    if let Some(r) = out.secondary {
        if let Some(cid) = key_at(r) {
            if !tab.search.sel.contains(&cid) {
                tab.search.sel.select_only(cid);
            }
            tab.search.menu_ids = tab.search.sel.sorted(row_of);
        } else {
            tab.search.menu_ids.clear();
        }
    }
    if !tab.search.menu_ids.is_empty() {
        let ids = std::mem::take(&mut tab.search.menu_ids);
        out.response
            .context_menu(|ui| hv::commit_menu(ui, tab, snap, &ids));
        tab.search.menu_ids = ids;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "4ca56ea8275359cbf761c0ca70e915ac2d28f067";
    const B: &str = "9ef121074b65d1f768ada8c890282ab0aacdca6f";
    const C: &str = "529d945b275dc7d95b8a1f25199d59ca94deede6";

    #[test]
    fn file_log_follows_renames() {
        let out =
            format!("\u{1}{A}\0\nM\0b.txt\0\u{1}{B}\0\nR100\0a.txt\0b.txt\0\u{1}{C}\0\nA\0a.txt\0");
        let v = parse_file_log(&out, "b.txt");
        let paths: Vec<&str> = v.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(paths, vec!["b.txt", "b.txt", "a.txt"]);
        assert_eq!(v[2].0, ObjectId::from_hex(C.as_bytes()).unwrap());
    }

    #[test]
    fn file_log_without_status_keeps_path() {
        // Merge commits may come without name-status entries.
        let out = format!("\u{1}{A}\0\n\u{1}{B}\0\nM\0dir/x.rs\0");
        let v = parse_file_log(&out, "dir/x.rs");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].1, "dir/x.rs");
        assert!(parse_file_log("", "x").is_empty());
    }

    #[test]
    fn file_hist_index() {
        let a = ObjectId::from_hex(A.as_bytes()).unwrap();
        let fh = FileHist::new(vec![(a, "p".into())]);
        assert_eq!(fh.index.get(&a), Some(&0));
    }
}
