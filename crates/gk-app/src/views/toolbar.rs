//! SourceTree-style toolbar: the main actions plus the view switcher.

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use gk_core::{RepoState, Snapshot};

/// Below this toolbar width the buttons drop their labels (icon + tooltip).
const COMPACT_BELOW: f32 = 1080.0;

/// A toolbar button: icon over label, or icon only when `compact`.
fn tool_button(compact: bool, icon: &str, label: &str) -> egui::Button<'static> {
    if compact {
        egui::Button::new(egui::RichText::new(icon).size(16.0)).min_size(egui::vec2(34.0, 30.0))
    } else {
        egui::Button::new(egui::RichText::new(format!("{icon}\n{label}")).size(12.0))
            .min_size(egui::vec2(54.0, 40.0))
    }
}

fn tool(ui: &mut egui::Ui, compact: bool, icon: &str, label: &str, hint: &str) -> bool {
    let hint = if compact {
        format!("{label}: {hint}")
    } else {
        hint.to_string()
    };
    ui.add(tool_button(compact, icon, label))
        .on_hover_text(hint)
        .clicked()
}

const BISECT_FILL: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x9A, 0x2A);

type MakeDialog = fn(&Snapshot) -> Dialog;

/// The "More" menu of less frequent tools; returns the dialog to open.
fn more_menu(ui: &mut egui::Ui, compact: bool, snap: &Snapshot) -> Option<Dialog> {
    let button = tool_button(compact, "⋯", "More");
    let (resp, inner) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        let items: [(&str, MakeDialog); 11] = [
            ("Bisect…", Dialog::bisect),
            ("LFS…", Dialog::lfs),
            ("Sparse checkout…", Dialog::sparse_checkout),
            ("Clean…", |_| Dialog::clean()),
            ("Optimize repository…", Dialog::optimize),
            ("Add submodule…", |_| Dialog::submodule_add()),
            ("Git console…", |_| Dialog::console()),
            ("Apply patch…", |_| Dialog::apply_patch()),
            ("Edit .gitignore…", |_| Dialog::edit_gitignore()),
            ("Edit repository config…", |_| Dialog::edit_repo_config()),
            ("Edit global git config…", |_| {
                Dialog::edit_global_config()
            }),
        ];
        let mut chosen = None;
        for (i, (label, make)) in items.iter().enumerate() {
            if matches!(i, 5 | 6 | 8) {
                ui.separator();
            }
            if ui.button(*label).clicked() {
                chosen = Some(make(snap));
                ui.close();
            }
        }
        chosen
    });
    resp.on_hover_text("More tools");
    inner.and_then(|r| r.inner)
}

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    egui::Panel::top(egui::Id::new(("toolbar", &tab.path))).show(ui, |ui| {
        ui.horizontal(|ui| {
            let compact = ui.available_width() < COMPACT_BELOW;
            let changes = snap.status.staged.len() + snap.status.unstaged.len();
            if tool(ui, compact, "✔", "Commit", "Commit staged changes") {
                tab.view = View::FileStatus;
            }
            if tool(ui, compact, "⬇", "Pull", "Pull from remote") {
                tab.dialog = Some(Dialog::pull(snap));
            }
            if tool(ui, compact, "⬆", "Push", "Push to remote") {
                tab.dialog = Some(Dialog::push(snap));
            }
            if tool(ui, compact, "⟳", "Fetch", "Fetch from all remotes") {
                tab.dialog = Some(Dialog::fetch(snap));
            }
            ui.separator();
            if tool(ui, compact, "⑂", "Branch", "Create or delete branches") {
                tab.dialog = Some(Dialog::branch(snap, None));
            }
            if tool(ui, compact, "⇄", "Merge", "Merge into the current branch") {
                tab.dialog = Some(Dialog::merge(snap, None));
            }
            if tool(ui, compact, "▣", "Stash", "Stash working copy changes") {
                tab.dialog = Some(Dialog::stash());
            }
            if tool(ui, compact, "✖", "Discard", "Discard working copy changes") && changes > 0 {
                tab.dialog = Some(Dialog::discard_all());
            }
            if tool(ui, compact, "🏷", "Tag", "Create a tag") {
                tab.dialog = Some(Dialog::tag(None));
            }
            if tool(
                ui,
                compact,
                "⚑",
                "Git Flow",
                "Git Flow: start or finish features, releases and hotfixes",
            ) {
                tab.dialog = Some(Dialog::git_flow(snap));
            }
            if snap.refs.state == RepoState::Bisecting {
                let (text, size) = if compact {
                    ("🔍", egui::vec2(34.0, 30.0))
                } else {
                    ("🔍\nBisect", egui::vec2(54.0, 40.0))
                };
                let b = egui::Button::new(
                    egui::RichText::new(text)
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::BLACK),
                )
                .fill(BISECT_FILL)
                .min_size(size);
                if ui
                    .add(b)
                    .on_hover_text("A bisect is in progress: mark commits good or bad")
                    .clicked()
                {
                    tab.dialog = Some(Dialog::bisect(snap));
                }
            }
            if let Some(d) = more_menu(ui, compact, snap) {
                tab.dialog = Some(d);
            }
            ui.separator();
            if tool(
                ui,
                compact,
                ">_",
                "Terminal",
                "Open a terminal in the repository",
            ) {
                open_terminal(&tab.path, shared);
            }
            if tool(ui, compact, "📂", "Explorer", "Show in Explorer") {
                let _ = open::that(&tab.path);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .selectable_label(tab.view == View::Search, "Search")
                    .clicked()
                {
                    tab.view = View::Search;
                }
                if ui
                    .selectable_label(tab.view == View::History, "History")
                    .clicked()
                {
                    tab.view = View::History;
                }
                let label = if changes > 0 {
                    format!("File Status ({changes})")
                } else {
                    "File Status".to_string()
                };
                if ui
                    .selectable_label(tab.view == View::FileStatus, label)
                    .clicked()
                {
                    tab.view = View::FileStatus;
                }
            });
        });
    });
}

pub fn open_terminal(path: &std::path::Path, shared: &Shared) {
    if let Some(t) = &shared.settings.terminal {
        let args = t.args.replace("$PWD", &path.display().to_string());
        let _ = std::process::Command::new(&t.command)
            .args(args.split_whitespace())
            .current_dir(path)
            .spawn();
        return;
    }
    #[cfg(windows)]
    {
        // Prefer Git Bash when available, else Windows Terminal / cmd.
        let git = gk_cli::find_git();
        let bash = git
            .parent()
            .and_then(|d| d.parent())
            .map(|d| d.join("git-bash.exe"));
        if let Some(b) = bash.filter(|b| b.is_file()) {
            let _ = std::process::Command::new(b)
                .arg(format!("--cd={}", path.display()))
                .spawn();
        } else {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "cmd"])
                .current_dir(path)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("x-terminal-emulator")
            .current_dir(path)
            .spawn();
    }
}
