//! One virtualized file list (staged or unstaged): flat or tree rows,
//! multi-selection, keyboard navigation and the context menu. The list only
//! emits [`Ev`]ents; the parent runs the operations.

use super::dnd::DragFiles;
use super::filter;
use super::tree::{self, Tree, TreeRow};
use crate::keymap::{self, Act};
use crate::theme::Palette;
use egui::{Align2, Color32, Id, Pos2, Rect, Sense, Ui, pos2, vec2};
use gk_status::{Entry, Kind};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Side {
    Staged,
    #[default]
    Unstaged,
}

const INDENT: f32 = 14.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    File {
        entry: u32,
        depth: u16,
    },
    Dir {
        node: u32,
        name: String,
        depth: u16,
        collapsed: bool,
    },
}

/// What the user asked for; handled by the file status view.
#[derive(Debug, Clone)]
pub enum Ev {
    /// Stage (unstaged list) or unstage (staged list) these paths.
    Toggle(Vec<String>),
    /// Stage all (unstaged list) or unstage all (staged list).
    All,
    /// The list was clicked / navigated: it becomes the active one.
    Activate,
    FocusDiff,
    Discard {
        tracked: Vec<String>,
        untracked: Vec<String>,
    },
    Stash(Vec<String>),
    Ignore(Vec<String>),
    Open(String),
    Reveal(String),
    Copy(String),
    History(String),
    Blame(String),
    ResolveOurs(Vec<String>),
    ResolveTheirs(Vec<String>),
    MarkResolved(Vec<String>),
    MergeTool(String),
    /// Open the built-in three-way merge editor.
    MergeEditor(String),
    /// Ask for a custom ignore pattern (prefilled with a suggestion).
    IgnoreCustom(String),
    /// Open the file's diff in the configured external diff tool.
    ExternalDiff(String),
}

/// Status filter chips: (bit, code, label). A mask of 0 shows everything.
pub const KIND_FILTERS: [(u8, &str, &str); 6] = [
    (1, "M", "Modified"),
    (2, "A", "Added"),
    (4, "D", "Deleted"),
    (8, "R", "Renamed"),
    (16, "?", "Untracked"),
    (32, "U", "Conflicted"),
];

/// The [`KIND_FILTERS`] bit of a status kind.
pub fn kind_bit(k: Kind) -> u8 {
    match k {
        Kind::Modified | Kind::TypeChange => 1,
        Kind::Added | Kind::IntentToAdd => 2,
        Kind::Deleted => 4,
        Kind::Renamed | Kind::Copied => 8,
        Kind::Untracked => 16,
        Kind::Conflict(_) => 32,
    }
}

#[derive(Debug, Default, PartialEq, Eq, Clone)]
struct ModelKey {
    status_gen: u64,
    filter: String,
    kinds: u8,
    tree: bool,
    collapse_ver: u64,
}

#[derive(Default)]
pub struct Model {
    key: Option<ModelKey>,
    pub rows: Vec<Row>,
    pub tree: Option<Tree>,
    /// Files passing the filter.
    pub files: usize,
    /// The last filter pass, narrowed incrementally while typing.
    matched: Option<filter::Matched>,
}

#[derive(Default)]
pub struct ListState {
    pub selected: HashSet<String>,
    pub anchor: Option<String>,
    /// The focused file (its diff is shown when this list is active).
    pub cursor: Option<String>,
    /// Last known row of `cursor`, to pick a neighbor when it disappears.
    pub cursor_row: usize,
    pub collapsed: HashSet<String>,
    pub collapse_ver: u64,
    pub model: Model,
    scroll_to_row: Option<usize>,
    offset: f32,
    view_h: f32,
}

/// Inputs for rendering a list.
pub struct ListCtx<'a> {
    pub side: Side,
    pub entries: &'a [Entry],
    pub paths: &'a [String],
    pub palette: Palette,
    pub tree: bool,
    /// The diff shows this list's cursor.
    pub active: bool,
    /// An index operation runs that touches this path.
    pub pending: &'a dyn Fn(&str) -> bool,
    pub has_merge_tool: bool,
    /// Drag-and-drop scope of the repository tab (see [`DragFiles`]).
    pub dnd_scope: Id,
    pub has_diff_tool: bool,
    /// Extra row height (comfortable density adds more).
    pub row_pad: f32,
}

impl ListState {
    /// Rebuilds rows when the status, filter, mode or collapsed set changed.
    /// `kinds` is a [`KIND_FILTERS`] mask (0 = all) over `entries` (parallel
    /// to `paths`). Returns true when rebuilt.
    pub fn sync(
        &mut self,
        status_gen: u64,
        paths: &[String],
        entries: &[Entry],
        filter: &str,
        kinds: u8,
        tree_mode: bool,
    ) -> bool {
        let key = ModelKey {
            status_gen,
            filter: filter.to_string(),
            kinds,
            tree: tree_mode,
            collapse_ver: self.collapse_ver,
        };
        if self.model.key.as_ref() == Some(&key) {
            return false;
        }
        // Same status, chips and query (tree mode or collapsed folders
        // changed): keep the matches. A query that only grew filters the
        // previous matches; anything else runs a full (parallel) pass.
        let idx = match self.model.matched.take() {
            Some(m) if m.status_gen == status_gen && m.kinds == kinds && m.query == filter => m.idx,
            prev => {
                let within = prev
                    .as_ref()
                    .and_then(|m| m.reusable(status_gen, kinds, filter));
                filter::filter_indices(paths, entries, kinds, filter, within)
            }
        };
        self.model.files = idx.len();
        if tree_mode {
            let items: Vec<(u32, &str)> = idx
                .iter()
                .map(|&i| (i, paths[i as usize].as_str()))
                .collect();
            let t = tree::build(&items);
            self.model.rows = tree::flatten(&t, &self.collapsed)
                .into_iter()
                .map(|r| match r {
                    TreeRow::Dir {
                        node,
                        name,
                        depth,
                        collapsed,
                    } => Row::Dir {
                        node,
                        name,
                        depth,
                        collapsed,
                    },
                    TreeRow::File { item, depth } => Row::File { entry: item, depth },
                })
                .collect();
            self.model.tree = Some(t);
        } else {
            self.model.rows = idx
                .iter()
                .map(|&i| Row::File { entry: i, depth: 0 })
                .collect();
            self.model.tree = None;
        }
        self.model.matched = Some(filter::Matched {
            status_gen,
            kinds,
            query: filter.to_string(),
            idx,
        });
        // Forget selected paths that are gone from the list.
        if self
            .model
            .key
            .as_ref()
            .is_none_or(|k| k.status_gen != status_gen)
        {
            self.selected.retain(|p| paths.binary_search(p).is_ok());
        }
        self.model.key = Some(key);
        true
    }

    pub fn row_of(&self, path: &str, paths: &[String]) -> Option<usize> {
        let is = |r: &Row| matches!(r, Row::File { entry, .. } if paths.get(*entry as usize).is_some_and(|p| p == path));
        if self.model.rows.get(self.cursor_row).is_some_and(is) {
            return Some(self.cursor_row);
        }
        self.model.rows.iter().position(is)
    }

    fn path_at(&self, row: usize, paths: &[String]) -> Option<String> {
        match self.model.rows.get(row)? {
            Row::File { entry, .. } => paths.get(*entry as usize).cloned(),
            Row::Dir { .. } => None,
        }
    }

    /// Nearest file row at or after `row` (or before, at the end).
    fn file_row_near(&self, row: usize) -> Option<usize> {
        let rows = &self.model.rows;
        let is_file = |r: &Row| matches!(r, Row::File { .. });
        let start = row.min(rows.len().saturating_sub(1));
        (start..rows.len())
            .find(|&i| is_file(&rows[i]))
            .or_else(|| (0..start).rev().find(|&i| is_file(&rows[i])))
    }

    /// Number of [`targets`](Self::targets), without building them.
    pub fn target_count(&self) -> usize {
        if self.selected.is_empty() {
            usize::from(self.cursor.is_some())
        } else {
            self.selected.len()
        }
    }

    /// Paths an action applies to: the selection, or the cursor.
    pub fn targets(&self) -> Vec<String> {
        if !self.selected.is_empty() {
            let mut v: Vec<String> = self.selected.iter().cloned().collect();
            v.sort();
            v
        } else {
            self.cursor.iter().cloned().collect()
        }
    }

    /// Paths dragged when the drag starts on `path`: the selection when
    /// `path` is part of it, else just `path`.
    pub fn drag_paths(&self, path: &str) -> Vec<String> {
        if self.selected.contains(path) {
            self.targets()
        } else {
            vec![path.to_string()]
        }
    }

    pub fn select_only(&mut self, path: String, row: usize) {
        self.selected.clear();
        self.selected.insert(path.clone());
        self.anchor = Some(path.clone());
        self.cursor = Some(path);
        self.cursor_row = row;
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.cursor = None;
    }

    /// Selects the files between the anchor and `row`.
    fn select_range(&mut self, row: usize, paths: &[String], additive: bool) {
        let anchor_row = self
            .anchor
            .as_deref()
            .and_then(|a| self.row_of(a, paths))
            .unwrap_or(row);
        if !additive {
            self.selected.clear();
        }
        let (lo, hi) = if anchor_row <= row {
            (anchor_row, row)
        } else {
            (row, anchor_row)
        };
        for r in lo..=hi {
            if let Some(p) = self.path_at(r, paths) {
                self.selected.insert(p);
            }
        }
        if self.anchor.is_none() {
            self.anchor = self.path_at(row, paths);
        }
        self.cursor = self.path_at(row, paths);
        self.cursor_row = row;
    }

    /// Keeps the cursor on an existing row: when its file vanished (e.g. it
    /// was staged), the file now at its old row takes over.
    pub fn repair_cursor(&mut self, paths: &[String]) -> bool {
        let Some(c) = self.cursor.clone() else {
            return false;
        };
        if let Some(r) = self.row_of(&c, paths) {
            self.cursor_row = r;
            return false;
        }
        match self
            .file_row_near(self.cursor_row)
            .and_then(|r| self.path_at(r, paths).map(|p| (r, p)))
        {
            Some((r, p)) => self.select_only(p, r),
            None => self.clear(),
        }
        true
    }

    /// All files below a directory row.
    fn dir_files(&self, node: u32, paths: &[String]) -> Vec<String> {
        let Some(t) = &self.model.tree else {
            return Vec::new();
        };
        let Some(n) = t.nodes.get(node as usize) else {
            return Vec::new();
        };
        t.order[n.range.start as usize..n.range.end as usize]
            .iter()
            .filter_map(|&i| paths.get(i as usize).cloned())
            .collect()
    }

    fn dir_path(&self, node: u32) -> String {
        self.model
            .tree
            .as_ref()
            .and_then(|t| t.nodes.get(node as usize))
            .map(|n| n.path.clone())
            .unwrap_or_default()
    }
}

enum Click {
    File {
        row: usize,
        path: String,
        mods: egui::Modifiers,
    },
    Dir {
        node: u32,
    },
    Secondary {
        row: usize,
        path: String,
    },
    Background,
}

/// Renders the list; appends user requests to `out`.
pub fn show(ui: &mut Ui, ls: &mut ListState, c: &ListCtx, out: &mut Vec<Ev>) {
    let p = c.palette;
    let list_id = ui.make_persistent_id(("gk-file-list", c.side));
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let k = crate::theme::text_scale(ui.style());
    let row_h = (ui.ctx().fonts_mut(|f| f.row_height(&body_font)) + c.row_pad).round();
    let n = ls.model.rows.len();

    if n == 0 {
        let text = if c.paths.is_empty() {
            match c.side {
                Side::Staged => "No staged files",
                Side::Unstaged => "No pending changes",
            }
        } else {
            "No files match the filter"
        };
        ui.add_space(6.0);
        ui.label(egui::RichText::new(text).color(p.muted).italics());
        return;
    }

    let mut sa = egui::ScrollArea::vertical()
        .id_salt(("gk-file-list-scroll", c.side))
        .auto_shrink([false, false]);
    if let Some(r) = ls.scroll_to_row.take() {
        let top = r as f32 * row_h;
        if top < ls.offset {
            sa = sa.vertical_scroll_offset(top);
        } else if top + row_h > ls.offset + ls.view_h && ls.view_h > 0.0 {
            sa = sa.vertical_scroll_offset(top + row_h - ls.view_h);
        }
    }

    let mut click: Option<Click> = None;
    let mut toggle_dir: Option<String> = None;
    let mut kb_menu_open = false;
    let focused = ui.memory(|m| m.focused() == Some(list_id));
    let strong = ui.visuals().strong_text_color();
    let hover_fill = ui
        .visuals()
        .widgets
        .hovered
        .weak_bg_fill
        .gamma_multiply(0.5);

    // Where a keyboard-opened context menu appears: below the cursor row
    // when it is on screen, else at the top of the list.
    let mut menu_pos: Option<Pos2> = None;
    let sa_out = sa.show_viewport(ui, |ui, vp| {
        ui.set_min_height(n as f32 * row_h);
        let origin = ui.max_rect().min;
        let vis = Rect::from_min_size(origin + vp.min.to_vec2(), vp.size());
        let bg = ui.interact(vis, list_id, Sense::click());
        if bg.clicked() {
            click = Some(Click::Background);
        }
        menu_pos = Some(vis.left_top() + vec2(30.0, 0.0));
        let first = ((vp.min.y / row_h).floor().max(0.0) as usize).min(n);
        let last = ((vp.max.y / row_h).ceil() as usize + 1).min(n);
        let painter = ui.painter().clone();
        for r in first..last {
            let y = origin.y + r as f32 * row_h;
            let rect = Rect::from_min_size(pos2(vis.left(), y), vec2(vis.width(), row_h));
            match &ls.model.rows[r] {
                Row::File { entry, depth } => {
                    let Some(e) = c.entries.get(*entry as usize) else {
                        continue;
                    };
                    let path = &c.paths[*entry as usize];
                    let id = list_id.with(path.as_str());
                    let resp = ui.interact(rect, id, Sense::click_and_drag());
                    if resp.drag_started() {
                        resp.dnd_set_drag_payload(DragFiles {
                            scope: c.dnd_scope,
                            from: c.side,
                            paths: ls.drag_paths(path),
                        });
                    }
                    let selected = ls.selected.contains(path);
                    let is_cursor = ls.cursor.as_deref() == Some(path.as_str());
                    if selected {
                        let fill = if c.active {
                            p.selection
                        } else {
                            p.selection.gamma_multiply(0.5)
                        };
                        painter.rect_filled(rect, 0.0, fill);
                    } else if resp.hovered() {
                        painter.rect_filled(rect, 0.0, hover_fill);
                    }
                    if is_cursor && vis.contains(rect.center()) {
                        menu_pos = Some(pos2(rect.left() + 30.0, rect.bottom()));
                    }
                    if is_cursor && focused {
                        painter.rect_stroke(
                            rect.shrink(0.5),
                            0.0,
                            (1.0, p.branch),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let pending = (c.pending)(path);
                    let dim = |col: Color32| {
                        if pending {
                            col.gamma_multiply(0.4)
                        } else {
                            col
                        }
                    };
                    let mut x = rect.left()
                        + 4.0
                        + *depth as f32 * INDENT * k
                        + if c.tree { INDENT * k } else { 0.0 };
                    let cy = rect.center().y;
                    // No check boxes: staging works on the selection (double click, Space,
                    // the header buttons, drag and drop, the context menu).
                    super::badge::paint(
                        &painter,
                        pos2(x, cy),
                        e.kind,
                        dim(crate::util::status_color(&p, e.kind)),
                        p.dark,
                        k,
                    );
                    x += 20.0 * k;
                    let (dir, name) = crate::views::diff::split_path(path);
                    let clip = painter.with_clip_rect(rect.intersect(vis));
                    let g = clip.layout_no_wrap(name.to_string(), body_font.clone(), dim(strong));
                    let w = g.size().x;
                    clip.galley(pos2(x, cy - g.size().y / 2.0), g, strong);
                    x += w + 6.0;
                    let mut extra = String::new();
                    if !c.tree && !dir.is_empty() {
                        extra.push_str(dir.trim_end_matches('/'));
                    }
                    if let Some(o) = &e.orig_path {
                        extra.push_str(&format!("  ← {o}"));
                    }
                    if !extra.is_empty() {
                        clip.text(
                            pos2(x, cy),
                            Align2::LEFT_CENTER,
                            extra,
                            body_font.clone(),
                            dim(p.muted),
                        );
                    }
                    if resp.double_clicked() && matches!(e.kind, Kind::Conflict(_)) {
                        // Staging a conflicted file would commit its markers: open the editor.
                        out.push(Ev::MergeEditor(path.clone()));
                    } else if resp.double_clicked() {
                        out.push(Ev::Toggle(vec![path.clone()]));
                    } else if resp.clicked() {
                        let mods = ui.input(|i| i.modifiers);
                        click = Some(Click::File {
                            row: r,
                            path: path.clone(),
                            mods,
                        });
                    } else if resp.secondary_clicked() && !selected {
                        click = Some(Click::Secondary {
                            row: r,
                            path: path.clone(),
                        });
                    }
                    let resp = if let Kind::Conflict(k) = e.kind {
                        resp.on_hover_text(format!("{path}\nConflict: {}", k.label()))
                    } else if c.tree {
                        resp.on_hover_text(path.as_str())
                    } else {
                        resp
                    };
                    resp.context_menu(|ui| {
                        let targets = if selected && ls.selected.len() > 1 {
                            ls.targets()
                        } else {
                            vec![path.clone()]
                        };
                        file_menu(ui, c, &targets, out);
                    });
                }
                Row::Dir {
                    node,
                    name,
                    depth,
                    collapsed,
                } => {
                    let id = list_id.with(("dir", *node, name.as_str()));
                    let resp = ui.interact(rect, id, Sense::click_and_drag());
                    if resp.drag_started() {
                        resp.dnd_set_drag_payload(DragFiles {
                            scope: c.dnd_scope,
                            from: c.side,
                            paths: ls.dir_files(*node, c.paths),
                        });
                    }
                    if resp.hovered() {
                        painter.rect_filled(rect, 0.0, hover_fill);
                    }
                    let cy = rect.center().y;
                    let mut x = rect.left() + 4.0 + *depth as f32 * INDENT * k;
                    painter.text(
                        pos2(x + 5.0, cy),
                        Align2::CENTER_CENTER,
                        if *collapsed { "▸" } else { "▾" },
                        body_font.clone(),
                        p.muted,
                    );
                    x += INDENT * k;
                    let clip = painter.with_clip_rect(rect.intersect(vis));
                    let icon_side = (body_font.size * 1.1).round();
                    crate::icons::paint(
                        &clip,
                        crate::icons::Icon::Folder,
                        Rect::from_center_size(
                            pos2(x + icon_side / 2.0, cy),
                            vec2(icon_side, icon_side),
                        ),
                        crate::icons::Colors::theme(),
                    );
                    x += icon_side + 4.0;
                    let g = clip.layout_no_wrap(name.to_string(), body_font.clone(), strong);
                    let w = g.size().x;
                    clip.galley(pos2(x, cy - g.size().y / 2.0), g, strong);
                    let count = ls
                        .model
                        .tree
                        .as_ref()
                        .and_then(|t| t.nodes.get(*node as usize))
                        .map(|n| n.range.len())
                        .unwrap_or(0);
                    clip.text(
                        pos2(x + w + 6.0, cy),
                        Align2::LEFT_CENTER,
                        format!("({count})"),
                        body_font.clone(),
                        p.muted,
                    );
                    if resp.double_clicked() {
                        // The two clicks folded and unfolded it again: stage / unstage it all.
                        out.push(Ev::Toggle(ls.dir_files(*node, c.paths)));
                    } else if resp.clicked() {
                        click = Some(Click::Dir { node: *node });
                    }
                    resp.context_menu(|ui| {
                        let files = ls.dir_files(*node, c.paths);
                        let dir = ls.dir_path(*node);
                        dir_menu(ui, c, &dir, &files, out);
                    });
                }
            }
        }
        bg
    });
    ls.offset = sa_out.state.offset.y;
    ls.view_h = sa_out.inner_rect.height();
    let bg = sa_out.inner;

    // ---- apply clicks
    match click {
        Some(Click::File { row, path, mods }) => {
            if mods.shift {
                ls.select_range(row, c.paths, mods.command);
            } else if mods.command {
                if !ls.selected.remove(&path) {
                    ls.selected.insert(path.clone());
                }
                ls.anchor = Some(path.clone());
                ls.cursor = Some(path);
                ls.cursor_row = row;
            } else {
                ls.select_only(path, row);
            }
            ui.memory_mut(|m| m.request_focus(list_id));
            out.push(Ev::Activate);
        }
        Some(Click::Secondary { row, path }) => {
            ls.select_only(path, row);
            out.push(Ev::Activate);
        }
        Some(Click::Dir { node }) => {
            toggle_dir = Some(ls.dir_path(node));
            ui.memory_mut(|m| m.request_focus(list_id));
        }
        Some(Click::Background) => {
            ui.memory_mut(|m| m.request_focus(list_id));
        }
        None => {}
    }
    if let Some(d) = toggle_dir {
        if !ls.collapsed.remove(&d) {
            ls.collapsed.insert(d);
        }
        ls.collapse_ver += 1;
    }

    // ---- keyboard
    if ui.memory(|m| m.focused() == Some(list_id)) {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                list_id,
                egui::EventFilter {
                    tab: false,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            )
        });
        use egui::Key;
        let acts = keymap::pressed(ui.ctx(), &FILE_ACTS);
        let (up, down, enter, all, shift, home, end, menu) = ui.input_mut(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::COMMAND, Key::A),
                i.modifiers.shift,
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.consume_key(egui::Modifiers::SHIFT, Key::F10),
            )
        });
        if menu && ls.target_count() > 0 {
            kb_menu_open = true;
        }
        for a in acts {
            key_action(a, ls, c, out);
        }
        if up || down || home || end {
            let rows = &ls.model.rows;
            let is_file = |r: usize| matches!(rows.get(r), Some(Row::File { .. }));
            let cur = ls.cursor.as_deref().and_then(|cp| ls.row_of(cp, c.paths));
            let next = if home {
                (0..rows.len()).find(|&r| is_file(r))
            } else if end {
                (0..rows.len()).rev().find(|&r| is_file(r))
            } else {
                match (cur, down) {
                    (None, _) => (0..rows.len()).find(|&r| is_file(r)),
                    (Some(c0), true) => (c0 + 1..rows.len()).find(|&r| is_file(r)),
                    (Some(c0), false) => (0..c0).rev().find(|&r| is_file(r)),
                }
            };
            if let Some(r) = next
                && let Some(path) = ls.path_at(r, c.paths)
            {
                if shift {
                    ls.select_range(r, c.paths, false);
                } else {
                    ls.select_only(path, r);
                }
                ls.scroll_to_row = Some(r);
                out.push(Ev::Activate);
            }
        }
        if enter {
            out.push(Ev::FocusDiff);
        }
        if all {
            let paths: Vec<String> = ls
                .model
                .rows
                .iter()
                .filter_map(|r| match r {
                    Row::File { entry, .. } => c.paths.get(*entry as usize).cloned(),
                    Row::Dir { .. } => None,
                })
                .collect();
            ls.selected = paths.into_iter().collect();
            out.push(Ev::Activate);
        }
    }

    // ---- context menu opened from the keyboard (Shift+F10): the same menu,
    // for the selection (or the cursor file), anchored below the cursor row.
    let targets = ls.targets();
    if !targets.is_empty() {
        let pos = menu_pos.unwrap_or(bg.rect.left_top());
        crate::views::history::keys::keyboard_menu(
            &bg,
            list_id.with("kb-menu"),
            kb_menu_open,
            pos,
            |ui| file_menu(ui, c, &targets, out),
        );
    }
}

/// Keymap actions of the file lists (see `keymap.rs`, scope `FileList`).
const FILE_ACTS: [Act; 6] = [
    Act::FileToggle,
    Act::FileDiscard,
    Act::FileHistory,
    Act::FileBlame,
    Act::FileExternalDiff,
    Act::FileIgnore,
];

/// Runs a file-list shortcut on the selection (or the cursor file).
fn key_action(a: Act, ls: &ListState, c: &ListCtx, out: &mut Vec<Ev>) {
    let targets = ls.targets();
    if targets.is_empty() {
        return;
    }
    // One-file actions use the cursor file when several are selected.
    let one = match targets.as_slice() {
        [t] => Some(t.clone()),
        _ => ls.cursor.clone(),
    };
    let unstaged = c.side == Side::Unstaged;
    match a {
        Act::FileToggle => out.push(Ev::Toggle(targets)),
        Act::FileDiscard if unstaged => {
            let (untracked, tracked): (Vec<String>, Vec<String>) = targets
                .into_iter()
                .partition(|t| kind_of(c, t).is_some_and(|e| e.kind == Kind::Untracked));
            out.push(Ev::Discard { tracked, untracked });
        }
        Act::FileHistory => out.extend(one.map(Ev::History)),
        Act::FileBlame => out.extend(one.map(Ev::Blame)),
        Act::FileExternalDiff => out.extend(one.map(Ev::ExternalDiff)),
        Act::FileIgnore if unstaged => out.push(Ev::IgnoreCustom(ignore_suggestion(&targets))),
        _ => {}
    }
}

/// Prefilled pattern of the custom-ignore editor: `*.ext` when all files
/// share an extension, else the single file itself.
fn ignore_suggestion(targets: &[String]) -> String {
    let ext = |p: &str| {
        let file = p.rsplit('/').next().unwrap_or(p);
        file.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map(|(_, e)| e.to_string())
    };
    let shared = targets.first().and_then(|f| ext(f)).filter(|e| {
        targets
            .iter()
            .all(|p| ext(p).as_deref() == Some(e.as_str()))
    });
    match (shared, targets) {
        (Some(e), _) => format!("*.{e}"),
        (None, [one]) => format!("/{one}"),
        _ => String::new(),
    }
}

fn kind_of<'a>(c: &'a ListCtx, path: &str) -> Option<&'a Entry> {
    c.paths
        .binary_search_by(|p| p.as_str().cmp(path))
        .ok()
        .and_then(|i| c.entries.get(i))
}

fn file_menu(ui: &mut Ui, c: &ListCtx, targets: &[String], out: &mut Vec<Ev>) {
    let mut emit = |ui: &mut Ui, ev: Ev| {
        out.push(ev);
        ui.close();
    };
    let n = targets.len();
    let single = (n == 1).then(|| targets[0].clone());
    let (mut tracked, mut untracked, mut conflicts) = (Vec::new(), Vec::new(), 0usize);
    for t in targets {
        match kind_of(c, t).map(|e| e.kind) {
            Some(Kind::Untracked) => untracked.push(t.clone()),
            Some(Kind::Conflict(_)) => {
                conflicts += 1;
                tracked.push(t.clone());
            }
            _ => tracked.push(t.clone()),
        }
    }
    let label = |verb: &str| {
        if n == 1 {
            verb.to_string()
        } else {
            format!("{verb} {n} files")
        }
    };
    // Menu buttons show their keymap shortcut on the right.
    let ctx = ui.ctx().clone();
    let item = |text: String, act: Act| shortcut_button(&ctx, text, act);
    match c.side {
        Side::Unstaged => {
            let verb = if conflicts == n {
                "Mark resolved (stage)"
            } else {
                "Stage"
            };
            if ui.add(item(label(verb), Act::FileToggle)).clicked() {
                emit(ui, Ev::Toggle(targets.to_vec()));
            }
        }
        Side::Staged => {
            if ui.add(item(label("Unstage"), Act::FileToggle)).clicked() {
                emit(ui, Ev::Toggle(targets.to_vec()));
            }
        }
    }
    if c.side == Side::Unstaged && conflicts > 0 && conflicts == n {
        ui.separator();
        if ui.button("Resolve using mine").clicked() {
            emit(ui, Ev::ResolveOurs(targets.to_vec()));
        }
        if ui.button("Resolve using theirs").clicked() {
            emit(ui, Ev::ResolveTheirs(targets.to_vec()));
        }
        if ui.button("Mark resolved").clicked() {
            emit(ui, Ev::MarkResolved(targets.to_vec()));
        }
        if let Some(p) = &single {
            if ui.button("Resolve in merge editor").clicked() {
                emit(ui, Ev::MergeEditor(p.clone()));
            }
            let b = ui
                .add_enabled(
                    c.has_merge_tool,
                    egui::Button::new("Open in external merge tool"),
                )
                .on_disabled_hover_text("Configure an external merge tool in Settings");
            if b.clicked() {
                emit(ui, Ev::MergeTool(p.clone()));
            }
        }
    }
    ui.separator();
    if c.side == Side::Unstaged
        && ui
            .add(item(label("Discard") + "…", Act::FileDiscard))
            .clicked()
    {
        emit(
            ui,
            Ev::Discard {
                tracked: tracked.clone(),
                untracked: untracked.clone(),
            },
        );
    }
    if ui.button(label("Stash") + "…").clicked() {
        emit(ui, Ev::Stash(targets.to_vec()));
    }
    if c.side == Side::Unstaged {
        ui.menu_button("Ignore", |ui| {
            if let Some(p) = &single {
                if ui.button(format!("Ignore this file ({p})")).clicked() {
                    emit(ui, Ev::Ignore(vec![format!("/{p}")]));
                }
                let file = p.rsplit('/').next().unwrap_or(p);
                if let Some((_, ext)) = file.rsplit_once('.').filter(|(stem, _)| !stem.is_empty())
                    && ui.button(format!("Ignore all *.{ext} files")).clicked()
                {
                    emit(ui, Ev::Ignore(vec![format!("*.{ext}")]));
                }
                if let Some((dir, _)) = p.rsplit_once('/')
                    && ui.button(format!("Ignore everything in {dir}/")).clicked()
                {
                    emit(ui, Ev::Ignore(vec![format!("/{dir}/")]));
                }
            } else if ui.button(format!("Ignore {n} selected files")).clicked() {
                emit(
                    ui,
                    Ev::Ignore(targets.iter().map(|t| format!("/{t}")).collect()),
                );
            }
            ui.separator();
            if ui
                .add(item("Custom pattern…".into(), Act::FileIgnore))
                .clicked()
            {
                emit(ui, Ev::IgnoreCustom(ignore_suggestion(targets)));
            }
        });
    }
    ui.separator();
    if let Some(p) = &single {
        let b = ui
            .add_enabled(
                c.has_diff_tool,
                item("Open in external diff tool".into(), Act::FileExternalDiff),
            )
            .on_disabled_hover_text("Configure an external diff tool in Settings");
        if b.clicked() {
            emit(ui, Ev::ExternalDiff(p.clone()));
        }
        if ui.button("Open file").clicked() {
            emit(ui, Ev::Open(p.clone()));
        }
        if ui.button("Show in Explorer").clicked() {
            emit(ui, Ev::Reveal(p.clone()));
        }
    }
    if ui
        .button(if n == 1 { "Copy path" } else { "Copy paths" })
        .clicked()
    {
        emit(ui, Ev::Copy(targets.join("\n")));
    }
    if let Some(p) = &single {
        ui.separator();
        if ui
            .add(item("File history".into(), Act::FileHistory))
            .clicked()
        {
            emit(ui, Ev::History(p.clone()));
        }
        if ui.add(item("Blame".into(), Act::FileBlame)).clicked() {
            emit(ui, Ev::Blame(p.clone()));
        }
    }
}

/// A menu button with `act`'s current shortcut on the right.
fn shortcut_button(ctx: &egui::Context, text: String, act: Act) -> egui::Button<'static> {
    let sc = keymap::text(ctx, act);
    let b = egui::Button::new(text);
    if sc.is_empty() {
        b
    } else {
        b.shortcut_text(egui::RichText::new(sc).weak())
    }
}

fn dir_menu(ui: &mut Ui, c: &ListCtx, dir: &str, files: &[String], out: &mut Vec<Ev>) {
    let mut emit = |ui: &mut Ui, ev: Ev| {
        out.push(ev);
        ui.close();
    };
    let n = files.len();
    let verb = if c.side == Side::Staged {
        "Unstage"
    } else {
        "Stage"
    };
    if ui.button(format!("{verb} folder ({n} files)")).clicked() {
        emit(ui, Ev::Toggle(files.to_vec()));
    }
    ui.separator();
    if c.side == Side::Unstaged && ui.button("Discard folder…").clicked() {
        let (untracked, tracked): (Vec<String>, Vec<String>) = files
            .iter()
            .cloned()
            .partition(|f| kind_of(c, f).is_some_and(|e| e.kind == Kind::Untracked));
        emit(ui, Ev::Discard { tracked, untracked });
    }
    if ui.button("Stash folder…").clicked() {
        emit(ui, Ev::Stash(files.to_vec()));
    }
    if c.side == Side::Unstaged && ui.button(format!("Ignore {dir}/")).clicked() {
        emit(ui, Ev::Ignore(vec![format!("/{dir}/")]));
    }
    ui.separator();
    if ui.button("Show in Explorer").clicked() {
        emit(ui, Ev::Open(dir.to_string()));
    }
    if ui.button("Copy path").clicked() {
        emit(ui, Ev::Copy(dir.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(paths: &[String], tree: bool) -> ListState {
        let mut ls = ListState::default();
        ls.sync(1, paths, &[], "", 0, tree);
        ls
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn range_selection_skips_dirs() {
        let paths = strs(&["a/x", "a/y", "b"]);
        let mut ls = state(&paths, true);
        // rows: a/, x, y, b
        assert_eq!(ls.model.rows.len(), 4);
        ls.select_only("a/x".into(), 1);
        ls.select_range(3, &paths, false);
        let mut sel: Vec<_> = ls.selected.iter().cloned().collect();
        sel.sort();
        assert_eq!(sel, strs(&["a/x", "a/y", "b"]));
        assert_eq!(ls.cursor.as_deref(), Some("b"));
    }

    #[test]
    fn cursor_moves_to_neighbor_when_file_vanishes() {
        let paths = strs(&["a", "b", "c"]);
        let mut ls = state(&paths, false);
        ls.select_only("b".into(), 1);
        let after = strs(&["a", "c"]);
        ls.sync(2, &after, &[], "", 0, false);
        assert!(ls.repair_cursor(&after));
        assert_eq!(ls.cursor.as_deref(), Some("c"));
        // Selection of vanished paths is dropped on status change.
        let last = strs(&["a"]);
        ls.sync(3, &last, &[], "", 0, false);
        ls.repair_cursor(&last);
        assert_eq!(ls.cursor.as_deref(), Some("a"));
        ls.sync(4, &[], &[], "", 0, false);
        ls.repair_cursor(&[]);
        assert!(ls.cursor.is_none());
    }

    #[test]
    fn drag_takes_selection_only_when_row_is_selected() {
        let paths = strs(&["a", "b", "c"]);
        let mut ls = state(&paths, false);
        assert_eq!(ls.drag_paths("b"), strs(&["b"]));
        ls.select_only("c".into(), 2);
        ls.selected.insert("a".into());
        assert_eq!(ls.drag_paths("c"), strs(&["a", "c"]));
        assert_eq!(ls.drag_paths("b"), strs(&["b"]), "unselected row");
    }

    #[test]
    fn dragging_a_folder_takes_its_files() {
        let paths = strs(&["a/x", "a/y/z", "b"]);
        let ls = state(&paths, true);
        let Some(Row::Dir { node, .. }) = ls.model.rows.first().cloned() else {
            panic!("first row is a folder");
        };
        let mut files = ls.dir_files(node, &paths);
        files.sort();
        assert_eq!(files, strs(&["a/x", "a/y/z"]));
    }

    #[test]
    fn filter_and_rebuild_key() {
        let paths = strs(&["src/main.rs", "README.md"]);
        let mut ls = state(&paths, false);
        assert!(!ls.sync(1, &paths, &[], "", 0, false));
        assert!(ls.sync(1, &paths, &[], "MAIN", 0, false));
        assert_eq!(ls.model.files, 1);
        ls.collapse_ver += 1;
        assert!(ls.sync(1, &paths, &[], "MAIN", 0, false));
    }
    #[test]
    fn status_chips_combine_with_text_filter() {
        let paths = strs(&["a.rs", "b.rs", "c.txt", "d.rs"]);
        let entries = vec![
            Entry::new("a.rs", Kind::Modified),
            Entry::new("b.rs", Kind::Untracked),
            Entry::new("c.txt", Kind::Untracked),
            Entry::new("d.rs", Kind::Deleted),
        ];
        let mut ls = ListState::default();
        ls.sync(1, &paths, &entries, "", 16, false);
        assert_eq!(ls.model.files, 2, "untracked only");
        assert!(ls.sync(1, &paths, &entries, "rs", 16, false));
        assert_eq!(ls.model.files, 1, "untracked .rs");
        ls.sync(1, &paths, &entries, "", 1 | 4, false);
        assert_eq!(ls.model.files, 2, "modified + deleted");
        ls.sync(1, &paths, &entries, "", 32, false);
        assert_eq!(ls.model.files, 0);
    }

    #[test]
    fn typing_narrows_and_backspace_widens() {
        let paths = strs(&["a.rs", "b.rs", "c.txt", "src/d.rs"]);
        let mut ls = state(&paths, false);
        ls.sync(1, &paths, &[], "s", 0, false);
        assert_eq!(ls.model.files, 3);
        ls.sync(1, &paths, &[], "rs", 0, false);
        assert_eq!(ls.model.files, 3);
        ls.sync(1, &paths, &[], "rs$", 0, false);
        assert_eq!(ls.model.files, 3);
        ls.sync(1, &paths, &[], "src rs$", 0, false);
        assert_eq!(ls.model.files, 1);
        ls.sync(1, &paths, &[], "t", 0, false);
        assert_eq!(ls.model.files, 1, "c.txt");
        // A new status never reuses matches of the old one.
        let more = strs(&["a.rs", "t.rs"]);
        ls.sync(2, &more, &[], "t", 0, false);
        assert_eq!(ls.model.files, 1, "t.rs");
        // Tree mode toggles keep the matches.
        assert!(ls.sync(2, &more, &[], "t", 0, true));
        assert_eq!(ls.model.files, 1);
    }

    fn ctx_for<'a>(
        side: Side,
        paths: &'a [String],
        entries: &'a [Entry],
        pending: &'a dyn Fn(&str) -> bool,
    ) -> ListCtx<'a> {
        ListCtx {
            side,
            entries,
            paths,
            palette: Palette::dark(),
            tree: false,
            active: true,
            pending,
            has_merge_tool: false,
            dnd_scope: Id::new("t"),
            has_diff_tool: true,
            row_pad: 6.0,
        }
    }

    #[test]
    fn keyboard_actions_on_selection_and_cursor() {
        let paths = strs(&["a.rs", "b.rs", "new.txt"]);
        let entries = vec![
            Entry::new("a.rs", Kind::Modified),
            Entry::new("b.rs", Kind::Modified),
            Entry::new("new.txt", Kind::Untracked),
        ];
        let no = |_: &str| false;
        let c = ctx_for(Side::Unstaged, &paths, &entries, &no);
        let mut ls = state(&paths, false);
        let run = |ls: &ListState, a: Act, c: &ListCtx| {
            let mut out = Vec::new();
            key_action(a, ls, c, &mut out);
            out
        };
        assert!(run(&ls, Act::FileToggle, &c).is_empty(), "nothing selected");
        ls.select_only("b.rs".into(), 1);
        ls.selected.insert("new.txt".into());
        assert!(
            matches!(&run(&ls, Act::FileToggle, &c)[..], [Ev::Toggle(p)] if *p == strs(&["b.rs", "new.txt"]))
        );
        assert!(matches!(
            &run(&ls, Act::FileDiscard, &c)[..],
            [Ev::Discard { tracked, untracked }] if *tracked == strs(&["b.rs"]) && *untracked == strs(&["new.txt"])
        ));
        // One-file actions take the cursor when several files are selected.
        assert!(matches!(&run(&ls, Act::FileHistory, &c)[..], [Ev::History(p)] if p == "b.rs"));
        assert!(matches!(&run(&ls, Act::FileBlame, &c)[..], [Ev::Blame(p)] if p == "b.rs"));
        assert!(
            matches!(&run(&ls, Act::FileExternalDiff, &c)[..], [Ev::ExternalDiff(p)] if p == "b.rs")
        );
        assert!(
            matches!(&run(&ls, Act::FileIgnore, &c)[..], [Ev::IgnoreCustom(s)] if s.is_empty())
        );
        // The staged list neither discards nor ignores.
        let staged = ctx_for(Side::Staged, &paths, &entries, &no);
        assert!(run(&ls, Act::FileDiscard, &staged).is_empty());
        assert!(run(&ls, Act::FileIgnore, &staged).is_empty());
        // Global actions are not handled here.
        assert!(run(&ls, Act::Fetch, &c).is_empty());
    }

    #[test]
    fn ignore_suggestions() {
        assert_eq!(ignore_suggestion(&strs(&["a/b.log"])), "*.log");
        assert_eq!(ignore_suggestion(&strs(&["a/b.log", "c.log"])), "*.log");
        assert_eq!(ignore_suggestion(&strs(&["Makefile"])), "/Makefile");
        assert_eq!(ignore_suggestion(&strs(&["a.log", "b.txt"])), "");
        assert_eq!(ignore_suggestion(&strs(&[".env"])), "/.env");
        assert_eq!(ignore_suggestion(&[]), "");
    }

    /// Whole row-model rebuild (flat list) per keystroke on 1M paths:
    /// `cargo test --release -p gk-app -- --ignored --nocapture sync_1m`
    #[test]
    #[ignore]
    fn sync_1m_paths_per_keystroke() {
        let mut paths: Vec<String> = (0..1_000_000)
            .map(|i| format!("src/module_{:03}/sub_{:02}/file_{i:07}.rs", i % 997, i % 37))
            .collect();
        paths.sort();
        let mut ls = ListState::default();
        ls.sync(1, &paths, &[], "", 0, false);
        let typed = "module_12 sub_3 rs";
        let mut worst = 0.0f64;
        for end in (1..=typed.len()).chain((1..typed.len()).rev()) {
            let q = &typed[..end];
            let t = std::time::Instant::now();
            ls.sync(1, &paths, &[], q, 0, false);
            ls.repair_cursor(&paths);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            println!("{q:<20} {ms:>7.1} ms  {} rows", ls.model.rows.len());
            worst = worst.max(ms);
        }
        println!("worst keystroke: {worst:.1} ms");
        assert!(worst < 50.0);
    }
}
