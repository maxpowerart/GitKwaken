//! SourceTree-style toolbar: the main actions plus the view switcher.

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use gk_core::Snapshot;

fn tool(ui: &mut egui::Ui, icon: &str, label: &str, hint: &str) -> bool {
    let b = egui::Button::new(egui::RichText::new(format!("{icon}\n{label}")).size(12.0)).min_size(egui::vec2(54.0, 40.0));
    ui.add(b).on_hover_text(hint).clicked()
}

pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    egui::Panel::top(egui::Id::new(("toolbar", &tab.path))).show(ui, |ui| {
        ui.horizontal(|ui| {
            let changes = snap.status.staged.len() + snap.status.unstaged.len();
            if tool(ui, "✔", "Commit", "Commit staged changes") {
                tab.view = View::FileStatus;
            }
            if tool(ui, "⬇", "Pull", "Pull from remote") {
                tab.dialog = Some(Dialog::pull(snap));
            }
            if tool(ui, "⬆", "Push", "Push to remote") {
                tab.dialog = Some(Dialog::push(snap));
            }
            if tool(ui, "⟳", "Fetch", "Fetch from all remotes") {
                tab.dialog = Some(Dialog::fetch(snap));
            }
            ui.separator();
            if tool(ui, "⑂", "Branch", "Create or delete branches") {
                tab.dialog = Some(Dialog::branch(snap, None));
            }
            if tool(ui, "⇄", "Merge", "Merge into the current branch") {
                tab.dialog = Some(Dialog::merge(snap, None));
            }
            if tool(ui, "▣", "Stash", "Stash working copy changes") {
                tab.dialog = Some(Dialog::stash());
            }
            if tool(ui, "✖", "Discard", "Discard working copy changes") && changes > 0 {
                tab.dialog = Some(Dialog::discard_all());
            }
            if tool(ui, "🏷", "Tag", "Create a tag") {
                tab.dialog = Some(Dialog::tag(None));
            }
            ui.separator();
            if tool(ui, ">_", "Terminal", "Open a terminal in the repository") {
                open_terminal(&tab.path, shared);
            }
            if tool(ui, "📂", "Explorer", "Show in Explorer") {
                let _ = open::that(&tab.path);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.selectable_label(tab.view == View::Search, "Search").clicked() {
                    tab.view = View::Search;
                }
                if ui.selectable_label(tab.view == View::History, "History").clicked() {
                    tab.view = View::History;
                }
                let label = if changes > 0 { format!("File Status ({changes})") } else { "File Status".to_string() };
                if ui.selectable_label(tab.view == View::FileStatus, label).clicked() {
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
        let bash = git.parent().and_then(|d| d.parent()).map(|d| d.join("git-bash.exe"));
        if let Some(b) = bash.filter(|b| b.is_file()) {
            let _ = std::process::Command::new(b).arg(format!("--cd={}", path.display())).spawn();
        } else {
            let _ = std::process::Command::new("cmd").args(["/C", "start", "cmd"]).current_dir(path).spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("x-terminal-emulator").current_dir(path).spawn();
    }
}
