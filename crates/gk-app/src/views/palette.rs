//! Command palette (Ctrl+Shift+P): fuzzy-searchable list of every action,
//! plus "Checkout <branch>" entries for the open repository.

use crate::app::{Action, Shared};
use crate::dialogs::{Dialog, GlobalDialog};
use crate::tab::{RepoTab, View};
use gk_core::Snapshot;

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

#[derive(Clone)]
enum Cmd {
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
}

struct Item {
    label: String,
    cmd: Cmd,
}

fn items(snap: Option<&Snapshot>) -> Vec<Item> {
    let mut v: Vec<Item> = Vec::new();
    let mut add = |label: &str, cmd: Cmd| {
        v.push(Item {
            label: label.to_string(),
            cmd,
        })
    };
    add("Open repository…", Cmd::App(|| Action::OpenRepoDialog));
    add("Clone repository…", Cmd::Global(GlobalDialog::clone_repo));
    add("Create repository…", Cmd::Global(GlobalDialog::init_repo));
    add("Settings", Cmd::Global(GlobalDialog::settings));
    add("About GitKwaken", Cmd::Global(GlobalDialog::about));
    add("Toggle output log", Cmd::ToggleOutput);
    if let Some(snap) = snap {
        add("View: File status", Cmd::View(View::FileStatus));
        add("View: History", Cmd::View(View::History));
        add("View: Search", Cmd::View(View::Search));
        add("Refresh", Cmd::Refresh);
        add("Fetch…", Cmd::Dialog(Dialog::fetch));
        add("Pull…", Cmd::Dialog(Dialog::pull));
        add("Push…", Cmd::Dialog(Dialog::push));
        add("New branch…", Cmd::Dialog(|s| Dialog::branch(s, None)));
        add("Merge…", Cmd::Dialog(|s| Dialog::merge(s, None)));
        add("Rebase…", Cmd::Dialog(|s| Dialog::rebase(s, None)));
        add("Stash changes…", Cmd::Dialog(|_| Dialog::stash()));
        add(
            "Discard all changes…",
            Cmd::Dialog(|_| Dialog::discard_all()),
        );
        add("New tag…", Cmd::Dialog(|_| Dialog::tag(None)));
        add("Add remote…", Cmd::Dialog(|_| Dialog::add_remote()));
        add("Apply patch…", Cmd::Dialog(|_| Dialog::apply_patch()));
        add("Git console…", Cmd::Dialog(|_| Dialog::console()));
        add("Bisect…", Cmd::Dialog(Dialog::bisect));
        add("Git Flow…", Cmd::Dialog(Dialog::git_flow));
        add("Git LFS…", Cmd::Dialog(Dialog::lfs));
        add("Sparse checkout…", Cmd::Dialog(Dialog::sparse_checkout));
        add("Clean working copy…", Cmd::Dialog(|_| Dialog::clean()));
        add("Optimize repository…", Cmd::Dialog(Dialog::optimize));
        add("Add submodule…", Cmd::Dialog(|_| Dialog::submodule_add()));
        add("Open terminal", Cmd::Terminal);
        add("Show in Explorer", Cmd::Explorer);
        for b in &snap.refs.branches {
            if !b.is_head {
                v.push(Item {
                    label: format!("Checkout {}", b.name),
                    cmd: Cmd::Checkout(b.name.clone()),
                });
            }
        }
        // Conflicted files open in the merge editor; changed tracked files can be blamed.
        let mut blames = 0;
        for e in &snap.status.unstaged {
            let path = e.path.to_string();
            match e.kind {
                gk_status::Kind::Conflict(_) => v.push(Item {
                    label: format!("Resolve in merge editor: {path}"),
                    cmd: Cmd::Merge(path),
                }),
                gk_status::Kind::Untracked
                | gk_status::Kind::Added
                | gk_status::Kind::Deleted
                | gk_status::Kind::IntentToAdd => {}
                _ if blames < 500 => {
                    blames += 1;
                    v.push(Item {
                        label: format!("Blame {path}"),
                        cmd: Cmd::Blame(path),
                    });
                }
                _ => {}
            }
        }
    }
    v
}

/// Subsequence match score; higher is better, `None` when not matching.
pub fn score(label: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let l: Vec<char> = label.to_lowercase().chars().collect();
    let mut s = 0;
    let mut pos = 0usize;
    let mut last: Option<usize> = None;
    for q in query.to_lowercase().chars() {
        if q == ' ' {
            continue;
        }
        let found = (pos..l.len()).find(|&i| l[i] == q)?;
        s += if last == Some(found.wrapping_sub(1)) {
            6
        } else {
            1
        };
        if found == 0 || !l[found - 1].is_alphanumeric() {
            s += 4;
        }
        last = Some(found);
        pos = found + 1;
    }
    Some(s * 100 - l.len() as i32)
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
    let all = items(snap.as_ref());
    let mut ranked: Vec<(i32, &Item)> = all
        .iter()
        .filter_map(|it| score(&it.label, &st.query).map(|s| (s, it)))
        .collect();
    ranked.sort_by_key(|r| std::cmp::Reverse(r.0));
    ranked.truncate(50);
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
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for (i, (_, it)) in ranked.iter().enumerate() {
                    let resp = ui.selectable_label(i == st.selected, &it.label);
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
    match (cmd, tab, snap) {
        (Cmd::App(f), _, _) => shared.actions.push(f()),
        (Cmd::Global(f), _, _) => shared.actions.push(Action::Global(f())),
        (Cmd::ToggleOutput, _, _) => *shared.show_log = !*shared.show_log,
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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn fuzzy_scoring() {
        assert!(score("New branch…", "nb").is_some());
        assert!(score("New branch…", "xyz").is_none());
        // word starts beat scattered matches
        assert!(
            score("Push…", "pu").unwrap() > score("Open repository…", "pu").unwrap_or(i32::MIN)
        );
    }
}
