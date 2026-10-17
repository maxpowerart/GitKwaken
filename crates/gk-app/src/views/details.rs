//! Commit details: message, metadata, changed files, file diff (agent: history).

use crate::app::Shared;
use crate::tab::{RepoTab, View};
use crate::views::diff::{DiffMode, DiffView};
use crate::views::history::task::{Task, TaskState};
use crate::views::history::{in_rect, split, table};
use egui::{Color32, Id, Rect, RichText, Sense, Ui, pos2, vec2};
use gk_core::content::{ChangeKind, ChangedFile, DiffTarget};
use gk_core::{Label, Load, ObjectId, Session, Snapshot};
use std::sync::Arc;

#[derive(Default)]
pub struct DetailsState {
    pub diff: DiffView,
    selected_file: Option<String>,
    filter: String,
    compare: Task<(ObjectId, ObjectId), Vec<ChangedFile>>,
    /// Note of the shown commit, keyed by (commit, [`NOTES_GEN`]).
    notes: Task<(ObjectId, u64), Option<String>>,
    /// Indices of files matching `filter`, keyed by (files identity, filter).
    filtered: Vec<usize>,
    filtered_key: Option<(usize, usize, String)>,
    /// What was shown last, to apply a preferred file when it changes.
    last_target: Option<(ObjectId, Option<ObjectId>)>,
}

impl DetailsState {
    /// Shows `path`'s diff when the selected commit changes it.
    pub fn preselect(&mut self, path: String) {
        self.selected_file = Some(path);
    }

    /// Paths changed between the two compared commits, once loaded.
    #[cfg(test)]
    pub(crate) fn compared_paths(&self) -> Option<Vec<String>> {
        match self.compare.state() {
            TaskState::Done(f) => Some(f.iter().map(|f| f.path.clone()).collect()),
            _ => None,
        }
    }
}

/// Something the user asked for inside the panel, applied after rendering.
enum Act {
    SelectCommit(ObjectId),
    FileHistory(String),
    /// Blame `path` at a revision (`None`: the working copy).
    Blame(String, Option<String>),
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let hist = snap.history.clone();
    // Selected commits of the active list, in display order when few.
    let (count, ids, preferred) = if tab.view == View::Reflog {
        match tab.reflog.selected_id() {
            Some(id) => (1, vec![id], None),
            None => (0, Vec::new(), None),
        }
    } else if tab.view == View::Search {
        let s = &tab.search;
        let ids = if s.sel.len() <= 2 {
            s.sel.sorted(|k| s.row_of(snap, k))
        } else {
            Vec::new()
        };
        let pref = ids
            .first()
            .filter(|_| ids.len() == 1)
            .and_then(|id| s.path_at(id));
        (s.sel.len(), ids, pref)
    } else {
        let s = &tab.history.sel;
        let ids = if s.len() <= 2 {
            s.sorted(|k| hist.row(k))
        } else {
            Vec::new()
        };
        (s.len(), ids, None)
    };
    let mut act = None;
    match count {
        0 => placeholder(ui, "Select a commit to see its details."),
        1 => single(ui, tab, snap, shared, ids[0], preferred, &mut act),
        2 => compare(ui, tab, shared, ids[1], ids[0], &mut act),
        n => placeholder(ui, &format!("{n} commits selected.")),
    }
    apply(act, tab, snap);
}

fn apply(act: Option<Act>, tab: &mut RepoTab, snap: &Snapshot) {
    match act {
        Some(Act::SelectCommit(id)) => select_anywhere(tab, snap, id),
        Some(Act::FileHistory(path)) => crate::views::search::open_file_history(tab, path),
        Some(Act::Blame(path, rev)) => crate::views::blame::open_blame(tab, path, rev),
        None => {}
    }
}

/// Changed-file list and diff of `files` (as in commit details), for other
/// views such as the stash view. `rev` names the newer side (for blame and
/// "open file at revision"); `target` identifies what is shown so the file
/// selection resets when it changes.
pub fn files_view(
    ui: &mut Ui,
    tab: &mut RepoTab,
    snap: &Snapshot,
    shared: &mut Shared,
    files: &[ChangedFile],
    rev: &str,
    target: ObjectId,
) {
    retarget(&mut tab.details, (target, None), None);
    let mut act = None;
    files_and_diff(ui, tab, shared, files, rev, &mut act);
    apply(act, tab, snap);
}

/// Bumped whenever a note is added or removed, so the details reload notes.
pub static NOTES_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `git notes show <id>` of the shown commit; `None` when it has no note.
fn note_of(tab: &mut RepoTab, ctx: &egui::Context, id: ObjectId) -> Option<Arc<Option<String>>> {
    let generation = NOTES_GEN.load(std::sync::atomic::Ordering::SeqCst);
    let git = tab.session.repo().git().clone();
    match tab.details.notes.get(&(id, generation), ctx, move || {
        let hex = id.to_string();
        let o = git.run_raw(&["notes", "show", hex.as_str()], None)?;
        Ok((o.code == Some(0))
            .then(|| o.stdout_str().trim_end().to_string())
            .filter(|s| !s.is_empty()))
    }) {
        TaskState::Done(n) => Some(n),
        _ => None,
    }
}

fn placeholder(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| ui.label(RichText::new(text).weak()));
}

/// Selects `id` in the active list, falling back to the history view.
fn select_anywhere(tab: &mut RepoTab, snap: &Snapshot, id: ObjectId) {
    if tab.view == View::Search && tab.search.row_of(snap, &id).is_some() {
        tab.search.select_commit(id);
    } else if snap.history.row(&id).is_some() {
        tab.view = View::History;
        tab.history.select_commit(id);
    } else {
        tab.session.push_message(
            gk_core::MessageKind::Info,
            format!(
                "Commit {} is not part of the displayed history.",
                crate::util::short_id(&id.to_string())
            ),
        );
    }
}

fn single(
    ui: &mut Ui,
    tab: &mut RepoTab,
    snap: &Snapshot,
    shared: &mut Shared,
    id: ObjectId,
    preferred: Option<String>,
    act: &mut Option<Act>,
) {
    let d = match tab.session.commit_details(id) {
        Load::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading commit…");
            });
            return;
        }
        Load::Failed(e) => {
            ui.colored_label(shared.palette.removed_fg, e);
            return;
        }
        Load::Ready(d) => d,
    };
    // The session's slot may still hold the previous commit for a frame.
    if d.id != id {
        ui.spinner();
        return;
    }
    retarget(&mut tab.details, (id, None), preferred);
    let note = note_of(tab, ui.ctx(), id);
    let max_h = (ui.available_height() * 0.45).clamp(60.0, 240.0);
    egui::ScrollArea::vertical()
        .id_salt(("details-header", id))
        .max_height(max_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            header(ui, snap, shared, &d, act);
            if let Some(n) = note.as_deref().and_then(|n| n.as_deref()) {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Note:").color(shared.palette.muted));
                    ui.add(egui::Label::new(RichText::new(n).italics()).selectable(true));
                });
            }
        });
    ui.separator();
    files_and_diff(ui, tab, shared, &d.files, &id.to_string(), act);
}

fn header(
    ui: &mut Ui,
    snap: &Snapshot,
    shared: &Shared,
    d: &gk_core::content::CommitDetails,
    act: &mut Option<Act>,
) {
    let msg = d.message.trim_end();
    let (summary, body) = match msg.split_once('\n') {
        Some((s, b)) => (s.trim(), b.trim()),
        None => (msg.trim(), ""),
    };
    let size = egui::TextStyle::Body.resolve(ui.style()).size + 1.0;
    ui.add(egui::Label::new(RichText::new(summary).strong().size(size)).selectable(true));
    if !body.is_empty() {
        ui.add(egui::Label::new(body).selectable(true));
    }
    ui.add_space(4.0);
    let muted = shared.palette.muted;
    let relative = shared.settings.history.relative_dates;
    egui::Grid::new(("details-grid", d.id))
        .num_columns(2)
        .spacing([10.0, 3.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Commit:").color(muted));
            ui.horizontal(|ui| {
                let hex = d.id.to_string();
                ui.add(egui::Label::new(RichText::new(&hex).monospace()).selectable(true));
                if ui.small_button("Copy").on_hover_text("Copy SHA").clicked() {
                    ui.ctx().copy_text(hex);
                }
            });
            ui.end_row();
            if !d.parents.is_empty() {
                ui.label(
                    RichText::new(if d.parents.len() > 1 {
                        "Parents:"
                    } else {
                        "Parent:"
                    })
                    .color(muted),
                );
                ui.horizontal(|ui| {
                    for p in &d.parents {
                        let s = p.to_string();
                        if ui
                            .link(RichText::new(crate::util::short_id(&s)).monospace())
                            .on_hover_text(&s)
                            .clicked()
                        {
                            *act = Some(Act::SelectCommit(*p));
                        }
                    }
                });
                ui.end_row();
            }
            ui.label(RichText::new("Author:").color(muted));
            ui.add(egui::Label::new(format!("{} <{}>", d.author, d.author_email)).selectable(true));
            ui.end_row();
            ui.label(RichText::new("Date:").color(muted));
            ui.horizontal(|ui| {
                ui.label(crate::util::format_time(
                    d.author_time,
                    d.author_offset,
                    false,
                ));
                if !relative {
                    ui.label(
                        RichText::new(format!("({})", crate::util::relative(d.author_time)))
                            .color(muted),
                    );
                }
            });
            ui.end_row();
            if d.committer != d.author
                || d.committer_email != d.author_email
                || d.committer_time != d.author_time
            {
                ui.label(RichText::new("Committer:").color(muted));
                ui.add(
                    egui::Label::new(format!(
                        "{} <{}>  {}",
                        d.committer,
                        d.committer_email,
                        crate::util::format_time(d.committer_time, d.author_offset, false)
                    ))
                    .selectable(true),
                );
                ui.end_row();
            }
            if let Some(ls) = table::labels_of(&snap.refs.labels, &d.id) {
                ui.label(RichText::new("Refs:").color(muted));
                ui.horizontal_wrapped(|ui| {
                    for l in ls {
                        let (text, color, bold) = match l {
                            Label::Head => ("HEAD".to_string(), shared.palette.head, true),
                            Label::Branch { name, is_head } => (
                                name.clone(),
                                if *is_head {
                                    shared.palette.head
                                } else {
                                    shared.palette.branch
                                },
                                *is_head,
                            ),
                            Label::Remote(n) => (n.clone(), shared.palette.remote, false),
                            Label::Tag(n) => (n.clone(), shared.palette.tag, false),
                            Label::Stash => ("stash".to_string(), shared.palette.stash, false),
                        };
                        crate::util::chip(ui, &text, color, bold);
                    }
                });
                ui.end_row();
            }
        });
}

fn compare(
    ui: &mut Ui,
    tab: &mut RepoTab,
    shared: &mut Shared,
    old: ObjectId,
    new: ObjectId,
    act: &mut Option<Act>,
) {
    let metas = tab.session.metas(&[old, new]);
    let muted = shared.palette.muted;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Comparing").strong());
        for (i, (id, m)) in [old, new].iter().zip(&metas).enumerate() {
            if i == 1 {
                ui.label("→");
            }
            let s = id.to_string();
            if ui
                .link(RichText::new(crate::util::short_id(&s)).monospace())
                .on_hover_text(&s)
                .clicked()
            {
                *act = Some(Act::SelectCommit(*id));
            }
            if let Some(m) = m {
                ui.label(RichText::new(&m.summary).color(muted));
            }
        }
    });
    ui.separator();
    let repo = tab.session.repo().clone();
    let files = match tab.details.compare.get(&(old, new), ui.ctx(), move || {
        gk_core::content::compare_commits(&repo, old, new)
    }) {
        TaskState::Done(f) => f,
        TaskState::Failed(e) => {
            ui.colored_label(shared.palette.removed_fg, e);
            return;
        }
        _ => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Comparing…");
            });
            return;
        }
    };
    retarget(&mut tab.details, (new, Some(old)), None);
    files_and_diff(ui, tab, shared, &files, &new.to_string(), act);
}

/// On a new commit/pair, prefer `preferred` as the selected file.
fn retarget(
    st: &mut DetailsState,
    target: (ObjectId, Option<ObjectId>),
    preferred: Option<String>,
) {
    if st.last_target != Some(target) {
        st.last_target = Some(target);
        st.filtered_key = None;
        if preferred.is_some() {
            st.selected_file = preferred;
        }
    }
}

/// Indices of `files` whose path contains `filter` (case-insensitive).
pub fn filter_files(files: &[ChangedFile], filter: &str) -> Vec<usize> {
    let f = filter.trim().to_lowercase();
    files
        .iter()
        .enumerate()
        .filter(|(_, x)| {
            f.is_empty()
                || x.path.to_lowercase().contains(&f)
                || x.old_path
                    .as_ref()
                    .is_some_and(|o| o.to_lowercase().contains(&f))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Index of the file to show: the current choice when present, else the first.
pub fn pick_file(
    files: &[ChangedFile],
    visible: &[usize],
    selected: Option<&str>,
) -> Option<usize> {
    selected
        .and_then(|s| {
            visible
                .iter()
                .copied()
                .find(|&i| files[i].path == s)
                .or_else(|| {
                    visible
                        .iter()
                        .copied()
                        .find(|&i| files[i].old_path.as_deref() == Some(s))
                })
        })
        .or_else(|| visible.first().copied())
}

fn kind_color(k: ChangeKind, p: &crate::theme::Palette) -> Color32 {
    match k {
        ChangeKind::Added | ChangeKind::Copied => p.added_fg,
        ChangeKind::Deleted => p.removed_fg,
        ChangeKind::Renamed => p.branch,
        _ => p.modified,
    }
}

enum FileAct {
    Open(ObjectId, String),
    CopyPath(String),
    History(String),
    Blame(String, Option<String>),
    External(gk_core::content::DiffTarget),
}

fn files_and_diff(
    ui: &mut Ui,
    tab: &mut RepoTab,
    shared: &mut Shared,
    files: &[ChangedFile],
    rev: &str,
    act: &mut Option<Act>,
) {
    let mut frac = tab.state.layout.detail_split;
    let (left, right) = split(
        ui,
        Id::new(("details-split", &tab.path)),
        &mut frac,
        false,
        120.0,
    );
    tab.state.layout.detail_split = frac;

    let st = &mut tab.details;
    let key = (files.as_ptr() as usize, files.len(), st.filter.clone());
    if st.filtered_key.as_ref() != Some(&key) {
        st.filtered = filter_files(files, &st.filter);
        st.filtered_key = Some(key);
    }
    let current = pick_file(files, &st.filtered, st.selected_file.as_deref());
    let workdir = tab.path.clone();
    let mut file_act = None;
    in_rect(ui, left, "details-files", |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut st.filter)
                    .hint_text("Filter files…")
                    .desired_width(ui.available_width() - 60.0),
            );
            ui.label(RichText::new(format!("{}", files.len())).color(shared.palette.muted))
                .on_hover_text("Changed files");
        });
        let row_h = ui.spacing().interact_size.y;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        egui::ScrollArea::vertical()
            .id_salt("details-file-list")
            .auto_shrink(false)
            .show_rows(ui, row_h, st.filtered.len(), |ui, range| {
                for vi in range {
                    let i = st.filtered[vi];
                    let f = &files[i];
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    let selected = current == Some(i);
                    let p = ui.painter();
                    if selected {
                        p.rect_filled(rect, 0.0, shared.palette.selection);
                    } else if resp.hovered() {
                        p.rect_filled(
                            rect,
                            0.0,
                            ui.visuals()
                                .widgets
                                .hovered
                                .weak_bg_fill
                                .gamma_multiply(0.5),
                        );
                    }
                    table::text_at(
                        p,
                        rect.left() + 4.0,
                        rect.center().y,
                        f.kind.code().to_string(),
                        &mono,
                        kind_color(f.kind, &shared.palette),
                    );
                    let text = match &f.old_path {
                        Some(o) => format!("{o} → {}", f.path),
                        None => f.path.clone(),
                    };
                    let cell = Rect::from_min_max(pos2(rect.left() + 16.0, rect.top()), rect.max);
                    table::cell_text(p, cell, text, &font, text_color);
                    if resp.clicked() {
                        st.selected_file = Some(f.path.clone());
                    }
                    resp.on_hover_text_at_pointer(&f.path).context_menu(|ui| {
                        if let Some(id) = f.new_id
                            && ui.button("Open file at this revision").clicked()
                        {
                            file_act = Some(FileAct::Open(id, f.path.clone()));
                            ui.close();
                        }
                        if let Some(id) = f.old_id
                            && ui.button("Open previous revision").clicked()
                        {
                            file_act = Some(FileAct::Open(
                                id,
                                f.old_path.clone().unwrap_or_else(|| f.path.clone()),
                            ));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Copy path").clicked() {
                            file_act = Some(FileAct::CopyPath(f.path.clone()));
                            ui.close();
                        }
                        if ui.button("File history").clicked() {
                            file_act = Some(FileAct::History(f.path.clone()));
                            ui.close();
                        }
                        if shared.settings.external_diff_tool.is_some()
                            && ui.button("Open in external diff tool").clicked()
                        {
                            file_act =
                                Some(FileAct::External(gk_core::content::DiffTarget::Blobs {
                                    path: f.path.clone(),
                                    old: f.old_id,
                                    new: f.new_id,
                                }));
                            ui.close();
                        }
                        if f.new_id.is_some() && ui.button("Blame").clicked() {
                            file_act = Some(FileAct::Blame(f.path.clone(), Some(rev.to_string())));
                            ui.close();
                        }
                        if workdir.join(&f.path).is_file()
                            && ui.button("Blame (working copy)").clicked()
                        {
                            file_act = Some(FileAct::Blame(f.path.clone(), None));
                            ui.close();
                        }
                    });
                }
            });
    });
    in_rect(ui, right, "details-diff", |ui| {
        match current.map(|i| &files[i]) {
            Some(f) => {
                let target = DiffTarget::Blobs {
                    path: f.path.clone(),
                    old: f.old_id,
                    new: f.new_id,
                };
                tab.details
                    .diff
                    .show(ui, &tab.session, &target, DiffMode::ReadOnly, 0, shared);
            }
            None => placeholder(
                ui,
                if files.is_empty() {
                    "No file changes."
                } else {
                    "No matching files."
                },
            ),
        }
    });
    match file_act {
        Some(FileAct::Open(id, path)) => open_blob(&tab.session, id, path, rev),
        Some(FileAct::CopyPath(p)) => ui.ctx().copy_text(p),
        Some(FileAct::History(p)) => *act = Some(Act::FileHistory(p)),
        Some(FileAct::Blame(p, r)) => *act = Some(Act::Blame(p, r)),
        Some(FileAct::External(t)) => crate::views::diff::open_external(&tab.session, &t, shared),
        None => {}
    }
}

/// Temp file name for `path` at `rev`: keeps the extension so the right
/// application opens it.
pub fn temp_name(path: &str, rev: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let short = crate::util::short_id(rev);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}@{short}.{ext}"),
        _ => format!("{name}@{short}"),
    }
}

/// Writes blob `id` to a temp file and opens it with the default application (background).
fn open_blob(session: &Session, id: ObjectId, path: String, rev: &str) {
    let session = session.clone();
    let rev = rev.to_string();
    std::thread::spawn(move || {
        let r = (|| -> anyhow::Result<()> {
            let g = session.repo().gix();
            let data = gk_core::content::blob(&g, id)?;
            let dir = std::env::temp_dir().join("gitkwaken");
            std::fs::create_dir_all(&dir)?;
            let file = dir.join(temp_name(&path, &rev));
            std::fs::write(&file, data)?;
            open::that(&file)?;
            Ok(())
        })();
        if let Err(e) = r {
            session.push_message(
                gk_core::MessageKind::Error,
                format!("Opening {path} failed: {e:#}"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, old: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            old_path: old.map(Into::into),
            kind: ChangeKind::Modified,
            old_id: None,
            new_id: None,
        }
    }

    #[test]
    fn filtering_and_picking() {
        let files = vec![
            f("src/Main.rs", None),
            f("README.md", None),
            f("lib/new.rs", Some("lib/old.rs")),
        ];
        assert_eq!(filter_files(&files, ""), vec![0, 1, 2]);
        assert_eq!(filter_files(&files, "main"), vec![0]);
        assert_eq!(filter_files(&files, "OLD"), vec![2]);
        let all = filter_files(&files, "");
        assert_eq!(pick_file(&files, &all, Some("README.md")), Some(1));
        assert_eq!(pick_file(&files, &all, Some("lib/old.rs")), Some(2));
        assert_eq!(pick_file(&files, &all, Some("gone")), Some(0));
        assert_eq!(pick_file(&files, &[], Some("README.md")), None);
    }

    #[test]
    fn temp_names() {
        assert_eq!(
            temp_name("src/main.rs", "0123456789abcdef"),
            "main@01234567.rs"
        );
        assert_eq!(temp_name("Makefile", "0123456789"), "Makefile@01234567");
        assert_eq!(
            temp_name("a/.gitignore", "0123456789"),
            ".gitignore@01234567"
        );
    }
}
