//! SourceTree-style menu bar: File, Edit, View, Repository, Actions, Tools,
//! Help. Entries run the command palette's [`Cmd`]s through
//! [`palette::execute`], so a menu item and its palette entry share one code
//! path; shortcuts come from the active keymap.

use crate::app::{Action, Shared};
use crate::dialogs::{Dialog, GlobalDialog};
use crate::keymap::{Act, Keymap};
use crate::tab::{RepoTab, View};
use crate::views::history::keys;
use crate::views::palette::{self, Cmd};
use egui::Ui;

/// App-level requests the menu bar cannot carry out itself.
#[derive(Default)]
pub struct MenuOut {
    pub toggle_palette: bool,
    pub update: Option<crate::update::UpdateAction>,
}

/// A top-level menu; its button is collected for [`switch_on_hover`].
fn top<R>(
    ui: &mut Ui,
    bar: &mut Vec<egui::Response>,
    label: &str,
    content: impl FnOnce(&mut Ui) -> R,
) {
    bar.push(ui.menu_button(label, content).response);
}

/// Windows-style menu bar: while one menu is open, pointing at another
/// top-level button opens that one instead (egui only switches on click).
fn switch_on_hover(ctx: &egui::Context, bar: &[egui::Response]) {
    use egui::containers::Popup;
    let ids: Vec<egui::Id> = bar.iter().map(Popup::default_response_id).collect();
    if !ids.iter().any(|&id| Popup::is_id_open(ctx, id)) {
        return;
    }
    if let Some((_, &id)) = bar
        .iter()
        .zip(&ids)
        .find(|(r, id)| r.contains_pointer() && !Popup::is_id_open(ctx, **id))
    {
        Popup::open_id(ctx, id);
        ctx.request_repaint();
    }
}

/// Width of one caption button (minimize, maximize, close), as on Windows 11.
const CAPTION_W: f32 = 46.0;

/// Caption of a window without system decorations: the free part of the menu
/// bar drags the window (double click maximizes / restores), then minimize,
/// maximize / restore and close on the right, flush with the window corner.
fn caption(ui: &mut Ui) {
    use egui::{Sense, Stroke, ViewportCommand, pos2, vec2};
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let row = ui.max_rect();
    // The panel's top / right margin belongs to the caption too.
    let top = row.top() - 2.0;
    let right = row.right() + 6.0;
    let buttons_left = right - 3.0 * CAPTION_W;
    let drag = egui::Rect::from_min_max(
        pos2(ui.cursor().left() + 8.0, top),
        pos2(buttons_left, row.bottom()),
    );
    if drag.width() > 0.0 {
        let r = ui.interact(drag, ui.id().with("caption-drag"), Sense::click_and_drag());
        if r.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if r.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }
    let painter = ui.painter().with_clip_rect(ctx.content_rect());
    let text = ui.visuals().text_color();
    for (i, kind) in ["min", "max", "close"].into_iter().enumerate() {
        let rect = egui::Rect::from_min_max(
            pos2(buttons_left + i as f32 * CAPTION_W, top),
            pos2(buttons_left + (i + 1) as f32 * CAPTION_W, row.bottom()),
        );
        let r = ui.interact(rect, ui.id().with(("caption", kind)), Sense::click());
        let close = kind == "close";
        let (fill, fg) = match (r.hovered(), r.is_pointer_button_down_on(), close) {
            (true, _, true) => (
                egui::Color32::from_rgb(0xC4, 0x2B, 0x1C),
                egui::Color32::WHITE,
            ),
            (true, true, false) => (ui.visuals().widgets.active.weak_bg_fill, text),
            (true, false, false) => (ui.visuals().widgets.hovered.weak_bg_fill, text),
            _ => (egui::Color32::TRANSPARENT, text),
        };
        painter.rect_filled(rect, 0.0, fill);
        let c = rect.center();
        let s = Stroke::new(1.0, fg);
        let h = 5.0;
        match kind {
            "min" => {
                painter.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], s);
            }
            "max" if maximized => {
                let front = egui::Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0));
                painter.rect_stroke(front, 0.0, s, egui::StrokeKind::Inside);
                painter.line_segment(
                    [
                        pos2(front.left() + 2.0, front.top() - 2.0),
                        pos2(front.right() + 2.0, front.top() - 2.0),
                    ],
                    s,
                );
                painter.line_segment(
                    [
                        pos2(front.right() + 2.0, front.top() - 2.0),
                        pos2(front.right() + 2.0, front.bottom() - 2.0),
                    ],
                    s,
                );
            }
            "max" => {
                let sq = egui::Rect::from_center_size(c, vec2(10.0, 10.0));
                painter.rect_stroke(sq, 0.0, s, egui::StrokeKind::Inside);
            }
            _ => {
                painter.line_segment([pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)], s);
                painter.line_segment([pos2(c.x - h, c.y + h), pos2(c.x + h, c.y - h)], s);
            }
        }
        let tip = match kind {
            "min" => "Minimize",
            "max" if maximized => "Restore",
            "max" => "Maximize",
            _ => "Close",
        };
        if r.on_hover_text(tip).clicked() {
            ctx.send_viewport_cmd(match kind {
                "min" => ViewportCommand::Minimized(true),
                "max" => ViewportCommand::Maximized(!maximized),
                _ => ViewportCommand::Close,
            });
        }
    }
}

/// One menu entry with its shortcut; closes the menu when clicked.
fn item(ui: &mut Ui, label: &str, shortcut: String, enabled: bool) -> bool {
    let mut b = egui::Button::new(label);
    if !shortcut.is_empty() {
        b = b.shortcut_text(egui::RichText::new(shortcut).weak());
    }
    let clicked = ui.add_enabled(enabled, b).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Checkbox entry (keeps the menu open, like egui checkboxes in menus).
fn check(ui: &mut Ui, on: &mut bool, label: &str) -> bool {
    ui.checkbox(on, label).changed()
}

pub struct MenuCtx<'a> {
    pub km: &'a Keymap,
    pub tab: Option<&'a mut RepoTab>,
    pub active: usize,
    pub tabs: usize,
    /// Help-menu update entry (label, enabled, action): one fixed slot.
    pub update: (String, bool, crate::update::UpdateAction),
    /// The window has no system title bar: the menu bar is the caption.
    pub caption: bool,
}

pub fn show(ui: &mut Ui, mc: MenuCtx, shared: &mut Shared) -> MenuOut {
    let MenuCtx {
        km,
        tab,
        active,
        tabs,
        update,
        caption: with_caption,
    } = mc;
    let mut bar = Vec::new();
    let k = |a: Act| km.text(a);
    let none = String::new;
    let repo = tab.is_some();
    let mut out = MenuOut::default();
    let mut cmd: Option<Cmd> = None;
    let mut set = |c: Cmd| cmd = Some(c);
    let current = tab.as_ref().map(|t| t.path.clone());

    egui::MenuBar::new().ui(ui, |ui| {
        top(ui, &mut bar, "File", |ui| {
            if item(ui, "New tab / Open repository…", k(Act::OpenRepo), true) {
                set(Cmd::App(|| Action::OpenRepoDialog));
            }
            if item(ui, "Clone repository…", none(), true) {
                set(Cmd::Global(GlobalDialog::clone_repo));
            }
            if item(ui, "Create repository…", none(), true) {
                set(Cmd::Global(GlobalDialog::init_repo));
            }
            if item(ui, "Scan folder for repositories…", none(), true) {
                set(Cmd::Global(GlobalDialog::scan_repos));
            }
            crate::views::welcome::repositories_menu(
                ui,
                shared,
                current.as_deref(),
                "Bookmarks and recent",
            );
            ui.separator();
            if item(ui, "Show in Explorer", none(), repo) {
                set(Cmd::Explorer);
            }
            if item(ui, "Close tab", k(Act::CloseTab), repo) {
                shared.actions.push(Action::CloseTab(active));
            }
            ui.separator();
            if item(ui, "Exit", none(), true) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });

        top(ui, &mut bar, "Edit", |ui| {
            if item(ui, "Undo last operation…", none(), repo) {
                set(Cmd::Dialog(|_| Dialog::undo()));
            }
            if item(ui, "Discarded changes… (restore)", none(), repo) {
                set(Cmd::Dialog(|_| Dialog::discarded()));
            }
            ui.separator();
            if item(ui, "Copy SHA of selected commits", "Ctrl+C".into(), repo) {
                set(Cmd::CopyShas(false));
            }
            if item(ui, "Copy short SHA", none(), repo) {
                set(Cmd::CopyShas(true));
            }
            ui.separator();
            if item(ui, "Find commits…", k(Act::ViewSearch), repo) {
                set(Cmd::View(View::Search));
            }
            if item(ui, "Command palette…", k(Act::Palette), true) {
                out.toggle_palette = true;
            }
        });

        top(ui, &mut bar, "View", |ui| {
            let view = tab.as_ref().map(|t| t.view);
            for (label, v, act) in [
                ("File Status", View::FileStatus, Some(Act::ViewStatus)),
                ("History", View::History, Some(Act::ViewHistory)),
                ("Search", View::Search, Some(Act::ViewSearch)),
                ("Reflog", View::Reflog, None),
            ] {
                let mark = if view == Some(v) { "● " } else { "    " };
                if item(
                    ui,
                    &format!("{mark}{label}"),
                    act.map(k).unwrap_or_default(),
                    repo,
                ) {
                    set(Cmd::View(v));
                }
            }
            ui.separator();
            if check(ui, &mut *shared.show_log, "Output log") {
                ui.close();
            }
            let mut compact = shared.settings.compact;
            if check(ui, &mut compact, "Compact density") {
                shared.settings.compact = compact;
                shared.actions.push(Action::ApplySettings);
                ui.close();
            }
            ui.separator();
            if item(ui, "Refresh", k(Act::Refresh), repo) {
                set(Cmd::Refresh);
            }
            if item(ui, "Next tab", k(Act::NextTab), tabs > 1) {
                shared.actions.push(Action::SelectTab((active + 1) % tabs));
            }
            if item(ui, "Previous tab", k(Act::PrevTab), tabs > 1) {
                shared
                    .actions
                    .push(Action::SelectTab((active + tabs - 1) % tabs));
            }
        });

        top(ui, &mut bar, "Repository", |ui| {
            ui.add_enabled_ui(repo, |ui| {
                if item(ui, "Fetch…", k(Act::Fetch), true) {
                    set(Cmd::Dialog(Dialog::fetch));
                }
                if item(ui, "Pull…", k(Act::Pull), true) {
                    set(Cmd::Dialog(Dialog::pull));
                }
                if item(ui, "Push…", k(Act::Push), true) {
                    set(Cmd::Dialog(Dialog::push));
                }
                ui.separator();
                if item(ui, "Branch…", k(Act::NewBranch), true) {
                    set(Cmd::Dialog(|s| Dialog::branch(s, None)));
                }
                if item(ui, "Merge…", none(), true) {
                    set(Cmd::Dialog(|s| Dialog::merge(s, None)));
                }
                if item(ui, "Rebase…", none(), true) {
                    set(Cmd::Dialog(|s| Dialog::rebase(s, None)));
                }
                if item(ui, "Tag…", none(), true) {
                    set(Cmd::Dialog(|_| Dialog::tag(None)));
                }
                if item(ui, "Stash changes…", k(Act::Stash), true) {
                    set(Cmd::Dialog(|_| Dialog::stash()));
                }
                ui.separator();
                if item(ui, "Add remote…", none(), true) {
                    set(Cmd::Dialog(|_| Dialog::add_remote()));
                }
                ui.menu_button("Submodules", |ui| {
                    if item(ui, "Add submodule…", none(), true) {
                        set(Cmd::Dialog(|_| Dialog::submodule_add()));
                    }
                    if item(ui, "Update all submodules", none(), true) {
                        set(Cmd::SubmoduleUpdate(None));
                    }
                });
                if item(ui, "Add worktree…", none(), true) {
                    set(Cmd::AddWorktree);
                }
                ui.separator();
                if item(ui, "Git Flow…", none(), true) {
                    set(Cmd::Dialog(Dialog::git_flow));
                }
                if item(ui, "Git LFS…", none(), true) {
                    set(Cmd::Dialog(Dialog::lfs));
                }
                if item(ui, "Sparse checkout…", none(), true) {
                    set(Cmd::Dialog(Dialog::sparse_checkout));
                }
                if item(ui, "Bisect…", none(), true) {
                    set(Cmd::Dialog(Dialog::bisect));
                }
                ui.separator();
                if item(ui, "Apply patch…", none(), true) {
                    set(Cmd::Dialog(|_| Dialog::apply_patch()));
                }
                if item(ui, "Clean working copy…", none(), true) {
                    set(Cmd::Dialog(|_| Dialog::clean()));
                }
                if item(ui, "Optimize repository…", none(), true) {
                    set(Cmd::Dialog(Dialog::optimize));
                }
                ui.separator();
                ui.menu_button("Repository settings", |ui| {
                    if item(ui, "Edit repository config…", none(), true) {
                        set(Cmd::Dialog(|_| Dialog::edit_repo_config()));
                    }
                    if item(ui, "Edit .gitignore…", none(), true) {
                        set(Cmd::Dialog(|_| Dialog::edit_gitignore()));
                    }
                });
            });
        });

        top(ui, &mut bar, "Actions", |ui| {
            ui.add_enabled_ui(repo, |ui| {
                if item(ui, "Commit…", k(Act::Commit), true) {
                    set(Cmd::Commit);
                }
                if item(ui, "Stage all", k(Act::StageAll), true) {
                    set(Cmd::StageAll(true));
                }
                if item(ui, "Unstage all", k(Act::UnstageAll), true) {
                    set(Cmd::StageAll(false));
                }
                if item(ui, "Discard all changes…", none(), true) {
                    set(Cmd::Dialog(|_| Dialog::discard_all()));
                }
                ui.separator();
                ui.label(egui::RichText::new("Selected commit").weak());
                if item(ui, "Checkout…", keys::CHECKOUT_KEY.into(), true) {
                    set(Cmd::OnCommit(|_, id| Dialog::checkout(id)));
                }
                if item(ui, "New branch here…", keys::BRANCH_KEY.into(), true) {
                    set(Cmd::OnCommit(|s, id| Dialog::branch(s, Some(id))));
                }
                if item(ui, "New tag here…", keys::TAG_KEY.into(), true) {
                    set(Cmd::OnCommit(|_, id| Dialog::tag(Some(id))));
                }
                if item(ui, "Reset current branch to here…", none(), true) {
                    set(Cmd::OnCommit(|_, id| Dialog::reset(id)));
                }
                if item(ui, "Revert…", none(), true) {
                    set(Cmd::OnCommits(|_, ids| Dialog::revert(ids)));
                }
                if item(ui, "Cherry-pick…", none(), true) {
                    set(Cmd::OnCommits(|_, ids| {
                        Dialog::cherry_pick(ids.into_iter().rev().collect())
                    }));
                }
                if item(ui, "Merge into current…", none(), true) {
                    set(Cmd::OnCommit(|s, id| {
                        Dialog::merge(s, Some(palette::commit_rev(s, &id)))
                    }));
                }
                if item(ui, "Rebase current onto this…", none(), true) {
                    set(Cmd::OnCommit(|s, id| {
                        Dialog::rebase(s, Some(palette::commit_rev(s, &id)))
                    }));
                }
                if item(ui, "Interactive rebase children…", none(), true) {
                    set(Cmd::OnCommit(Dialog::interactive_rebase));
                }
                if item(ui, "Create patch…", none(), true) {
                    set(Cmd::OnCommits(|_, ids| {
                        Dialog::create_patch(ids.into_iter().rev().collect())
                    }));
                }
                if item(ui, "Archive…", none(), true) {
                    set(Cmd::OnCommit(|_, id| Dialog::archive(id)));
                }
                if item(ui, "Add/Edit note…", none(), true) {
                    set(Cmd::OnCommit(|_, id| Dialog::edit_note(id)));
                }
                ui.separator();
                if item(ui, "Blame selected file", none(), true) {
                    set(Cmd::BlameSelected);
                }
            });
        });

        top(ui, &mut bar, "Tools", |ui| {
            if item(ui, "Options…", k(Act::Settings), true) {
                set(Cmd::Global(GlobalDialog::settings));
            }
            if item(ui, "Authentication…", none(), repo) {
                set(Cmd::Dialog(Dialog::authentication));
            }
            ui.separator();
            if item(ui, "Git console…", none(), repo) {
                set(Cmd::Dialog(|_| Dialog::console()));
            }
            if item(ui, "Open terminal", none(), repo) {
                set(Cmd::Terminal);
            }
            if item(ui, "Edit global git config…", none(), true) {
                set(Cmd::Global(GlobalDialog::edit_global_config));
            }
        });

        top(ui, &mut bar, "Help", |ui| {
            if item(ui, "Command palette…", k(Act::Palette), true) {
                out.toggle_palette = true;
            }
            let (label, enabled, action) = &update;
            if item(ui, label, none(), *enabled) {
                out.update = Some(*action);
            }
            ui.separator();
            if item(ui, "About GitKwaken", none(), true) {
                set(Cmd::Global(GlobalDialog::about));
            }
        });
        if with_caption {
            caption(ui);
        }
    });
    switch_on_hover(ui.ctx(), &bar);

    if let Some(c) = cmd {
        let snap = tab.as_ref().map(|t| t.session.snapshot());
        let ctx = ui.ctx().clone();
        palette::execute(&ctx, c, tab, snap, shared);
    }
    out
}
