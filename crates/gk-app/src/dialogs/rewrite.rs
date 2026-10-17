//! History rewriting: merge, rebase, interactive rebase, cherry-pick/revert, reset.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use gk_core::ops::{FastForward, MergeOptions, ResetMode, TodoAction, TodoItem};

fn current_label(snap: &Snapshot) -> String {
    match &snap.refs.head {
        gk_core::Head::Branch(b) | gk_core::Head::Unborn(b) => b.clone(),
        gk_core::Head::Detached(id) => format!("detached HEAD at {}", crate::util::short_id(id)),
        gk_core::Head::Unknown => "HEAD".into(),
    }
}

fn state_warning(ui: &mut egui::Ui, ctl: &mut Ctl, snap: &Snapshot) {
    if snap.refs.state != gk_core::RepoState::Clean {
        error(
            ui,
            format!(
                "Another operation is in progress ({}). Finish or abort it first.",
                snap.refs.state.label()
            ),
        );
        ctl.invalid();
    }
}

/// Default rev for merge/rebase: the current branch's upstream, else the first other branch.
fn default_rev(s: &Snapshot) -> String {
    if let Some(u) = s.refs.current_branch().and_then(|b| b.upstream.clone()) {
        return u;
    }
    s.refs
        .branches
        .iter()
        .find(|b| !b.is_head)
        .map(|b| b.name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- merge

pub struct Merge {
    rev: String,
    given: Option<String>,
    ff: FastForward,
    squash: bool,
    commit: bool,
    custom_message: bool,
    message: String,
    label: RevLabel,
}

impl Merge {
    pub fn new(s: &Snapshot, rev: Option<String>) -> Self {
        Self {
            rev: rev.clone().unwrap_or_else(|| default_rev(s)),
            given: rev,
            ff: FastForward::Allow,
            squash: false,
            commit: true,
            custom_message: false,
            message: String::new(),
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for Merge {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.rev.is_empty() && refs_loaded(snap) {
            // Opened before the refs were loaded.
            self.rev = default_rev(snap);
        }
        let into = current_label(snap);
        let (out, ()) = modal(ctx, "merge", "Merge", 560.0, "Merge", |ui, ctl| {
            egui::Grid::new("merge-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Merge from:");
                    rev_combo(
                        ui,
                        "merge-rev",
                        &mut self.rev,
                        snap,
                        self.given.as_deref(),
                        360.0,
                    );
                    ui.end_row();
                    ui.label("Into:");
                    ui.label(RichText::new(&into).strong());
                    ui.end_row();
                });
            let label = self.label.get(tab, snap, &self.rev).to_string();
            if !label.is_empty() {
                hint(ui, label);
            }
            ui.add_space(4.0);
            ui.label("Fast-forward:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.ff, FastForward::Allow, "When possible");
                ui.radio_value(
                    &mut self.ff,
                    FastForward::Never,
                    "Always create a merge commit",
                );
                ui.radio_value(&mut self.ff, FastForward::Only, "Only fast-forward");
            });
            ui.checkbox(
                &mut self.squash,
                "Squash commits (stage the combined changes, no merge commit)",
            );
            ui.add_enabled_ui(!self.squash, |ui| {
                ui.checkbox(&mut self.commit, "Commit merged changes immediately");
                ui.add_enabled_ui(self.commit, |ui| {
                    ui.checkbox(&mut self.custom_message, "Custom commit message");
                    if self.custom_message {
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut self.message)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        );
                        ctl.multiline(&r);
                    }
                });
            });
            if self.squash && self.ff == FastForward::Never {
                error(
                    ui,
                    "Squash cannot be combined with 'Always create a merge commit'.",
                );
                ctl.invalid();
            }
            if self.rev.trim().is_empty() {
                ctl.invalid();
            }
            if matches!(snap.refs.head, gk_core::Head::Detached(_)) {
                warning(
                    ui,
                    "HEAD is detached; the merge result will not be on a branch.",
                );
            }
            if worktree_dirty(snap) {
                warning(
                    ui,
                    "You have uncommitted changes. Git refuses to merge if they touch the same files.",
                );
            }
            state_warning(ui, ctl, snap);
        });
        finish(out, || {
            let rev = self.rev.trim().to_string();
            let opts = MergeOptions {
                ff: self.ff,
                squash: self.squash,
                commit: self.commit && !self.squash,
                message: (self.commit
                    && !self.squash
                    && self.custom_message
                    && !self.message.trim().is_empty())
                .then(|| self.message.trim().to_string()),
            };
            tab.session
                .run_op(format!("Merge {rev}"), move |repo, _, _| {
                    repo.merge(&rev, &opts)
                });
        })
    }
}

// ---------------------------------------------------------------- rebase

pub struct Rebase {
    onto: String,
    given: Option<String>,
    label: RevLabel,
}

impl Rebase {
    pub fn new(s: &Snapshot, onto: Option<String>) -> Self {
        Self {
            onto: onto.clone().unwrap_or_else(|| default_rev(s)),
            given: onto,
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for Rebase {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.onto.is_empty() && refs_loaded(snap) {
            // Opened before the refs were loaded.
            self.onto = default_rev(snap);
        }
        let cur = current_label(snap);
        let (out, ()) = modal(ctx, "rebase", "Rebase", 560.0, "Rebase", |ui, ctl| {
            egui::Grid::new("rebase-grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Rebase:");
                    ui.label(RichText::new(&cur).strong());
                    ui.end_row();
                    ui.label("Onto:");
                    rev_combo(
                        ui,
                        "rebase-onto",
                        &mut self.onto,
                        snap,
                        self.given.as_deref(),
                        360.0,
                    );
                    ui.end_row();
                });
            let label = self.label.get(tab, snap, &self.onto).to_string();
            if !label.is_empty() {
                hint(ui, label);
            }
            ui.add_space(4.0);
            warning(
                ui,
                "Rebasing rewrites the commits of the current branch. Do not rebase commits that others \
                 have already pulled; pushing afterwards requires a force push.",
            );
            if worktree_dirty(snap) {
                error(
                    ui,
                    "You have uncommitted changes. Commit or stash them before rebasing.",
                );
                ctl.invalid();
            }
            if self.onto.trim().is_empty() {
                ctl.invalid();
            }
            state_warning(ui, ctl, snap);
        });
        finish(out, || {
            let onto = self.onto.trim().to_string();
            tab.session
                .run_op(format!("Rebase onto {onto}"), move |repo, _, _| {
                    repo.rebase(&onto)
                });
        })
    }
}

// ---------------------------------------------------------------- interactive rebase

struct Row {
    action: TodoAction,
    id: String,
    summary: String,
    message: String,
    new_message: String,
    editing: bool,
}

#[derive(Clone, Copy)]
struct RowDrag(usize);

const ACTIONS: [TodoAction; 6] = [
    TodoAction::Pick,
    TodoAction::Reword,
    TodoAction::Edit,
    TodoAction::Squash,
    TodoAction::Fixup,
    TodoAction::Drop,
];

/// (commit id, full message), oldest first.
type Commits = Result<Vec<(String, String)>, String>;

/// Splits "parents\0message" records (`--format=%H%x00%P%x00%B`), refusing
/// ranges that contain merge commits.
pub fn split_parents(records: Vec<(String, String)>) -> Commits {
    records
        .into_iter()
        .map(|(id, rest)| {
            let (parents, msg) = rest.split_once('\0').unwrap_or(("", rest.as_str()));
            if parents.split_whitespace().count() > 1 {
                return Err(format!(
                    "The range contains merge commits ({}); interactive rebase of merges is not supported.",
                    crate::util::short_id(&id)
                ));
            }
            Ok((id, msg.trim_start_matches(['\n', '\r']).to_string()))
        })
        .collect()
}

pub struct InteractiveRebase {
    base: String,
    task: Option<Task<Commits>>,
    rows: Option<Vec<Row>>,
    error: Option<String>,
    autosquash: bool,
}

impl InteractiveRebase {
    pub fn new(base: String) -> Self {
        Self {
            base,
            task: None,
            rows: None,
            error: None,
            autosquash: false,
        }
    }

    fn apply_autosquash(rows: &mut Vec<Row>) {
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let sums: Vec<String> = rows.iter().map(|r| r.summary.clone()).collect();
        let plan = autosquash_plan(&ids, &sums);
        let mut old: Vec<Option<Row>> = std::mem::take(rows).into_iter().map(Some).collect();
        for (i, action) in plan {
            if let Some(mut r) = old[i].take() {
                if let Some(a) = action {
                    r.action = a;
                }
                rows.push(r);
            }
        }
    }
}

fn action_color(ui: &egui::Ui, a: TodoAction) -> egui::Color32 {
    let dark = ui.visuals().dark_mode;
    match a {
        TodoAction::Pick => ui.visuals().text_color(),
        TodoAction::Reword if dark => egui::Color32::from_rgb(0x4F, 0x9D, 0xF7),
        TodoAction::Reword => egui::Color32::from_rgb(0x1F, 0x6F, 0xD0),
        TodoAction::Edit if dark => egui::Color32::from_rgb(0xC3, 0x7B, 0xE8),
        TodoAction::Edit => egui::Color32::from_rgb(0x8E, 0x3F, 0xB8),
        TodoAction::Squash | TodoAction::Fixup => warn_color(ui),
        TodoAction::Drop => DANGER,
    }
}

impl TabDialog for InteractiveRebase {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.rows.is_none() && self.error.is_none() {
            let task = self.task.get_or_insert_with(|| {
                let repo = tab.session.repo().clone();
                let range = format!("{}..HEAD", self.base);
                Task::spawn(ctx, move || {
                    // Our todo replaces git's, so commits missing from it would
                    // vanish: merges are refused rather than skipped.
                    let out = repo
                        .git()
                        .run(&[
                            "log",
                            "--reverse",
                            "--topo-order",
                            "--format=%H%x00%P%x00%B%x1e",
                            &range,
                        ])
                        .map_err(|e| format!("{e:#}"))?;
                    split_parents(parse_log_records(&out.stdout_str()))
                })
            });
            match task.take() {
                Some(Ok(list)) => {
                    self.rows = Some(
                        list.into_iter()
                            .map(|(id, message)| Row {
                                action: TodoAction::Pick,
                                summary: message.lines().next().unwrap_or_default().to_string(),
                                new_message: message.clone(),
                                message,
                                id,
                                editing: false,
                            })
                            .collect(),
                    );
                }
                Some(Err(e)) => self.error = Some(e),
                None => {}
            }
        }
        let base_label = describe_rev(tab, snap, &self.base);
        let (out, ()) = modal(
            ctx,
            "interactive-rebase",
            "Interactive rebase",
            760.0,
            "Start rebase",
            |ui, ctl| {
                ui.horizontal(|ui| {
                    ui.label("Rewrite the commits of");
                    ui.label(RichText::new(current_label(snap)).strong());
                    ui.label("after");
                    ui.label(
                        RichText::new(if base_label.is_empty() {
                            self.base.clone()
                        } else {
                            base_label.clone()
                        })
                        .monospace(),
                    );
                });
                hint(
                    ui,
                    "Oldest commit first. Drag ☰ or use ↑/↓ to reorder. Squash/fixup meld a commit into the one above it.",
                );
                if let Some(e) = &self.error {
                    error(ui, e.clone());
                    ctl.invalid();
                    return;
                }
                let Some(rows) = &mut self.rows else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading commits…");
                    });
                    ctl.invalid();
                    return;
                };
                if ui
                    .checkbox(
                        &mut self.autosquash,
                        "Autosquash (move fixup!/squash! commits under their targets)",
                    )
                    .changed()
                    && self.autosquash
                {
                    Self::apply_autosquash(rows);
                }
                let mut move_op: Option<(usize, usize)> = None;
                let n = rows.len();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(380.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, r) in rows.iter_mut().enumerate() {
                                let row = ui.horizontal(|ui| {
                                    let h = ui.add(
                                        egui::Label::new(RichText::new("☰").weak())
                                            .sense(egui::Sense::drag()),
                                    );
                                    h.dnd_set_drag_payload(RowDrag(i));
                                    egui::ComboBox::from_id_salt(("rebase-action", i))
                                        .width(80.0)
                                        .selected_text(
                                            RichText::new(r.action.word())
                                                .color(action_color(ui, r.action)),
                                        )
                                        .show_ui(ui, |ui| {
                                            for a in ACTIONS {
                                                ui.selectable_value(
                                                    &mut r.action,
                                                    a,
                                                    RichText::new(a.word())
                                                        .color(action_color(ui, a)),
                                                );
                                            }
                                        });
                                    if ui
                                        .add_enabled(i > 0, egui::Button::new("↑").small())
                                        .clicked()
                                    {
                                        move_op = Some((i, i - 1));
                                    }
                                    if ui
                                        .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                                        .clicked()
                                    {
                                        move_op = Some((i, i + 1));
                                    }
                                    ui.label(
                                        RichText::new(crate::util::short_id(&r.id)).monospace(),
                                    );
                                    let mut text = RichText::new(&r.summary);
                                    if r.action == TodoAction::Drop {
                                        text = text.strikethrough().weak();
                                    }
                                    ui.label(text);
                                    if r.action == TodoAction::Reword {
                                        let changed = r.new_message.trim() != r.message.trim();
                                        let label = if r.editing {
                                            "Done"
                                        } else if changed {
                                            "✎ Edited"
                                        } else {
                                            "✎ Message"
                                        };
                                        if ui.small_button(label).clicked() {
                                            r.editing = !r.editing;
                                        }
                                    }
                                });
                                if let Some(from) = row.response.dnd_release_payload::<RowDrag>() {
                                    move_op = Some((from.0, i));
                                }
                                if row.response.dnd_hover_payload::<RowDrag>().is_some() {
                                    let rect = row.response.rect;
                                    ui.painter().hline(
                                        rect.x_range(),
                                        rect.top(),
                                        ui.visuals().selection.stroke,
                                    );
                                }
                                if r.action == TodoAction::Reword && r.editing {
                                    ui.indent(("reword", i), |ui| {
                                        let resp = ui.add(
                                            egui::TextEdit::multiline(&mut r.new_message)
                                                .desired_rows(4)
                                                .desired_width(f32::INFINITY)
                                                .font(egui::TextStyle::Monospace),
                                        );
                                        ctl.multiline(&resp);
                                    });
                                }
                            }
                        });
                });
                if let Some((from, to)) = move_op {
                    move_item(rows, from, to);
                }
                if rows.is_empty() {
                    error(ui, "There are no commits to rebase.");
                    ctl.invalid();
                }
                if let Some(first) = rows.iter().find(|r| r.action != TodoAction::Drop) {
                    if matches!(first.action, TodoAction::Squash | TodoAction::Fixup) {
                        error(
                            ui,
                            "The first commit cannot be squashed or fixed up: there is no previous commit.",
                        );
                        ctl.invalid();
                    }
                } else if !rows.is_empty() {
                    warning(ui, "All commits will be dropped.");
                    ctl.danger = true;
                }
                if rows
                    .iter()
                    .any(|r| r.action == TodoAction::Reword && r.new_message.trim().is_empty())
                {
                    error(ui, "A reworded commit has an empty message.");
                    ctl.invalid();
                }
                warning(
                    ui,
                    "Rewrites history: do not rebase commits that others have already pulled.",
                );
                if worktree_dirty(snap) {
                    error(
                        ui,
                        "You have uncommitted changes. Commit or stash them before rebasing.",
                    );
                    ctl.invalid();
                }
                state_warning(ui, ctl, snap);
            },
        );
        finish(out, || {
            let Some(rows) = &self.rows else { return };
            let todo: Vec<TodoItem> = rows
                .iter()
                .map(|r| TodoItem {
                    action: r.action,
                    id: r.id.clone(),
                    summary: r.summary.clone(),
                    new_message: (r.action == TodoAction::Reword
                        && r.new_message.trim() != r.message.trim())
                    .then(|| r.new_message.clone()),
                })
                .collect();
            let base = self.base.clone();
            let autosquash = self.autosquash;
            tab.session.run_op("Interactive rebase", move |repo, _, _| {
                repo.rebase_interactive(&base, &todo, autosquash)
            });
        })
    }
}

// ---------------------------------------------------------------- cherry-pick / revert

pub struct Picks {
    ids: Vec<String>,
    revert: bool,
    no_commit: bool,
    sorted: bool,
    labels: RevLabels,
}

impl Picks {
    pub fn new(ids: Vec<String>, revert: bool) -> Self {
        Self {
            ids,
            revert,
            no_commit: false,
            sorted: false,
            labels: RevLabels::default(),
        }
    }
}

impl TabDialog for Picks {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if !self.sorted {
            // Cherry-pick oldest first; revert newest first.
            let row = |id: &String| {
                gk_core::ObjectId::from_hex(id.as_bytes())
                    .ok()
                    .and_then(|o| snap.history.row(&o))
                    .unwrap_or(usize::MAX)
            };
            self.ids.sort_by_key(|id| std::cmp::Reverse(row(id)));
            if self.revert {
                self.ids.reverse();
            }
            self.sorted = true;
        }
        let labels = self.labels.get(tab, snap, &self.ids).to_vec();
        let (title, verb) = if self.revert {
            ("Revert", "Revert")
        } else {
            ("Cherry-pick", "Cherry-pick")
        };
        let into = current_label(snap);
        let (out, ()) = modal(ctx, "picks", title, 560.0, verb, |ui, ctl| {
            let n = self.ids.len();
            ui.label(if self.revert {
                format!("Create commits that undo {n} commit(s) on {into}:")
            } else {
                format!("Apply {n} commit(s) onto {into}:")
            });
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (id, l) in self.ids.iter().zip(&labels) {
                            ui.label(
                                RichText::new(if l.is_empty() {
                                    id.as_str()
                                } else {
                                    l.as_str()
                                })
                                .monospace(),
                            );
                        }
                    });
            });
            ui.checkbox(
                &mut self.no_commit,
                "Do not commit (only apply the changes to the index and working copy)",
            );
            if n == 0 {
                ctl.invalid();
            }
            state_warning(ui, ctl, snap);
        });
        finish(out, || {
            let ids = self.ids.clone();
            let no_commit = self.no_commit;
            if self.revert {
                tab.session
                    .run_op("Revert", move |repo, _, _| repo.revert(&ids, no_commit));
            } else {
                tab.session.run_op("Cherry-pick", move |repo, _, _| {
                    repo.cherry_pick(&ids, no_commit)
                });
            }
        })
    }
}

// ---------------------------------------------------------------- reset

pub struct Reset {
    rev: String,
    mode: ResetMode,
    confirmed: bool,
    label: RevLabel,
}

impl Reset {
    pub fn new(rev: String) -> Self {
        Self {
            rev,
            mode: ResetMode::Mixed,
            confirmed: false,
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for Reset {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        let dirty = worktree_dirty(snap);
        let confirm = shared.settings.confirmations.hard_reset;
        let cur = current_label(snap);
        let (out, ()) = modal(ctx, "reset", "Reset", 540.0, "Reset", |ui, ctl| {
            ui.horizontal(|ui| {
                ui.label("Reset");
                ui.label(RichText::new(&cur).strong());
                ui.label("to");
            });
            let l = self.label.get(tab, snap, &self.rev).to_string();
            ui.label(RichText::new(if l.is_empty() { self.rev.clone() } else { l }).monospace());
            ui.add_space(4.0);
            ui.radio_value(&mut self.mode, ResetMode::Soft, "Soft");
            hint(
                ui,
                "    Keep all changes; the undone commits' changes become staged.",
            );
            ui.radio_value(&mut self.mode, ResetMode::Mixed, "Mixed (default)");
            hint(
                ui,
                "    Keep the working copy; the undone commits' changes become unstaged.",
            );
            ui.radio_value(
                &mut self.mode,
                ResetMode::Hard,
                RichText::new("Hard").color(DANGER),
            );
            hint(
                ui,
                "    Discard all changes: index and working copy match the target commit.",
            );
            if self.mode == ResetMode::Hard {
                ctl.danger = true;
                if dirty {
                    ui.colored_label(
                        DANGER,
                        "⚠ Your uncommitted changes will be permanently lost.",
                    );
                    if confirm {
                        ui.checkbox(
                            &mut self.confirmed,
                            RichText::new("I understand, discard my changes").color(DANGER),
                        );
                        if !self.confirmed {
                            ctl.invalid();
                        }
                    }
                }
            }
            if snap.refs.state != gk_core::RepoState::Clean {
                warning(
                    ui,
                    format!(
                        "An operation is in progress ({}); resetting may leave it in an odd state.",
                        snap.refs.state.label()
                    ),
                );
            }
        });
        finish(out, || {
            let rev = self.rev.clone();
            let mode = self.mode;
            let label = format!(
                "Reset ({}) to {}",
                format!("{mode:?}").to_lowercase(),
                crate::util::short_id(&rev)
            );
            tab.session.run_op(label, move |repo, _, _| {
                repo.reset(&rev, mode)?;
                Ok(String::new())
            });
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_refused() {
        let out = "aaaa\0p1\0Subject A\n\nBody\n\x1e\nbbbb\0aaaa\0Subject B\n\x1e\n";
        let commits = split_parents(parse_log_records(out)).unwrap();
        assert_eq!(
            commits,
            [
                ("aaaa".to_string(), "Subject A\n\nBody".to_string()),
                ("bbbb".to_string(), "Subject B".to_string())
            ]
        );
        let out = "aaaa\0p1\0A\n\x1e\ncccc\0p1 p2\0Merge\n\x1e\n";
        assert!(
            split_parents(parse_log_records(out))
                .unwrap_err()
                .contains("merge commits")
        );
    }
}
