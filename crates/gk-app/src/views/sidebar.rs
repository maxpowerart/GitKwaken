//! Sidebar: workspace views, branches, remotes, tags, stashes, submodules and
//! worktrees, SourceTree style.
//!
//! Everything is flattened into one list of fixed-height rows drawn with
//! `ScrollArea::show_rows`, so only the visible rows cost anything even with
//! tens of thousands of refs. The derived trees are rebuilt only when the
//! refs snapshot or the filter changes; the flat row list only when that or
//! the collapsed state changes.

mod model;

use self::model::{SubState, Submodule, Tree, Worktree};
use crate::app::{Action, Shared};
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use crate::theme::Palette;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, TextFormat, Ui, pos2, vec2};
use gk_core::refs::Refs;
use gk_core::{Repo, Snapshot};
use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

const INDENT: f32 = 12.0;
const ARROW_W: f32 = 12.0;
const ICON_W: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Workspace,
    Branches,
    Remotes,
    Tags,
    Stashes,
    Submodules,
    Worktrees,
}

impl Section {
    const ALL: [Section; 7] = [
        Section::Workspace,
        Section::Branches,
        Section::Remotes,
        Section::Tags,
        Section::Stashes,
        Section::Submodules,
        Section::Worktrees,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Workspace => "WORKSPACE",
            Section::Branches => "BRANCHES",
            Section::Remotes => "REMOTES",
            Section::Tags => "TAGS",
            Section::Stashes => "STASHES",
            Section::Submodules => "SUBMODULES",
            Section::Worktrees => "WORKTREES",
        }
    }

    /// Key in `RepoState::sidebar_collapsed`.
    fn key(self) -> &'static str {
        match self {
            Section::Workspace => "section:workspace",
            Section::Branches => "section:branches",
            Section::Remotes => "section:remotes",
            Section::Tags => "section:tags",
            Section::Stashes => "section:stashes",
            Section::Submodules => "section:submodules",
            Section::Worktrees => "section:worktrees",
        }
    }
}

// ------------------------------------------------------------------ state

struct RemoteGroup {
    name: String,
    /// Index into `refs.remotes`; `None` for leftovers of a removed remote.
    idx: Option<u32>,
    tree: Tree,
    count: usize,
}

/// Trees and orderings derived from one refs snapshot and filter.
struct Derived {
    refs: Arc<Refs>,
    needle: String,
    branches: Tree,
    branch_count: usize,
    remotes: Vec<RemoteGroup>,
    tags: Vec<u32>,
    stashes: Vec<u32>,
}

type Slot<T> = Arc<Mutex<Option<Result<Vec<T>, String>>>>;

/// A list loaded on a background thread, reloaded when the refs change.
struct Lazy<T> {
    data: Option<Result<Vec<T>, String>>,
    pending: Option<Slot<T>>,
    /// Refs snapshot current when the last load started.
    refs: Option<Arc<Refs>>,
}

impl<T> Default for Lazy<T> {
    fn default() -> Self {
        Self {
            data: None,
            pending: None,
            refs: None,
        }
    }
}

impl<T: Send + 'static> Lazy<T> {
    /// Takes a finished load; true when new data arrived.
    fn poll(&mut self) -> bool {
        let done = self.pending.as_ref().and_then(|s| s.lock().take());
        match done {
            Some(r) => {
                self.data = Some(r);
                self.pending = None;
                true
            }
            None => false,
        }
    }

    fn ensure(
        &mut self,
        refs: &Arc<Refs>,
        ctx: &egui::Context,
        repo: &Repo,
        load: fn(&Repo) -> Result<Vec<T>, String>,
    ) {
        if self.pending.is_some() {
            return;
        }
        if self.data.is_some() && self.refs.as_ref().is_some_and(|r| Arc::ptr_eq(r, refs)) {
            return;
        }
        self.refs = Some(refs.clone());
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        self.pending = Some(slot.clone());
        let repo = repo.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("gk-sidebar".into())
            .spawn(move || {
                let r = load(&repo);
                *slot.lock() = Some(r);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.pending = None;
            self.data = Some(Err(e.to_string()));
        }
    }

    fn invalidate(&mut self) {
        self.refs = None;
    }

    fn items(&self) -> &[T] {
        match &self.data {
            Some(Ok(v)) => v,
            _ => &[],
        }
    }
}

enum SidebarModal {
    AddWorktree { path: String, branch: String },
    RemoveWorktree { path: PathBuf, force: bool },
}

#[derive(Default)]
pub struct SidebarState {
    pub filter: String,
    /// Commit id the history view should select next ("Show in history",
    /// clicking a ref). The history view takes it (`Option::take`).
    pub jump_to: Option<String>,
    selected: Option<String>,
    derived: Option<Derived>,
    /// Tags sorted by name descending, version-aware; per refs snapshot.
    tag_order: Option<(Arc<Refs>, Arc<Vec<u32>>)>,
    rows: Vec<Row>,
    rows_valid: bool,
    submodules: Lazy<Submodule>,
    worktrees: Lazy<Worktree>,
    modal: Option<SidebarModal>,
}

// ------------------------------------------------------------------- rows

enum RowKind {
    Header(Section),
    Workspace(View),
    Folder {
        key: String,
        label: String,
    },
    /// Index into `Derived::remotes`.
    Remote(u32),
    Branch(u32),
    RemoteBranch(u32),
    Tag(u32),
    Stash(u32),
    Submodule(u32),
    Worktree(u32),
    AddWorktree,
    Note(String),
}

struct Row {
    depth: u8,
    open: bool,
    kind: RowKind,
}

impl Row {
    fn new(depth: u8, kind: RowKind) -> Self {
        Self {
            depth,
            open: false,
            kind,
        }
    }
}

fn derive(refs: &Arc<Refs>, needle: &str, tag_order: &[u32]) -> Derived {
    use model::{build_tree, matches};
    let branches: Vec<(u32, &str)> = refs
        .branches
        .iter()
        .enumerate()
        .filter(|(_, b)| matches(&b.name, needle))
        .map(|(i, b)| (i as u32, b.name.as_str()))
        .collect();
    let branch_count = branches.len();

    /// Remote name -> (index into `refs.remotes`, matching branches).
    type Groups<'a> = BTreeMap<&'a str, (Option<u32>, Vec<(u32, &'a str)>)>;
    let mut groups: Groups = BTreeMap::new();
    for (i, r) in refs.remotes.iter().enumerate() {
        groups.entry(r.name.as_str()).or_default().0 = Some(i as u32);
    }
    for (i, rb) in refs.remote_branches.iter().enumerate() {
        if needle.is_empty() || matches(&rb.full(), needle) {
            groups
                .entry(rb.remote.as_str())
                .or_default()
                .1
                .push((i as u32, rb.name.as_str()));
        } else {
            groups.entry(rb.remote.as_str()).or_default();
        }
    }
    let remotes = groups
        .into_iter()
        .filter(|(name, (_, v))| needle.is_empty() || !v.is_empty() || matches(name, needle))
        .map(|(name, (idx, v))| RemoteGroup {
            name: name.to_string(),
            idx,
            count: v.len(),
            tree: build_tree(v),
        })
        .collect();

    let tags = tag_order
        .iter()
        .copied()
        .filter(|&i| matches(&refs.tags[i as usize].name, needle))
        .collect();
    let stashes = refs
        .stashes
        .iter()
        .enumerate()
        .filter(|(_, s)| needle.is_empty() || matches(&stash_label(s), needle))
        .map(|(i, _)| i as u32)
        .collect();
    Derived {
        refs: refs.clone(),
        needle: needle.to_string(),
        branches: build_tree(branches),
        branch_count,
        remotes,
        tags,
        stashes,
    }
}

fn stash_label(s: &gk_core::refs::Stash) -> String {
    format!("stash@{{{}}}: {}", s.index, s.message)
}

fn flatten(
    tree: &Tree,
    depth: u8,
    prefix: &str,
    collapsed: &BTreeSet<String>,
    force_open: bool,
    leaf: fn(u32) -> RowKind,
    out: &mut Vec<Row>,
) {
    for (name, sub) in &tree.folders {
        let key = format!("{prefix}/{name}");
        let open = force_open || !collapsed.contains(&key);
        out.push(Row {
            depth,
            open,
            kind: RowKind::Folder {
                key: key.clone(),
                label: name.clone(),
            },
        });
        if open {
            flatten(
                sub,
                depth.saturating_add(1),
                &key,
                collapsed,
                force_open,
                leaf,
                out,
            );
        }
    }
    out.extend(tree.leaves.iter().map(|&i| Row::new(depth, leaf(i))));
}

fn lazy_rows<T>(
    lazy: &Lazy<T>,
    keep: impl Fn(&T) -> bool,
    kind: fn(u32) -> RowKind,
    empty: &str,
    out: &mut Vec<Row>,
) {
    match &lazy.data {
        None => out.push(Row::new(1, RowKind::Note("Loading…".into()))),
        Some(Err(e)) => out.push(Row::new(
            1,
            RowKind::Note(e.lines().next().unwrap_or("failed").to_string()),
        )),
        Some(Ok(v)) => {
            let before = out.len();
            out.extend(
                v.iter()
                    .enumerate()
                    .filter(|(_, x)| keep(x))
                    .map(|(i, _)| Row::new(1, kind(i as u32))),
            );
            if out.len() == before && !empty.is_empty() {
                out.push(Row::new(1, RowKind::Note(empty.into())));
            }
        }
    }
}

fn build_rows(st: &SidebarState, collapsed: &BTreeSet<String>) -> Vec<Row> {
    let Some(d) = &st.derived else {
        return Vec::new();
    };
    let filtering = !d.needle.is_empty();
    let needle = d.needle.as_str();
    let mut out = Vec::new();
    for s in Section::ALL {
        let open = !collapsed.contains(s.key());
        out.push(Row {
            depth: 0,
            open,
            kind: RowKind::Header(s),
        });
        if !open {
            continue;
        }
        match s {
            Section::Workspace => {
                for v in [View::FileStatus, View::History, View::Search] {
                    out.push(Row::new(1, RowKind::Workspace(v)));
                }
            }
            Section::Branches => flatten(
                &d.branches,
                1,
                "b",
                collapsed,
                filtering,
                RowKind::Branch,
                &mut out,
            ),
            Section::Remotes => {
                for (gi, g) in d.remotes.iter().enumerate() {
                    let key = format!("r/{}", g.name);
                    let open = filtering || !collapsed.contains(&key);
                    out.push(Row {
                        depth: 1,
                        open,
                        kind: RowKind::Remote(gi as u32),
                    });
                    if open {
                        flatten(
                            &g.tree,
                            2,
                            &key,
                            collapsed,
                            filtering,
                            RowKind::RemoteBranch,
                            &mut out,
                        );
                    }
                }
            }
            Section::Tags => out.extend(d.tags.iter().map(|&i| Row::new(1, RowKind::Tag(i)))),
            Section::Stashes => {
                out.extend(d.stashes.iter().map(|&i| Row::new(1, RowKind::Stash(i))))
            }
            Section::Submodules => lazy_rows(
                &st.submodules,
                |m| model::matches(&m.path, needle),
                RowKind::Submodule,
                if filtering { "" } else { "No submodules" },
                &mut out,
            ),
            Section::Worktrees => {
                lazy_rows(
                    &st.worktrees,
                    |w| {
                        model::matches(&w.path.to_string_lossy(), needle)
                            || w.branch
                                .as_deref()
                                .is_some_and(|b| model::matches(b, needle))
                    },
                    RowKind::Worktree,
                    "",
                    &mut out,
                );
                out.push(Row::new(1, RowKind::AddWorktree));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- loaders

fn load_submodules(repo: &Repo) -> Result<Vec<Submodule>, String> {
    if !repo.workdir().join(".gitmodules").exists() {
        return Ok(Vec::new());
    }
    let out = repo
        .git()
        .run(&["submodule", "status", "--recursive"])
        .map_err(|e| format!("{e:#}"))?;
    Ok(model::parse_submodules(&out.stdout_str()))
}

fn load_worktrees(repo: &Repo) -> Result<Vec<Worktree>, String> {
    let out = repo
        .git()
        .run(&["worktree", "list", "--porcelain"])
        .map_err(|e| format!("{e:#}"))?;
    Ok(model::parse_worktrees(&out.stdout_str()))
}

// --------------------------------------------------------------- commands

/// What a row interaction asks for; applied after rendering.
enum Cmd {
    Toggle(String),
    SetView(View),
    Select(String),
    Jump(String),
    Dialog(Box<Dialog>),
    Checkout(String),
    Fetch(String),
    Copy(String),
    OpenRepo(PathBuf),
    Explore(PathBuf),
    SubmoduleUpdate(String),
    AddWorktree,
    RemoveWorktree(PathBuf),
    Reload,
}

fn dialog(d: Dialog) -> Cmd {
    Cmd::Dialog(Box::new(d))
}

fn worktree_dirty(snap: &Snapshot) -> bool {
    !snap.status.staged.is_empty()
        || snap
            .status
            .unstaged
            .iter()
            .any(|e| !matches!(e.kind, gk_status::Kind::Untracked))
}

fn apply(cmd: Cmd, ui: &Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let st = &mut tab.sidebar;
    match cmd {
        Cmd::Toggle(key) => {
            let set = &mut tab.state.sidebar_collapsed;
            if !set.remove(&key) {
                set.insert(key);
            }
            st.rows_valid = false;
        }
        Cmd::SetView(v) => tab.view = v,
        Cmd::Select(k) => st.selected = Some(k),
        Cmd::Jump(id) => {
            st.jump_to = Some(id);
            tab.view = View::History;
        }
        Cmd::Dialog(d) => tab.dialog = Some(*d),
        Cmd::Checkout(name) => {
            if snap.refs.current_branch().is_some_and(|b| b.name == name) {
                return;
            }
            if worktree_dirty(snap) {
                tab.dialog = Some(Dialog::checkout(name));
            } else {
                tab.session.run_op("Checkout", move |r, _, _| {
                    r.checkout(&name).map(|_| String::new())
                });
            }
        }
        Cmd::Fetch(remote) => {
            let opts = gk_core::ops::FetchOptions {
                prune: true,
                ..Default::default()
            };
            tab.session
                .run_op(format!("Fetch {remote}"), move |r, c, p| {
                    r.fetch(Some(&remote), &opts, c, p)
                });
        }
        Cmd::Copy(s) => ui.ctx().copy_text(s),
        Cmd::OpenRepo(p) => shared.actions.push(Action::OpenRepo(p)),
        Cmd::Explore(p) => {
            let _ = open::that(p);
        }
        Cmd::SubmoduleUpdate(path) => {
            tab.session
                .run_op(format!("Update submodule {path}"), move |r, c, p| {
                    let args = [
                        "submodule",
                        "update",
                        "--init",
                        "--recursive",
                        "--",
                        path.as_str(),
                    ];
                    r.git().run_streaming(&args, None, c, p)?.check()?;
                    Ok(String::new())
                });
        }
        Cmd::AddWorktree => {
            st.modal = Some(SidebarModal::AddWorktree {
                path: String::new(),
                branch: String::new(),
            })
        }
        Cmd::RemoveWorktree(path) => {
            st.modal = Some(SidebarModal::RemoveWorktree { path, force: false })
        }
        Cmd::Reload => {
            st.submodules.invalidate();
            st.worktrees.invalidate();
        }
    }
}

// -------------------------------------------------------------- rendering

struct Cx<'a> {
    snap: &'a Snapshot,
    d: &'a Derived,
    st: &'a SidebarState,
    pal: Palette,
    view: View,
    workdir: &'a std::path::Path,
    font: FontId,
    small: FontId,
    text: Color32,
    strong: Color32,
}

fn menu_item(ui: &mut Ui, enabled: bool, text: &str) -> bool {
    let clicked = ui.add_enabled(enabled, egui::Button::new(text)).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn paint_arrow(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let s = 3.5;
    let pts = if open {
        vec![
            pos2(c.x - s, c.y - s * 0.55),
            pos2(c.x + s, c.y - s * 0.55),
            pos2(c.x, c.y + s * 0.75),
        ]
    } else {
        vec![
            pos2(c.x - s * 0.55, c.y - s),
            pos2(c.x - s * 0.55, c.y + s),
            pos2(c.x + s * 0.75, c.y),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Paints single-line text truncated to `max_w`, vertically centered on `y`; returns its width.
#[allow(clippy::too_many_arguments)]
fn paint_text(
    p: &egui::Painter,
    x: f32,
    y: f32,
    max_w: f32,
    s: &str,
    font: &FontId,
    color: Color32,
    bold: bool,
) -> f32 {
    if max_w <= 4.0 || s.is_empty() {
        return 0.0;
    }
    let mut job = LayoutJob::single_section(s.to_owned(), TextFormat::simple(font.clone(), color));
    job.wrap = TextWrapping::truncate_at_width(max_w);
    let g = p.layout_job(job);
    let w = g.size().x;
    let pos = pos2(x, y - g.size().y / 2.0);
    if bold {
        p.galley(pos + vec2(0.6, 0.0), g.clone(), color);
    }
    p.galley(pos, g, color);
    w
}

/// Visual content of one row.
struct Look<'a> {
    arrow: Option<bool>,
    marker: Option<Color32>,
    icon: Option<(&'static str, Color32)>,
    text: &'a str,
    color: Color32,
    bold: bool,
    badges: Vec<(String, Color32)>,
    selected: bool,
}

fn paint_row(ui: &Ui, cx: &Cx, rect: Rect, depth: u8, hovered: bool, look: Look) {
    let p = ui.painter_at(rect);
    if look.selected {
        p.rect_filled(rect, 2.0, cx.pal.selection);
    } else if hovered {
        p.rect_filled(
            rect,
            2.0,
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(0.6),
        );
    }
    let y = rect.center().y;
    let mut x = rect.left() + 2.0 + depth as f32 * INDENT;
    if let Some(open) = look.arrow {
        paint_arrow(&p, pos2(x + ARROW_W / 2.0, y), open, cx.pal.muted);
    } else if let Some(c) = look.marker {
        p.circle_filled(pos2(x + ARROW_W / 2.0, y), 3.0, c);
    }
    x += ARROW_W;
    if let Some((icon, color)) = look.icon {
        paint_text(&p, x, y, ICON_W, icon, &cx.font, color, false);
        x += ICON_W;
    }
    // Badges from the right.
    let mut right = rect.right() - 4.0;
    for (b, color) in look.badges.iter().rev() {
        let g = p.layout_no_wrap(b.clone(), cx.small.clone(), *color);
        let w = g.size().x;
        if right - w < x + 24.0 {
            break;
        }
        p.galley(pos2(right - w, y - g.size().y / 2.0), g, *color);
        right -= w + 5.0;
    }
    paint_text(
        &p,
        x,
        y,
        right - x - 2.0,
        look.text,
        &cx.font,
        look.color,
        look.bold,
    );
}

fn row_ui(ui: &mut Ui, cx: &Cx, row: &Row, h: f32, cmds: &mut Vec<Cmd>) {
    let w = ui.available_width().max(40.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let snap = cx.snap;
    let refs = &*cx.d.refs;
    let pal = cx.pal;
    let sel = |k: &str| cx.st.selected.as_deref() == Some(k);
    let hovered = resp.hovered();
    let base = Look {
        arrow: None,
        marker: None,
        icon: None,
        text: "",
        color: cx.text,
        bold: false,
        badges: Vec::new(),
        selected: false,
    };
    match &row.kind {
        RowKind::Header(s) => {
            let count = match s {
                Section::Workspace => None,
                Section::Branches => Some(cx.d.branch_count),
                Section::Remotes => Some(cx.d.remotes.len()),
                Section::Tags => Some(cx.d.tags.len()),
                Section::Stashes => Some(cx.d.stashes.len()),
                Section::Submodules => cx
                    .st
                    .submodules
                    .data
                    .as_ref()
                    .map(|_| cx.st.submodules.items().len()),
                Section::Worktrees => cx
                    .st
                    .worktrees
                    .data
                    .as_ref()
                    .map(|_| cx.st.worktrees.items().len()),
            };
            let plus = matches!(s, Section::Remotes | Section::Worktrees);
            let mut badges = Vec::new();
            if let Some(n) = count {
                badges.push((n.to_string(), pal.muted));
            }
            if plus {
                badges.push(("    ".to_string(), pal.muted));
            }
            paint_row(
                ui,
                cx,
                rect,
                0,
                false,
                Look {
                    arrow: Some(row.open),
                    text: s.title(),
                    color: pal.muted,
                    bold: true,
                    badges,
                    ..base
                },
            );
            let mut plus_clicked = false;
            if plus {
                let pr = Rect::from_center_size(
                    pos2(rect.right() - 12.0, rect.center().y),
                    vec2(16.0, h - 2.0),
                );
                let r = ui.interact(pr, ui.id().with(("sidebar-plus", s.key())), Sense::click());
                let color = if r.hovered() { cx.strong } else { pal.muted };
                if r.hovered() {
                    ui.painter()
                        .rect_filled(pr, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                ui.painter().text(
                    pr.center(),
                    egui::Align2::CENTER_CENTER,
                    "+",
                    cx.font.clone(),
                    color,
                );
                let tip = if *s == Section::Remotes {
                    "Add remote…"
                } else {
                    "Add worktree…"
                };
                if r.on_hover_text(tip).clicked() {
                    plus_clicked = true;
                    cmds.push(if *s == Section::Remotes {
                        dialog(Dialog::add_remote())
                    } else {
                        Cmd::AddWorktree
                    });
                }
            }
            if resp.clicked() && !plus_clicked {
                cmds.push(Cmd::Toggle(s.key().to_string()));
            }
            resp.context_menu(|ui| header_menu(ui, *s, snap, cmds));
        }
        RowKind::Workspace(v) => {
            let (icon, text) = match v {
                View::FileStatus => ("📝", "File status"),
                View::History => ("🕘", "History"),
                View::Search => ("🔍", "Search"),
            };
            let mut badges = Vec::new();
            if *v == View::FileStatus {
                let n = snap.status.staged.len() + snap.status.unstaged.len();
                if n > 0 {
                    badges.push((n.to_string(), pal.modified));
                }
            }
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some((icon, pal.branch)),
                    text,
                    badges,
                    selected: cx.view == *v,
                    bold: cx.view == *v,
                    ..base
                },
            );
            if resp.clicked() {
                cmds.push(Cmd::SetView(*v));
            }
        }
        RowKind::Folder { key, label } => {
            let color = if key.starts_with("r/") {
                pal.remote
            } else {
                pal.branch
            };
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    arrow: Some(row.open),
                    icon: Some(("📁", color)),
                    text: label,
                    ..base
                },
            );
            if resp.clicked() && cx.d.needle.is_empty() {
                cmds.push(Cmd::Toggle(key.clone()));
            }
        }
        RowKind::Remote(gi) => {
            let g = &cx.d.remotes[*gi as usize];
            let remote = g.idx.map(|i| &refs.remotes[i as usize]);
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    arrow: Some(row.open),
                    icon: Some(("☁", pal.remote)),
                    text: &g.name,
                    badges: vec![(g.count.to_string(), pal.muted)],
                    ..base
                },
            );
            let resp = match remote {
                Some(r) if hovered => resp.on_hover_text(format!(
                    "{}\nfetch: {}\npush: {}",
                    r.name, r.fetch_url, r.push_url
                )),
                _ => resp,
            };
            if resp.clicked() && cx.d.needle.is_empty() {
                cmds.push(Cmd::Toggle(format!("r/{}", g.name)));
            }
            let name = g.name.clone();
            resp.context_menu(|ui| {
                if menu_item(ui, true, &format!("Fetch from {name}")) {
                    cmds.push(Cmd::Fetch(name.clone()));
                }
                ui.separator();
                if menu_item(ui, remote.is_some(), "Edit…") {
                    cmds.push(dialog(Dialog::edit_remote(name.clone())));
                }
                if menu_item(ui, remote.is_some(), "Remove…") {
                    cmds.push(dialog(Dialog::remove_remote(name.clone())));
                }
                ui.separator();
                if menu_item(ui, remote.is_some(), "Copy URL") {
                    cmds.push(Cmd::Copy(
                        remote.map(|r| r.fetch_url.clone()).unwrap_or_default(),
                    ));
                }
            });
        }
        RowKind::Branch(i) => {
            let b = &refs.branches[*i as usize];
            let key = format!("b:{}", b.name);
            let mut badges = Vec::new();
            if b.upstream_gone {
                badges.push(("gone".to_string(), pal.removed_fg));
            } else {
                if b.ahead > 0 {
                    badges.push((format!("↑{}", b.ahead), pal.added_fg));
                }
                if b.behind > 0 {
                    badges.push((format!("↓{}", b.behind), pal.modified));
                }
            }
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    marker: b.is_head.then_some(pal.head),
                    icon: Some(("⑂", if b.is_head { pal.head } else { pal.branch })),
                    text: model::leaf_name(&b.name),
                    color: if b.is_head { cx.strong } else { cx.text },
                    bold: b.is_head,
                    badges,
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                let up = match &b.upstream {
                    Some(u) if b.upstream_gone => format!("\nupstream: {u} (gone)"),
                    Some(u) => format!("\nupstream: {u}  ↑{} ↓{}", b.ahead, b.behind),
                    None => "\nno upstream".to_string(),
                };
                resp.on_hover_text(format!(
                    "{}\n{}{up}",
                    b.name,
                    crate::util::short_id(&b.target)
                ))
            } else {
                resp
            };
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
                cmds.push(Cmd::Jump(b.target.clone()));
            }
            if resp.double_clicked() {
                cmds.push(Cmd::Checkout(b.name.clone()));
            }
            resp.context_menu(|ui| branch_menu(ui, b, snap, cmds));
        }
        RowKind::RemoteBranch(i) => {
            let rb = &refs.remote_branches[*i as usize];
            let full = rb.full();
            let key = format!("rb:{full}");
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some(("⑂", pal.remote)),
                    text: model::leaf_name(&rb.name),
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                resp.on_hover_text(format!("{full}\n{}", crate::util::short_id(&rb.target)))
            } else {
                resp
            };
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
                cmds.push(Cmd::Jump(rb.target.clone()));
            }
            if resp.double_clicked() {
                cmds.push(dialog(Dialog::checkout_remote(full.clone())));
            }
            resp.context_menu(|ui| {
                if menu_item(ui, true, "Checkout as tracking branch…") {
                    cmds.push(dialog(Dialog::checkout_remote(full.clone())));
                }
                if menu_item(ui, true, &format!("Merge {full} into current")) {
                    cmds.push(dialog(Dialog::merge(snap, Some(full.clone()))));
                }
                if menu_item(ui, true, &format!("Rebase current onto {full}")) {
                    cmds.push(dialog(Dialog::rebase(snap, Some(full.clone()))));
                }
                if menu_item(ui, true, "New branch from here…") {
                    cmds.push(dialog(Dialog::branch(snap, Some(full.clone()))));
                }
                ui.separator();
                if menu_item(ui, true, "Delete on remote…") {
                    cmds.push(dialog(Dialog::delete_remote_branch(
                        rb.remote.clone(),
                        rb.name.clone(),
                    )));
                }
                ui.separator();
                if menu_item(ui, true, "Copy name") {
                    cmds.push(Cmd::Copy(full.clone()));
                }
                if menu_item(ui, true, "Show in history") {
                    cmds.push(Cmd::Jump(rb.target.clone()));
                }
            });
        }
        RowKind::Tag(i) => {
            let t = &refs.tags[*i as usize];
            let key = format!("t:{}", t.name);
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some(("🏷", pal.tag)),
                    text: &t.name,
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                let kind = if t.annotated {
                    "annotated"
                } else {
                    "lightweight"
                };
                resp.on_hover_text(format!(
                    "{}\n{} ({kind})",
                    t.name,
                    crate::util::short_id(&t.target)
                ))
            } else {
                resp
            };
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
                cmds.push(Cmd::Jump(t.target.clone()));
            }
            resp.context_menu(|ui| {
                if menu_item(ui, true, "Checkout…") {
                    cmds.push(dialog(Dialog::checkout(t.name.clone())));
                }
                if menu_item(ui, true, "Push to remote…") {
                    cmds.push(dialog(Dialog::push_tag(t.name.clone())));
                }
                if menu_item(ui, true, "Delete…") {
                    cmds.push(dialog(Dialog::delete_tag(t.name.clone())));
                }
                ui.separator();
                if menu_item(ui, true, "Copy name") {
                    cmds.push(Cmd::Copy(t.name.clone()));
                }
                if menu_item(ui, true, "Show in history") {
                    cmds.push(Cmd::Jump(t.target.clone()));
                }
            });
        }
        RowKind::Stash(i) => {
            let s = &refs.stashes[*i as usize];
            let key = format!("s:{}", s.index);
            let label = stash_label(s);
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some(("▣", pal.stash)),
                    text: &label,
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                resp.on_hover_text(&label)
            } else {
                resp
            };
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
            }
            if resp.double_clicked() {
                cmds.push(dialog(Dialog::stash_apply(s.index)));
            }
            let idx = s.index;
            resp.context_menu(|ui| {
                if menu_item(ui, true, "Apply…") {
                    cmds.push(dialog(Dialog::stash_apply(idx)));
                }
                if menu_item(ui, true, "Drop…") {
                    cmds.push(dialog(Dialog::stash_drop(idx)));
                }
                if menu_item(ui, true, "Branch from stash…") {
                    cmds.push(dialog(Dialog::stash_branch(idx)));
                }
                ui.separator();
                if menu_item(ui, true, "Copy message") {
                    cmds.push(Cmd::Copy(s.message.clone()));
                }
            });
        }
        RowKind::Submodule(i) => {
            let m = &cx.st.submodules.items()[*i as usize];
            let key = format!("sm:{}", m.path);
            let badge = match m.state {
                SubState::UpToDate => None,
                SubState::NotInitialized => Some(("not initialized", pal.muted)),
                SubState::Modified => Some(("modified", pal.modified)),
                SubState::Conflict => Some(("conflict", pal.conflict)),
            };
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some(("📦", pal.branch)),
                    text: &m.path,
                    badges: badge.map(|(t, c)| (t.to_string(), c)).into_iter().collect(),
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                let d = m
                    .describe
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                resp.on_hover_text(format!("{}\n{}{d}", m.path, crate::util::short_id(&m.id)))
            } else {
                resp
            };
            let abs = cx.workdir.join(&m.path);
            let initialized = m.state != SubState::NotInitialized;
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
            }
            if resp.double_clicked() && initialized {
                cmds.push(Cmd::OpenRepo(abs.clone()));
            }
            resp.context_menu(|ui| {
                if menu_item(ui, initialized, "Open") {
                    cmds.push(Cmd::OpenRepo(abs.clone()));
                }
                if menu_item(ui, true, "Update") {
                    cmds.push(Cmd::SubmoduleUpdate(m.path.clone()));
                }
                if menu_item(ui, initialized, "Show in Explorer") {
                    cmds.push(Cmd::Explore(abs.clone()));
                }
                ui.separator();
                if menu_item(ui, true, "Copy path") {
                    cmds.push(Cmd::Copy(m.path.clone()));
                }
            });
        }
        RowKind::Worktree(i) => {
            let wt = &cx.st.worktrees.items()[*i as usize];
            let key = format!("wt:{}", wt.path.display());
            let current = model::path_key(&wt.path) == model::path_key(cx.workdir);
            let main = *i == 0;
            let label = match (&wt.branch, &wt.head) {
                _ if wt.bare => "(bare)".to_string(),
                (Some(b), _) => b.clone(),
                (None, Some(h)) => format!("(detached {})", crate::util::short_id(h)),
                (None, None) => "(unknown)".to_string(),
            };
            let dir = wt
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut badges = vec![(dir, pal.muted)];
            if wt.locked {
                badges.push(("locked".into(), pal.modified));
            }
            if wt.prunable {
                badges.push(("prunable".into(), pal.removed_fg));
            }
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    marker: current.then_some(pal.head),
                    icon: Some(("🌲", if current { pal.head } else { pal.branch })),
                    text: &label,
                    color: if current { cx.strong } else { cx.text },
                    bold: current,
                    badges,
                    selected: sel(&key),
                    ..base
                },
            );
            let resp = if hovered {
                resp.on_hover_text(format!(
                    "{}{}",
                    wt.path.display(),
                    if main { "\nmain worktree" } else { "" }
                ))
            } else {
                resp
            };
            if resp.clicked() {
                cmds.push(Cmd::Select(key));
            }
            if resp.double_clicked() && !current && !wt.bare {
                cmds.push(Cmd::OpenRepo(wt.path.clone()));
            }
            resp.context_menu(|ui| {
                if menu_item(ui, !current && !wt.bare, "Open") {
                    cmds.push(Cmd::OpenRepo(wt.path.clone()));
                }
                if menu_item(ui, true, "Show in Explorer") {
                    cmds.push(Cmd::Explore(wt.path.clone()));
                }
                if menu_item(ui, !main && !current, "Remove…") {
                    cmds.push(Cmd::RemoveWorktree(wt.path.clone()));
                }
                ui.separator();
                if menu_item(ui, true, "Copy path") {
                    cmds.push(Cmd::Copy(wt.path.display().to_string()));
                }
                if menu_item(ui, true, "Add worktree…") {
                    cmds.push(Cmd::AddWorktree);
                }
            });
        }
        RowKind::AddWorktree => {
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                hovered,
                Look {
                    icon: Some(("+", pal.muted)),
                    text: "Add worktree…",
                    color: pal.muted,
                    ..base
                },
            );
            if resp.clicked() {
                cmds.push(Cmd::AddWorktree);
            }
        }
        RowKind::Note(t) => {
            paint_row(
                ui,
                cx,
                rect,
                row.depth,
                false,
                Look {
                    text: t,
                    color: pal.muted,
                    ..base
                },
            );
            if hovered && t.len() > 30 {
                resp.on_hover_text(t);
            }
        }
    }
}

fn header_menu(ui: &mut Ui, s: Section, snap: &Snapshot, cmds: &mut Vec<Cmd>) {
    match s {
        Section::Branches => {
            if menu_item(ui, true, "New branch…") {
                cmds.push(dialog(Dialog::branch(snap, None)));
            }
        }
        Section::Remotes => {
            if menu_item(ui, true, "Add remote…") {
                cmds.push(dialog(Dialog::add_remote()));
            }
            if menu_item(ui, !snap.refs.remotes.is_empty(), "Fetch…") {
                cmds.push(dialog(Dialog::fetch(snap)));
            }
        }
        Section::Tags => {
            if menu_item(ui, true, "New tag…") {
                cmds.push(dialog(Dialog::tag(None)));
            }
        }
        Section::Stashes => {
            if menu_item(ui, true, "Stash changes…") {
                cmds.push(dialog(Dialog::stash()));
            }
        }
        Section::Submodules => {
            if menu_item(ui, true, "Refresh") {
                cmds.push(Cmd::Reload);
            }
        }
        Section::Worktrees => {
            if menu_item(ui, true, "Add worktree…") {
                cmds.push(Cmd::AddWorktree);
            }
            if menu_item(ui, true, "Refresh") {
                cmds.push(Cmd::Reload);
            }
        }
        Section::Workspace => {
            ui.close();
        }
    }
}

fn branch_menu(ui: &mut Ui, b: &gk_core::refs::Branch, snap: &Snapshot, cmds: &mut Vec<Cmd>) {
    let name = &b.name;
    let other = !b.is_head;
    let on_branch = snap.refs.head.branch().is_some();
    if menu_item(ui, other, &format!("Checkout {name}")) {
        cmds.push(Cmd::Checkout(name.clone()));
    }
    if menu_item(
        ui,
        other && on_branch,
        &format!("Merge {name} into current"),
    ) {
        cmds.push(dialog(Dialog::merge(snap, Some(name.clone()))));
    }
    if menu_item(ui, other, &format!("Rebase current onto {name}")) {
        cmds.push(dialog(Dialog::rebase(snap, Some(name.clone()))));
    }
    ui.separator();
    if menu_item(ui, true, "Push to…") {
        cmds.push(dialog(Dialog::push(snap)));
    }
    if menu_item(ui, true, "Pull…") {
        cmds.push(dialog(Dialog::pull(snap)));
    }
    if menu_item(ui, true, "Track remote branch…") {
        cmds.push(dialog(Dialog::set_upstream(name.clone())));
    }
    ui.separator();
    if menu_item(ui, true, "New branch from here…") {
        cmds.push(dialog(Dialog::branch(snap, Some(name.clone()))));
    }
    if menu_item(ui, true, "Rename…") {
        cmds.push(dialog(Dialog::rename_branch(name.clone())));
    }
    if menu_item(ui, other, "Delete…") {
        cmds.push(dialog(Dialog::delete_branch(name.clone())));
    }
    ui.separator();
    if menu_item(ui, true, "Copy name") {
        cmds.push(Cmd::Copy(name.clone()));
    }
    if menu_item(ui, true, "Show in history") {
        cmds.push(Cmd::Jump(b.target.clone()));
    }
}

fn modal_ui(ui: &Ui, tab: &mut RepoTab, snap: &Snapshot) {
    let Some(modal) = tab.sidebar.modal.as_mut() else {
        return;
    };
    let mut close = false;
    let mut run: Option<(String, Vec<String>)> = None;
    let id = egui::Id::new(("sidebar-modal", &tab.path));
    let resp = egui::Modal::new(id).show(ui.ctx(), |ui| {
        ui.set_width(440.0);
        match modal {
            SidebarModal::AddWorktree { path, branch } => {
                ui.heading("Add worktree");
                ui.add_space(4.0);
                egui::Grid::new("add-wt")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Folder:");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(path)
                                    .desired_width(300.0)
                                    .hint_text("new, empty folder"),
                            );
                            if ui.button("Browse…").clicked()
                                && let Some(p) = rfd::FileDialog::new()
                                    .set_title("Worktree folder")
                                    .pick_folder()
                            {
                                *path = p.display().to_string();
                            }
                        });
                        ui.end_row();
                        ui.label("Branch:");
                        ui.add(
                            egui::TextEdit::singleline(branch)
                                .desired_width(300.0)
                                .hint_text("optional"),
                        );
                        ui.end_row();
                    });
                let b = branch.trim();
                let exists = !b.is_empty() && snap.refs.branch(b).is_some();
                let hint = if b.is_empty() {
                    "A new branch named after the folder is created from HEAD.".to_string()
                } else if exists {
                    format!("Checks out the existing branch '{b}'.")
                } else {
                    format!("Creates the new branch '{b}' from HEAD.")
                };
                ui.label(egui::RichText::new(hint).weak());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = !path.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new("Add")).clicked() {
                        let mut args = vec!["worktree".to_string(), "add".to_string()];
                        if !b.is_empty() && !exists {
                            args.extend(["-b".to_string(), b.to_string()]);
                        }
                        args.push(path.trim().to_string());
                        if exists {
                            args.push(b.to_string());
                        }
                        run = Some(("Add worktree".into(), args));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            SidebarModal::RemoveWorktree { path, force } => {
                ui.heading("Remove worktree");
                ui.label(format!("Remove the worktree at {}?", path.display()));
                ui.checkbox(force, "Force (discard local changes in the worktree)");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        let mut args = vec!["worktree".to_string(), "remove".to_string()];
                        if *force {
                            args.push("--force".into());
                        }
                        args.push(path.display().to_string());
                        run = Some(("Remove worktree".into(), args));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
        }
    });
    if resp.should_close() {
        close = true;
    }
    if let Some((label, args)) = run {
        tab.session.run_op(label, move |r, c, p| {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            r.git().run_streaming(&a, None, c, p)?.check()?;
            Ok(String::new())
        });
        tab.sidebar.worktrees.invalidate();
    }
    if close {
        tab.sidebar.modal = None;
    }
}

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    // Background loads that finished.
    {
        let st = &mut tab.sidebar;
        if st.submodules.poll() | st.worktrees.poll() {
            st.rows_valid = false;
        }
    }

    // Filter box.
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let st = &mut tab.sidebar;
        let clear = !st.filter.is_empty();
        let w = ui.available_width() - if clear { 24.0 } else { 0.0 };
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("🔍 Filter")
                .desired_width(w),
        );
        if clear && ui.small_button("✖").on_hover_text("Clear filter").clicked() {
            st.filter.clear();
        }
    });
    ui.add_space(2.0);

    // Derived trees: rebuilt when refs or the filter change.
    let refs = &snap.refs;
    let needle = tab.sidebar.filter.trim().to_lowercase();
    {
        let st = &mut tab.sidebar;
        let stale = st
            .derived
            .as_ref()
            .is_none_or(|d| !Arc::ptr_eq(&d.refs, refs) || d.needle != needle);
        if stale {
            let order = match &st.tag_order {
                Some((r, o)) if Arc::ptr_eq(r, refs) => o.clone(),
                _ => {
                    let tags = &refs.tags;
                    let mut o: Vec<u32> = (0..tags.len() as u32).collect();
                    o.sort_by(|&a, &b| {
                        model::version_cmp(&tags[b as usize].name, &tags[a as usize].name)
                    });
                    let o = Arc::new(o);
                    st.tag_order = Some((refs.clone(), o.clone()));
                    o
                }
            };
            st.derived = Some(derive(refs, &needle, &order));
            st.rows_valid = false;
        }
    }

    // Lazy sections load only while expanded.
    {
        let collapsed = &tab.state.sidebar_collapsed;
        let st = &mut tab.sidebar;
        let repo = tab.session.repo();
        if !collapsed.contains(Section::Submodules.key()) {
            st.submodules.ensure(refs, ui.ctx(), repo, load_submodules);
        }
        if !collapsed.contains(Section::Worktrees.key()) {
            st.worktrees.ensure(refs, ui.ctx(), repo, load_worktrees);
        }
    }

    if !tab.sidebar.rows_valid {
        tab.sidebar.rows = build_rows(&tab.sidebar, &tab.state.sidebar_collapsed);
        tab.sidebar.rows_valid = true;
    }

    // Rows.
    let mut cmds: Vec<Cmd> = Vec::new();
    {
        let st = &tab.sidebar;
        let Some(d) = &st.derived else { return };
        let style = ui.style().clone();
        let cx = Cx {
            snap,
            d,
            st,
            pal: shared.palette,
            view: tab.view,
            workdir: &tab.path,
            font: egui::TextStyle::Body.resolve(&style),
            small: egui::TextStyle::Small.resolve(&style),
            text: ui.visuals().text_color(),
            strong: ui.visuals().strong_text_color(),
        };
        let h = (ui.text_style_height(&egui::TextStyle::Body) + 4.0).max(16.0);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(ui, h, st.rows.len(), |ui, range| {
                    for row in &st.rows[range] {
                        row_ui(ui, &cx, row, h, &mut cmds);
                    }
                });
        });
    }
    for c in cmds {
        apply(c, ui, tab, snap, shared);
    }
    modal_ui(ui, tab, snap);
}
