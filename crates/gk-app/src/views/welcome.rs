//! Shown when no repository is open: open, clone, init, scan, bookmarks
//! (grouped) and recents. Also the "Repositories ▾" menu for the tab bar.

use crate::app::{Action, Shared};
use crate::dialogs::GlobalDialog;
use egui::{Id, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use std::path::{Path, PathBuf};

/// Width of the centered content block.
const CONTENT_W: f32 = 860.0;

/// A rename / new-group prompt (kept in egui temp memory between frames).
#[derive(Clone, Default)]
enum Prompt {
    #[default]
    None,
    RenameBookmark {
        path: PathBuf,
        name: String,
    },
    RenameGroup {
        old: String,
        name: String,
    },
    /// New group, moving `path` into it when given.
    NewGroup {
        path: Option<PathBuf>,
        name: String,
    },
}

/// A change to the bookmarks, applied after rendering.
enum Change {
    Remove(PathBuf),
    Move(PathBuf, Option<String>),
    RemoveGroup(String),
    Prompt(Prompt),
}

pub fn show(ui: &mut Ui, shared: &mut Shared) {
    let full = ui.available_rect_before_wrap();
    let w = CONTENT_W.min(full.width() - 32.0).max(200.0);
    let block = Rect::from_min_max(
        pos2(full.center().x - w / 2.0, full.top()),
        pos2(full.center().x + w / 2.0, full.bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(block), |ui| content(ui, shared));
    prompt_ui(ui, shared);
}

fn content(ui: &mut Ui, shared: &mut Shared) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(RichText::new("GitKwaken").heading().strong());
        ui.label(RichText::new("Fast git client").color(shared.palette.muted));
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 430.0).max(0.0) / 2.0);
            if ui.button("Open repository…").clicked() {
                shared.actions.push(Action::OpenRepoDialog);
            }
            if ui.button("Clone…").clicked() {
                shared
                    .actions
                    .push(Action::Global(GlobalDialog::clone_repo()));
            }
            if ui.button("Create new…").clicked() {
                shared
                    .actions
                    .push(Action::Global(GlobalDialog::init_repo()));
            }
            if ui
                .button("Scan folder…")
                .on_hover_text("Find repositories in a folder tree")
                .clicked()
            {
                shared
                    .actions
                    .push(Action::Global(GlobalDialog::scan_repos()));
            }
        });
    });
    ui.add_space(28.0);
    let muted = shared.palette.muted;
    let mut open = None;
    let mut bookmark = None;
    let mut changes: Vec<Change> = Vec::new();
    ui.columns(2, |cols| {
        let r = section_title(&mut cols[0], "Bookmarks", shared.settings.bookmarks.len());
        r.context_menu(|ui| {
            if ui.button("New group…").clicked() {
                changes.push(Change::Prompt(Prompt::NewGroup {
                    path: None,
                    name: String::new(),
                }));
                ui.close();
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("bookmarks")
            .auto_shrink([false, true])
            .show(&mut cols[0], |ui| {
                bookmarks(ui, shared, muted, &mut open, &mut changes);
            });
        section_title(&mut cols[1], "Recent", shared.settings.recent_repos.len());
        egui::ScrollArea::vertical()
            .id_salt("recent")
            .auto_shrink([false, true])
            .show(&mut cols[1], |ui| {
                for p in &shared.settings.recent_repos {
                    let r = repo_item(ui, &name_of(p), p, muted);
                    if r.clicked() {
                        open = Some(p.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button("Bookmark").clicked() {
                            bookmark = Some(p.clone());
                            ui.close();
                        }
                    });
                }
                if shared.settings.recent_repos.is_empty() {
                    placeholder(ui, "Repositories you open show up here.", muted);
                }
            });
    });
    if let Some(p) = bookmark {
        shared
            .actions
            .push(Action::Global(GlobalDialog::bookmark(p)));
    } else if let Some(p) = open {
        shared.actions.push(Action::OpenRepo(p));
    }
    for c in changes {
        match c {
            Change::Remove(p) => {
                shared.settings.remove_bookmark(&p);
            }
            Change::Move(p, g) => {
                shared.settings.move_bookmark_to_group(&p, g);
            }
            Change::RemoveGroup(g) => {
                shared.settings.remove_group(&g);
            }
            Change::Prompt(p) => set_prompt(ui.ctx(), p),
        }
    }
}

/// Group names in display order: configured groups, then any group only
/// bookmarks mention.
pub fn group_names(s: &gk_settings::Settings) -> Vec<String> {
    let mut v = s.groups.clone();
    for b in &s.bookmarks {
        if let Some(g) = &b.group
            && !v.contains(g)
        {
            v.push(g.clone());
        }
    }
    v
}

fn bookmarks(
    ui: &mut Ui,
    shared: &Shared,
    muted: egui::Color32,
    open: &mut Option<PathBuf>,
    changes: &mut Vec<Change>,
) {
    let s = &*shared.settings;
    if s.bookmarks.is_empty() && s.groups.is_empty() {
        placeholder(
            ui,
            "No bookmarks yet. Right-click a recent repository to add one, or scan a folder.",
            muted,
        );
        return;
    }
    let groups = group_names(s);
    let item = |ui: &mut Ui,
                b: &gk_settings::Bookmark,
                open: &mut Option<PathBuf>,
                changes: &mut Vec<Change>| {
        let r = repo_item(ui, &b.name, &b.path, muted);
        if r.clicked() {
            *open = Some(b.path.clone());
        }
        r.context_menu(|ui| bookmark_menu(ui, b, &groups, changes));
    };
    for b in s.bookmarks.iter().filter(|b| b.group.is_none()) {
        item(ui, b, open, changes);
    }
    for g in &groups {
        let n = s
            .bookmarks
            .iter()
            .filter(|b| b.group.as_ref() == Some(g))
            .count();
        let header = egui::CollapsingHeader::new(RichText::new(format!("{g}  ({n})")).strong())
            .id_salt(("bookmark-group", g))
            .default_open(true)
            .show(ui, |ui| {
                for b in s.bookmarks.iter().filter(|b| b.group.as_ref() == Some(g)) {
                    item(ui, b, open, changes);
                }
                if n == 0 {
                    placeholder(ui, "Empty group.", muted);
                }
            });
        header.header_response.context_menu(|ui| {
            if ui.button("Rename group…").clicked() {
                changes.push(Change::Prompt(Prompt::RenameGroup {
                    old: g.clone(),
                    name: g.clone(),
                }));
                ui.close();
            }
            if ui
                .button("Remove group")
                .on_hover_text("Its bookmarks become ungrouped")
                .clicked()
            {
                changes.push(Change::RemoveGroup(g.clone()));
                ui.close();
            }
        });
    }
}

fn bookmark_menu(
    ui: &mut Ui,
    b: &gk_settings::Bookmark,
    groups: &[String],
    changes: &mut Vec<Change>,
) {
    if ui.button("Rename…").clicked() {
        changes.push(Change::Prompt(Prompt::RenameBookmark {
            path: b.path.clone(),
            name: b.name.clone(),
        }));
        ui.close();
    }
    ui.menu_button("Move to group", |ui| {
        if ui
            .add_enabled(b.group.is_some(), egui::Button::new("(no group)"))
            .clicked()
        {
            changes.push(Change::Move(b.path.clone(), None));
            ui.close();
        }
        for g in groups {
            if ui
                .add_enabled(b.group.as_ref() != Some(g), egui::Button::new(g))
                .clicked()
            {
                changes.push(Change::Move(b.path.clone(), Some(g.clone())));
                ui.close();
            }
        }
        ui.separator();
        if ui.button("New group…").clicked() {
            changes.push(Change::Prompt(Prompt::NewGroup {
                path: Some(b.path.clone()),
                name: String::new(),
            }));
            ui.close();
        }
    });
    if ui.button("Show in Explorer").clicked() {
        let _ = open::that(&b.path);
        ui.close();
    }
    ui.separator();
    if ui.button("Remove bookmark").clicked() {
        changes.push(Change::Remove(b.path.clone()));
        ui.close();
    }
}

fn prompt_id() -> Id {
    Id::new("welcome-prompt")
}

fn set_prompt(ctx: &egui::Context, p: Prompt) {
    ctx.data_mut(|d| d.insert_temp(prompt_id(), p));
}

/// The rename / new group prompt, if one is open.
fn prompt_ui(ui: &mut Ui, shared: &mut Shared) {
    let ctx = ui.ctx().clone();
    let mut p: Prompt = ctx.data_mut(|d| d.get_temp(prompt_id()).unwrap_or_default());
    let (title, name) = match &mut p {
        Prompt::None => return,
        Prompt::RenameBookmark { name, .. } => ("Rename bookmark", name),
        Prompt::RenameGroup { name, .. } => ("Rename group", name),
        Prompt::NewGroup { name, .. } => ("New group", name),
    };
    let mut ok = false;
    let mut cancel = false;
    let resp = egui::Modal::new(Id::new("welcome-prompt-modal")).show(&ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(title);
        let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            ok = ui
                .add_enabled(!name.trim().is_empty(), egui::Button::new("OK"))
                .clicked()
                || (enter && !name.trim().is_empty());
            cancel = ui.button("Cancel").clicked();
        });
    });
    if resp.should_close() {
        cancel = true;
    }
    if ok {
        let s = &mut *shared.settings;
        match &p {
            Prompt::RenameBookmark { path, name } => {
                s.rename_bookmark(path, name.trim());
            }
            Prompt::RenameGroup { old, name } => {
                if !s.groups.contains(old) {
                    s.add_group(old.clone());
                }
                s.rename_group(old, name.trim());
            }
            Prompt::NewGroup { path, name } => {
                s.add_group(name.trim());
                if let Some(path) = path {
                    s.move_bookmark_to_group(path, Some(name.trim().to_string()));
                }
            }
            Prompt::None => {}
        }
    }
    set_prompt(&ctx, if ok || cancel { Prompt::None } else { p });
}

/// What the "Repositories ▾" menu offers for the open repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentBookmark {
    /// No repository tab is active.
    None,
    /// "Bookmark this repository…".
    Add,
    /// "Remove bookmark".
    Remove,
}

pub fn current_bookmark(s: &gk_settings::Settings, current: Option<&Path>) -> CurrentBookmark {
    match current {
        None => CurrentBookmark::None,
        Some(p) if s.find_bookmark(p).is_some() => CurrentBookmark::Remove,
        Some(_) => CurrentBookmark::Add,
    }
}

/// Repositories menu titled `title`: bookmark / unbookmark the open
/// repository, bookmarks by group, recents, open and scan. Used by the tab
/// row ("Repositories ▾") and File › Repositories in the menu bar.
pub fn repositories_menu(ui: &mut Ui, shared: &mut Shared, current: Option<&Path>, title: &str) {
    let mut open: Option<PathBuf> = None;
    let mut action: Option<Action> = None;
    let mut unbookmark: Option<PathBuf> = None;
    ui.menu_button(title, |ui| {
        let s = &*shared.settings;
        if let Some(cur) = current {
            match current_bookmark(s, current) {
                CurrentBookmark::Add => {
                    if ui
                        .button("Bookmark this repository…")
                        .on_hover_text(cur.display().to_string())
                        .clicked()
                    {
                        action = Some(Action::Global(GlobalDialog::bookmark(cur.to_path_buf())));
                        ui.close();
                    }
                }
                CurrentBookmark::Remove => {
                    let name = s
                        .find_bookmark(cur)
                        .map(|b| b.name.clone())
                        .unwrap_or_default();
                    if ui
                        .button("Remove bookmark")
                        .on_hover_text(format!("Remove the bookmark \"{name}\" of this repository"))
                        .clicked()
                    {
                        unbookmark = Some(cur.to_path_buf());
                        ui.close();
                    }
                }
                CurrentBookmark::None => {}
            }
            ui.separator();
        }
        let pick = |ui: &mut Ui, name: &str, path: &Path, open: &mut Option<PathBuf>| {
            if ui
                .button(name)
                .on_hover_text(path.display().to_string())
                .clicked()
            {
                *open = Some(path.to_path_buf());
                ui.close();
            }
        };
        ui.label(RichText::new("Bookmarks").weak());
        for b in s.bookmarks.iter().filter(|b| b.group.is_none()) {
            pick(ui, &b.name, &b.path, &mut open);
        }
        for g in group_names(s) {
            ui.menu_button(format!("📁 {g}"), |ui| {
                let mut any = false;
                for b in s
                    .bookmarks
                    .iter()
                    .filter(|b| b.group.as_deref() == Some(g.as_str()))
                {
                    any = true;
                    pick(ui, &b.name, &b.path, &mut open);
                }
                if !any {
                    ui.label(RichText::new("Empty group").weak());
                }
            });
        }
        if s.bookmarks.is_empty() {
            ui.label(RichText::new("No bookmarks").weak());
        }
        ui.separator();
        ui.menu_button("Recent", |ui| {
            for p in &s.recent_repos {
                pick(ui, &name_of(p), p, &mut open);
            }
            if s.recent_repos.is_empty() {
                ui.label(RichText::new("Nothing yet").weak());
            }
        });
        ui.separator();
        if ui.button("Open repository…").clicked() {
            action = Some(Action::OpenRepoDialog);
            ui.close();
        }
        if ui.button("Scan folder for repositories…").clicked() {
            action = Some(Action::Global(GlobalDialog::scan_repos()));
            ui.close();
        }
    });
    if let Some(p) = open {
        shared.actions.push(Action::OpenRepo(p));
    }
    if let Some(a) = action {
        shared.actions.push(a);
    }
    if let Some(p) = unbookmark {
        shared.settings.remove_bookmark(&p);
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn section_title(ui: &mut Ui, title: &str, count: usize) -> egui::Response {
    let r = ui
        .horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            if count > 0 {
                ui.label(RichText::new(count.to_string()).weak());
            }
        })
        .response
        .interact(Sense::click());
    ui.separator();
    r
}

fn placeholder(ui: &mut Ui, text: &str, muted: egui::Color32) {
    ui.add_space(4.0);
    ui.add(egui::Label::new(RichText::new(text).color(muted)).wrap());
}

/// A two-line clickable entry: repository name over its (truncated) path.
fn repo_item(ui: &mut Ui, name: &str, path: &Path, muted: egui::Color32) -> egui::Response {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let h = body.size + small.size + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let text_w = rect.width() - 16.0;
        let x = rect.left() + 8.0;
        let name_g = crate::views::history::table::text_trunc(
            p,
            name.to_string(),
            body.clone(),
            ui.visuals().strong_text_color(),
            text_w,
        );
        let path_g = crate::views::history::table::text_trunc(
            p,
            path.display().to_string(),
            small,
            muted,
            text_w,
        );
        let top = rect.top() + 5.0;
        let name_h = name_g.size().y;
        p.galley(pos2(x, top), name_g, muted);
        p.galley(pos2(x, top + name_h + 1.0), path_g, muted);
    }
    resp.on_hover_text(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_repository_bookmark_entry() {
        let mut s = gk_settings::Settings::default();
        let repo = Path::new("C:/work/repo");
        assert_eq!(current_bookmark(&s, None), CurrentBookmark::None);
        assert_eq!(current_bookmark(&s, Some(repo)), CurrentBookmark::Add);
        s.add_bookmark("repo", repo, Some("Work".into()));
        assert_eq!(current_bookmark(&s, Some(repo)), CurrentBookmark::Remove);
        s.remove_bookmark(repo);
        assert_eq!(current_bookmark(&s, Some(repo)), CurrentBookmark::Add);
    }
}
