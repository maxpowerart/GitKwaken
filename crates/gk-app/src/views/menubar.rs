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
}

pub fn show(ui: &mut Ui, mc: MenuCtx, shared: &mut Shared) -> MenuOut {
    let MenuCtx {
        km,
        tab,
        active,
        tabs,
        update,
    } = mc;
    let k = |a: Act| km.text(a);
    let none = String::new;
    let repo = tab.is_some();
    let mut out = MenuOut::default();
    let mut cmd: Option<Cmd> = None;
    let mut set = |c: Cmd| cmd = Some(c);
    let current = tab.as_ref().map(|t| t.path.clone());

    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
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

        ui.menu_button("Edit", |ui| {
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

        ui.menu_button("View", |ui| {
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

        ui.menu_button("Repository", |ui| {
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

        ui.menu_button("Actions", |ui| {
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

        ui.menu_button("Tools", |ui| {
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

        ui.menu_button("Help", |ui| {
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
    });

    if let Some(c) = cmd {
        let snap = tab.as_ref().map(|t| t.session.snapshot());
        let ctx = ui.ctx().clone();
        palette::execute(&ctx, c, tab, snap, shared);
    }
    out
}
