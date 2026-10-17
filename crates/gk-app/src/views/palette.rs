//! Command palette (Ctrl+Shift+P): fuzzy-searchable list of every action,
//! plus entries for the open repository (checkout a branch, stash, worktree
//! and submodule actions, actions on the selected commits, blame).
//!
//! Matching uses nucleo-matcher (fzf syntax, like the file filter); exact
//! and prefix matches rank first, matched characters are highlighted.

use crate::app::{Action, Shared};
use crate::dialogs::{Dialog, GlobalDialog};
use crate::keymap::{self, Act};
use crate::tab::{RepoTab, View};
use crate::views::history::keys;
use egui::text::{LayoutJob, TextFormat};
use gk_core::Snapshot;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::path::PathBuf;

/// Rows shown at most.
const SHOWN: usize = 50;

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    query: String,
    selected: usize,
    focus: bool,
    /// Runs the best match automatically (GITKWAKEN_PALETTE, for UI tests).
    auto_run: bool,
}

impl PaletteState {
    /// Opens the palette with `query` and runs the best match on the next
    /// frame. Used by `GITKWAKEN_PALETTE=<text>` to script screenshots.
    pub fn run_on_start(query: String) -> Self {
        Self {
            open: true,
            query,
            selected: 0,
            focus: false,
            auto_run: true,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StashCmd {
    Apply,
    Pop,
    Drop,
    Branch,
}

/// What a palette entry (or a menu bar item, see `views::menubar`) does.
#[derive(Clone)]
pub(crate) enum Cmd {
    View(View),
    Dialog(fn(&Snapshot) -> Dialog),
    Global(fn() -> GlobalDialog),
    App(fn() -> Action),
    Refresh,
    Terminal,
    Explorer,
    ToggleOutput,
    Checkout(String),
    Merge(String),
    Blame(String),
    /// A dialog for the single selected commit (history, search or reflog).
    OnCommit(fn(&Snapshot, String) -> Dialog),
    /// A dialog for the selected commits, newest first.
    OnCommits(fn(&Snapshot, Vec<String>) -> Dialog),
    /// Blame the file shown in commit details, at the selected commit.
    BlameSelected,
    Stash(StashCmd, usize),
    /// Update one submodule, or all.
    SubmoduleUpdate(Option<String>),
    OpenRepo(PathBuf),
    AddWorktree,
    RemoveWorktree(PathBuf),
    /// Focus the commit message box (File Status).
    Commit,
    /// Stage (true) or unstage (false) everything.
    StageAll(bool),
    /// Copy the selected commits' ids (short ones when true).
    CopyShas(bool),
}

/// Selected commits of the active list, newest first.
fn selected_commits(t: &RepoTab, snap: &Snapshot) -> Vec<String> {
    let ids = match t.view {
        View::Search => t.search.sel.sorted(|k| t.search.row_of(snap, k)),
        View::Reflog => t.reflog.selected_id().into_iter().collect(),
        _ => {
            let f = &t.history.filter;
            if f.active() {
                t.history.sel.sorted(|k| f.row_of(k))
            } else {
                t.history.sel.sorted(|k| snap.history.row(k))
            }
        }
    };
    ids.iter().map(|i| i.to_string()).collect()
}

struct Item {
    label: String,
    /// Shortcut shown on the right ("" = none).
    hint: String,
    cmd: Cmd,
}

fn items(tab: Option<&RepoTab>, snap: Option<&Snapshot>, km: &keymap::Keymap) -> Vec<Item> {
    let mut v: Vec<Item> = Vec::new();
    let mut add = |label: &str, hint: String, cmd: Cmd| {
        v.push(Item {
            label: label.to_string(),
            hint,
            cmd,
        })
    };
    let k = |a: Act| km.text(a);
    let none = String::new;
    add(
        "Open repository…",
        k(Act::OpenRepo),
        Cmd::App(|| Action::OpenRepoDialog),
    );
    add(
        "Clone repository…",
        none(),
        Cmd::Global(GlobalDialog::clone_repo),
    );
    add(
        "Create repository…",
        none(),
        Cmd::Global(GlobalDialog::init_repo),
    );
    add(
        "Scan folder for repositories…",
        none(),
        Cmd::Global(GlobalDialog::scan_repos),
    );
    add(
        "Settings",
        k(Act::Settings),
        Cmd::Global(GlobalDialog::settings),
    );
    add("About GitKwaken", none(), Cmd::Global(GlobalDialog::about));
    add("Toggle output log", none(), Cmd::ToggleOutput);
    let (Some(snap), Some(tab)) = (snap, tab) else {
        return v;
    };
    add(
        "View: File status",
        k(Act::ViewStatus),
        Cmd::View(View::FileStatus),
    );
    add(
        "View: History",
        k(Act::ViewHistory),
        Cmd::View(View::History),
    );
    add("View: Search", k(Act::ViewSearch), Cmd::View(View::Search));
    add("View: Reflog", none(), Cmd::View(View::Reflog));
    add(
        "Undo last operation…",
        none(),
        Cmd::Dialog(|_| Dialog::undo()),
    );
    add(
        "Discarded changes… (restore)",
        none(),
        Cmd::Dialog(|_| Dialog::discarded()),
    );
    // Actions on the selected commit(s), as in the commit context menu.
    add(
        "Checkout selected commit…",
        keys::CHECKOUT_KEY.into(),
        Cmd::OnCommit(|_, id| Dialog::checkout(id)),
    );
    add(
        "New branch at selected commit…",
        keys::BRANCH_KEY.into(),
        Cmd::OnCommit(|s, id| Dialog::branch(s, Some(id))),
    );
    add(
        "New tag at selected commit…",
        keys::TAG_KEY.into(),
        Cmd::OnCommit(|_, id| Dialog::tag(Some(id))),
    );
    add(
        "Merge selected commit into current…",
        none(),
        Cmd::OnCommit(|s, id| Dialog::merge(s, Some(commit_rev(s, &id)))),
    );
    add(
        "Rebase current branch onto selected commit…",
        none(),
        Cmd::OnCommit(|s, id| Dialog::rebase(s, Some(commit_rev(s, &id)))),
    );
    add(
        "Interactive rebase children of selected commit…",
        none(),
        Cmd::OnCommit(Dialog::interactive_rebase),
    );
    add(
        "Cherry-pick selected commit(s)…",
        none(),
        Cmd::OnCommits(|_, ids| Dialog::cherry_pick(ids.into_iter().rev().collect())),
    );
    add(
        "Revert selected commit(s)…",
        none(),
        Cmd::OnCommits(|_, ids| Dialog::revert(ids)),
    );
    add(
        "Reset current branch to selected commit…",
        none(),
        Cmd::OnCommit(|_, id| Dialog::reset(id)),
    );
    add(
        "Create patch from selected commit(s)…",
        none(),
        Cmd::OnCommits(|_, ids| Dialog::create_patch(ids.into_iter().rev().collect())),
    );
    add(
        "Archive selected commit…",
        none(),
        Cmd::OnCommit(|_, id| Dialog::archive(id)),
    );
    add(
        "Add/Edit note of selected commit…",
        none(),
        Cmd::OnCommit(|_, id| Dialog::edit_note(id)),
    );
    add(
        "Remove note of selected commit…",
        none(),
        Cmd::OnCommit(|_, id| Dialog::remove_note(id)),
    );
    add(
        "Blame selected file at selected commit",
        none(),
        Cmd::BlameSelected,
    );
    add("Refresh", k(Act::Refresh), Cmd::Refresh);
    add("Fetch…", k(Act::Fetch), Cmd::Dialog(Dialog::fetch));
    add("Pull…", k(Act::Pull), Cmd::Dialog(Dialog::pull));
    add("Push…", k(Act::Push), Cmd::Dialog(Dialog::push));
    add(
        "New branch…",
        k(Act::NewBranch),
        Cmd::Dialog(|s| Dialog::branch(s, None)),
    );
    add("Merge…", none(), Cmd::Dialog(|s| Dialog::merge(s, None)));
    add("Rebase…", none(), Cmd::Dialog(|s| Dialog::rebase(s, None)));
    add(
        "Stash changes…",
        k(Act::Stash),
        Cmd::Dialog(|_| Dialog::stash()),
    );
    add(
        "Discard all changes…",
        none(),
        Cmd::Dialog(|_| Dialog::discard_all()),
    );
    add("New tag…", none(), Cmd::Dialog(|_| Dialog::tag(None)));
    add("Add remote…", none(), Cmd::Dialog(|_| Dialog::add_remote()));
    add(
        "Apply patch…",
        none(),
        Cmd::Dialog(|_| Dialog::apply_patch()),
    );
    add("Git console…", none(), Cmd::Dialog(|_| Dialog::console()));
    add(
        "Edit .gitignore…",
        none(),
        Cmd::Dialog(|_| Dialog::edit_gitignore()),
    );
    add(
        "Edit repository config…",
        none(),
        Cmd::Dialog(|_| Dialog::edit_repo_config()),
    );
    add(
        "Edit global git config…",
        none(),
        Cmd::Dialog(|_| Dialog::edit_global_config()),
    );
    add("Bisect…", none(), Cmd::Dialog(Dialog::bisect));
    add("Git Flow…", none(), Cmd::Dialog(Dialog::git_flow));
    add("Git LFS…", none(), Cmd::Dialog(Dialog::lfs));
    add(
        "Sparse checkout…",
        none(),
        Cmd::Dialog(Dialog::sparse_checkout),
    );
    add(
        "Clean working copy…",
        none(),
        Cmd::Dialog(|_| Dialog::clean()),
    );
    add(
        "Optimize repository…",
        none(),
        Cmd::Dialog(Dialog::optimize),
    );
    add(
        "Authentication…",
        none(),
        Cmd::Dialog(Dialog::authentication),
    );
    add(
        "Add submodule…",
        none(),
        Cmd::Dialog(|_| Dialog::submodule_add()),
    );
    add("Update all submodules", none(), Cmd::SubmoduleUpdate(None));
    add("Add worktree…", none(), Cmd::AddWorktree);
    add("Open terminal", none(), Cmd::Terminal);
    add("Show in Explorer", none(), Cmd::Explorer);
    for b in &snap.refs.branches {
        if !b.is_head {
            add(
                &format!("Checkout {}", b.name),
                none(),
                Cmd::Checkout(b.name.clone()),
            );
        }
    }
    for s in snap.refs.stashes.iter().take(100) {
        let name = format!("stash@{{{}}}: {}", s.index, s.message);
        for (verb, c) in [
            ("Apply", StashCmd::Apply),
            ("Pop", StashCmd::Pop),
            ("Drop", StashCmd::Drop),
            ("Branch from", StashCmd::Branch),
        ] {
            add(
                &format!("{verb} stash {name}…"),
                none(),
                Cmd::Stash(c, s.index),
            );
        }
    }
    for (path, initialized) in crate::views::sidebar::submodules(tab) {
        add(
            &format!("Update submodule {path}"),
            none(),
            Cmd::SubmoduleUpdate(Some(path.clone())),
        );
        if initialized {
            add(
                &format!("Open submodule {path}"),
                none(),
                Cmd::OpenRepo(tab.path.join(&path)),
            );
        }
    }
    for w in crate::views::sidebar::worktrees(tab) {
        let at = format!("{} ({})", w.label, w.path.display());
        if w.openable {
            add(
                &format!("Open worktree {at}"),
                none(),
                Cmd::OpenRepo(w.path.clone()),
            );
        }
        if w.removable {
            add(
                &format!("Remove worktree {at}…"),
                none(),
                Cmd::RemoveWorktree(w.path),
            );
        }
    }
    // Conflicted files open in the merge editor; changed tracked files can be blamed.
    let mut blames = 0;
    for e in &snap.status.unstaged {
        let path = e.path.to_string();
        match e.kind {
            gk_status::Kind::Conflict(_) => add(
                &format!("Resolve in merge editor: {path}"),
                none(),
                Cmd::Merge(path),
            ),
            gk_status::Kind::Untracked
            | gk_status::Kind::Added
            | gk_status::Kind::Deleted
            | gk_status::Kind::IntentToAdd => {}
            _ if blames < 500 => {
                blames += 1;
                add(&format!("Blame {path}"), none(), Cmd::Blame(path));
            }
            _ => {}
        }
    }
    v
}

/// A branch tip merges / rebases by name (nicer merge message).
pub(crate) fn commit_rev(s: &Snapshot, id: &str) -> String {
    gk_core::ObjectId::from_hex(id.as_bytes())
        .map(|oid| crate::views::history::checkout_rev(s, &oid))
        .unwrap_or_else(|_| id.to_string())
}

/// How well a label matches: exact > prefix > word start > anywhere, then
/// nucleo's score, then shorter labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rank {
    tier: u8,
    score: u32,
    shorter: std::cmp::Reverse<usize>,
}

/// nucleo-matcher with the palette's tie-breaking rules.
pub struct Ranker {
    pattern: Option<Pattern>,
    lower: String,
    matcher: Matcher,
    buf: Vec<char>,
}

impl Ranker {
    pub fn new(query: &str) -> Ranker {
        let q = query.trim();
        Ranker {
            pattern: (!q.is_empty())
                .then(|| Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart)),
            lower: q.to_lowercase(),
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// `None` when `label` does not match.
    pub fn rank(&mut self, label: &str) -> Option<Rank> {
        let shorter = std::cmp::Reverse(label.chars().count());
        let Some(p) = &self.pattern else {
            return Some(Rank {
                tier: 0,
                score: 0,
                shorter,
            });
        };
        let score = p.score(Utf32Str::new(label, &mut self.buf), &mut self.matcher)?;
        let l = label.to_lowercase();
        let q = self.lower.as_str();
        let word_start = l.match_indices(q).any(|(i, _)| {
            l[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
        });
        let tier = if l == q || l.trim_end_matches('…') == q {
            3
        } else if l.starts_with(q) {
            2
        } else if word_start {
            1
        } else {
            0
        };
        Some(Rank {
            tier,
            score,
            shorter,
        })
    }

    /// Matched character positions of `label`, sorted.
    pub fn indices(&mut self, label: &str) -> Vec<u32> {
        let mut idx = Vec::new();
        if let Some(p) = &self.pattern {
            p.indices(
                Utf32Str::new(label, &mut self.buf),
                &mut self.matcher,
                &mut idx,
            );
        }
        idx.sort_unstable();
        idx.dedup();
        idx
    }
}

/// `label` with the characters at `matched` (char positions) in `hl`.
fn highlighted(
    label: &str,
    matched: &[u32],
    font: egui::FontId,
    text: egui::Color32,
    hl: egui::Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut run = String::new();
    let mut run_hl = false;
    let mut m = matched.iter().peekable();
    let flush = |job: &mut LayoutJob, run: &mut String, on: bool| {
        if !run.is_empty() {
            let mut f = TextFormat::simple(font.clone(), if on { hl } else { text });
            if on {
                f.underline = egui::Stroke::new(1.0, hl);
            }
            job.append(run, 0.0, f);
            run.clear();
        }
    };
    for (i, c) in label.chars().enumerate() {
        let on = m.next_if(|&&j| j as usize == i).is_some();
        if on != run_hl {
            flush(&mut job, &mut run, run_hl);
            run_hl = on;
        }
        run.push(c);
    }
    flush(&mut job, &mut run, run_hl);
    job
}

pub fn show(
    ctx: &egui::Context,
    st: &mut PaletteState,
    tab: Option<&mut RepoTab>,
    shared: &mut Shared,
) {
    if !st.open {
        return;
    }
    let snap = tab.as_ref().map(|t| t.session.snapshot());
    // Scripted runs wait until the repository is loaded, like a user would.
    if st.auto_run
        && snap
            .as_ref()
            .is_some_and(|s| s.refs.head == gk_core::Head::Unknown || s.history_loading)
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return;
    }
    let km = keymap::current(ctx);
    let all = items(tab.as_deref(), snap.as_ref(), &km);
    let mut ranker = Ranker::new(&st.query);
    let mut ranked: Vec<(Rank, &Item)> = all
        .iter()
        .filter_map(|it| ranker.rank(&it.label).map(|r| (r, it)))
        .collect();
    // Stable: equal ranks keep the list order.
    ranked.sort_by_key(|r| std::cmp::Reverse(r.0));
    ranked.truncate(SHOWN);
    st.selected = st.selected.min(ranked.len().saturating_sub(1));

    let mut run: Option<Cmd> = None;
    let mut close = false;
    egui::Modal::new(egui::Id::new("palette")).show(ctx, |ui| {
        ui.set_width(520.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut st.query)
                .hint_text("Type a command…")
                .desired_width(f32::INFINITY),
        );
        if st.focus {
            r.request_focus();
            st.focus = false;
        }
        if r.changed() {
            st.selected = 0;
        }
        let (up, down, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if up {
            st.selected = st.selected.saturating_sub(1);
        }
        if down && st.selected + 1 < ranked.len() {
            st.selected += 1;
        }
        if esc {
            close = true;
        }
        if enter || std::mem::take(&mut st.auto_run) {
            run = ranked.get(st.selected).map(|(_, it)| it.cmd.clone());
        }
        let font = egui::TextStyle::Button.resolve(ui.style());
        let text = ui.visuals().text_color();
        let hl = shared.palette.tag;
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for (i, (_, it)) in ranked.iter().enumerate() {
                    let job = highlighted(
                        &it.label,
                        &ranker.indices(&it.label),
                        font.clone(),
                        text,
                        hl,
                    );
                    let mut b = egui::Button::selectable(i == st.selected, job)
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if !it.hint.is_empty() {
                        b = b.shortcut_text(egui::RichText::new(&it.hint).weak());
                    }
                    let resp = ui.add(b);
                    if i == st.selected && (up || down) {
                        resp.scroll_to_me(None);
                    }
                    if resp.clicked() {
                        run = Some(it.cmd.clone());
                    }
                }
            });
    });
    if run.is_some() || close {
        st.open = false;
    }
    let Some(cmd) = run else { return };
    execute(ctx, cmd, tab, snap, shared);
}

/// Runs `cmd` on the active tab (`None` without a repository).
pub(crate) fn execute(
    ctx: &egui::Context,
    cmd: Cmd,
    tab: Option<&mut RepoTab>,
    snap: Option<Snapshot>,
    shared: &mut Shared,
) {
    match (cmd, tab, snap) {
        (Cmd::App(f), _, _) => shared.actions.push(f()),
        (Cmd::Global(f), _, _) => shared.actions.push(Action::Global(f())),
        (Cmd::ToggleOutput, _, _) => *shared.show_log = !*shared.show_log,
        (Cmd::OpenRepo(p), _, _) => shared.actions.push(Action::OpenRepo(p)),
        (Cmd::View(v), Some(t), _) => t.view = v,
        (Cmd::Dialog(f), Some(t), Some(s)) => t.dialog = Some(f(&s)),
        (Cmd::Refresh, Some(t), _) => t.session.refresh_all(),
        (Cmd::Terminal, Some(t), _) => crate::views::toolbar::open_terminal(&t.path, shared),
        (Cmd::Explorer, Some(t), _) => {
            let _ = open::that(&t.path);
        }
        (Cmd::Checkout(b), Some(t), _) => t.dialog = Some(Dialog::checkout(b)),
        (Cmd::Merge(p), Some(t), _) => crate::views::merge::open_merge(t, p),
        (Cmd::Blame(p), Some(t), _) => crate::views::blame::open_blame(t, p, None),
        (Cmd::Stash(c, i), Some(t), _) => {
            t.dialog = Some(match c {
                StashCmd::Apply => Dialog::stash_apply(i),
                StashCmd::Pop => Dialog::stash_pop(i),
                StashCmd::Drop => Dialog::stash_drop(i),
                StashCmd::Branch => Dialog::stash_branch(i),
            })
        }
        (Cmd::SubmoduleUpdate(p), Some(t), _) => crate::views::sidebar::update_submodule(t, p),
        (Cmd::AddWorktree, Some(t), _) => crate::views::sidebar::add_worktree(t),
        (Cmd::RemoveWorktree(p), Some(t), _) => crate::views::sidebar::remove_worktree(t, p),
        (Cmd::OnCommit(f), Some(t), Some(s)) => match selected_commits(t, &s).as_slice() {
            [id] => t.dialog = Some(f(&s, id.clone())),
            _ => t.session.push_message(
                gk_core::MessageKind::Info,
                "Select a single commit in History, Search or Reflog first.",
            ),
        },
        (Cmd::OnCommits(f), Some(t), Some(s)) => {
            let ids = selected_commits(t, &s);
            if ids.is_empty() {
                t.session.push_message(
                    gk_core::MessageKind::Info,
                    "Select commits in History, Search or Reflog first.",
                );
            } else {
                t.dialog = Some(f(&s, ids));
            }
        }
        (Cmd::Commit, Some(t), _) => {
            t.view = View::FileStatus;
            t.status.commit.request_focus();
        }
        (Cmd::StageAll(stage), Some(t), _) => {
            t.view = View::FileStatus;
            t.status.request_all(stage);
        }
        (Cmd::CopyShas(short), Some(t), Some(s)) => {
            let ids = selected_commits(t, &s);
            if ids.is_empty() {
                t.session.push_message(
                    gk_core::MessageKind::Info,
                    "Select commits in History, Search or Reflog first.",
                );
            } else {
                let ids: Vec<String> = if short {
                    ids.iter()
                        .map(|i| crate::util::short_id(i).to_string())
                        .collect()
                } else {
                    ids
                };
                ctx.copy_text(ids.join("\n"));
            }
        }
        (Cmd::BlameSelected, Some(t), Some(s)) => {
            let ids = selected_commits(t, &s);
            match (ids.as_slice(), t.details.selected_file()) {
                ([id], Some(path)) => {
                    let (path, rev) = (path.to_string(), Some(id.clone()));
                    crate::views::blame::open_blame(t, path, rev)
                }
                _ => t.session.push_message(
                    gk_core::MessageKind::Info,
                    "Select a commit and one of its files in History first.",
                ),
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(labels: &[&str], query: &str) -> Vec<String> {
        let mut r = Ranker::new(query);
        let mut v: Vec<(Rank, &str)> = labels
            .iter()
            .filter_map(|l| r.rank(l).map(|k| (k, *l)))
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.0));
        v.into_iter().map(|(_, l)| l.to_string()).collect()
    }

    #[test]
    fn fuzzy_scoring() {
        let mut r = Ranker::new("nb");
        assert!(r.rank("New branch…").is_some());
        assert!(Ranker::new("xyz").rank("New branch…").is_none());
        // Word starts beat scattered matches.
        let mut r = Ranker::new("pu");
        assert!(r.rank("Push…").unwrap() > r.rank("Open repo, update").unwrap());
        // Empty query: everything, in list order (equal ranks).
        assert!(Ranker::new("  ").rank("anything").is_some());
    }

    #[test]
    fn exact_and_prefix_first() {
        let labels = [
            "Checkout selected commit…",
            "Rebase current branch onto selected commit…",
            "Fetch…",
            "Refresh",
            "View: History",
            "Revert selected commit(s)…",
            "View: Reflog",
        ];
        assert_eq!(
            order(&labels, "fetch")[0],
            "Fetch…",
            "exact (ellipsis ignored)"
        );
        let re = order(&labels, "re");
        let pos = |l: &str| re.iter().position(|x| x == l).unwrap();
        assert_eq!(re[0], "Refresh", "shortest prefix match");
        assert!(pos("View: Reflog") > pos("Revert selected commit(s)…"));
        assert!(
            pos("View: Reflog") > pos("Rebase current branch onto selected commit…"),
            "prefix before word start"
        );
        assert_eq!(order(&labels, "View: History")[0], "View: History");
        assert_eq!(order(&labels, "history")[0], "View: History", "word start");
        let revert = order(&labels, "revert");
        assert_eq!(revert[0], "Revert selected commit(s)…");
        assert_eq!(revert.len(), 1);
    }

    #[test]
    fn highlight_indices_follow_chars() {
        let mut r = Ranker::new("vh");
        let idx = r.indices("View: History");
        assert_eq!(idx, vec![0, 6]);
        // Non-ASCII labels index chars, not bytes.
        let mut r = Ranker::new("pull");
        assert_eq!(r.indices("Pull…"), vec![0, 1, 2, 3]);
        let job = highlighted(
            "Pull…",
            &[0, 1, 2, 3],
            egui::FontId::default(),
            egui::Color32::WHITE,
            egui::Color32::RED,
        );
        assert_eq!(job.text, "Pull…");
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.color, egui::Color32::RED);
    }
}
