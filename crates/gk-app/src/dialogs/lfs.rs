//! Git LFS: tracked patterns, file locks and fetching LFS objects.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;

/// First token of a .gitattributes line, honoring a leading "quoted pattern".
fn split_pattern(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line.strip_prefix('"') {
        let mut pat = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Some((pat, &rest[i + 1..])),
                '\\' => pat.push(chars.next()?.1),
                c => pat.push(c),
            }
        }
        return None;
    }
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    Some((line[..end].to_string(), &line[end..]))
}

/// Patterns of a .gitattributes file stored in LFS (`filter=lfs`).
pub fn parse_lfs_patterns(gitattributes: &str) -> Vec<String> {
    gitattributes
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (pat, attrs) = split_pattern(l)?;
            attrs
                .split_whitespace()
                .any(|a| a == "filter=lfs")
                .then_some(pat)
        })
        .collect()
}

struct Info {
    available: bool,
    patterns: Vec<String>,
}

type Locks = Result<Vec<(String, String)>, String>;

pub struct Lfs {
    info_task: Option<Task<Info>>,
    info: Option<Info>,
    locks_task: Option<Task<Locks>>,
    locks: Option<Locks>,
    pattern: String,
    lock_path: String,
    force: bool,
    pending: Option<(String, TrackedOp<()>)>,
    message: Option<Result<String, String>>,
}

impl Lfs {
    pub fn new(_snap: &Snapshot) -> Self {
        Self {
            info_task: None,
            info: None,
            locks_task: None,
            locks: None,
            pattern: String::new(),
            lock_path: String::new(),
            force: false,
            pending: None,
            message: None,
        }
    }

    fn load_info(&mut self, ctx: &Context, tab: &RepoTab) {
        let repo = tab.session.repo().clone();
        self.info_task = Some(Task::spawn(ctx, move || {
            let available = repo.lfs_available();
            let text = std::fs::read_to_string(repo.abs(".gitattributes")).unwrap_or_default();
            Info {
                available,
                patterns: parse_lfs_patterns(&text),
            }
        }));
    }

    fn load_locks(&mut self, ctx: &Context, tab: &RepoTab) {
        let repo = tab.session.repo().clone();
        self.locks_task = Some(Task::spawn(ctx, move || {
            repo.lfs_locks().map_err(|e| format!("{e:#}"))
        }));
    }

    fn queue(
        &mut self,
        tab: &RepoTab,
        label: String,
        f: impl FnOnce(&gk_core::Repo, &gk_cli::CancelToken, OpRun) -> anyhow::Result<()>
        + Send
        + 'static,
    ) {
        self.message = None;
        self.pending = Some((label.clone(), TrackedOp::queue(tab, label, f)));
    }

    fn git_op(&mut self, tab: &RepoTab, label: String, args: Vec<String>) {
        self.queue(tab, label, move |repo, _, _| {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            repo.git().run(&a)?;
            Ok(())
        });
    }

    fn patterns_ui(&mut self, ui: &mut egui::Ui, tab: &RepoTab, busy: bool) {
        ui.label(RichText::new("Tracked patterns").strong());
        let patterns = self
            .info
            .as_ref()
            .map(|i| i.patterns.clone())
            .unwrap_or_default();
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(6)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("lfs-patterns")
                    .max_height(120.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if patterns.is_empty() {
                            ui.label(
                                RichText::new("No patterns in .gitattributes use LFS.").weak(),
                            );
                        }
                        for p in &patterns {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(p).monospace());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new("Untrack").small(),
                                            )
                                            .clicked()
                                        {
                                            self.git_op(
                                                tab,
                                                format!("LFS untrack {p}"),
                                                vec!["lfs".into(), "untrack".into(), p.clone()],
                                            );
                                        }
                                    },
                                );
                            });
                        }
                    });
            });
        ui.horizontal(|ui| {
            ui.label("Track pattern:");
            ui.add(
                egui::TextEdit::singleline(&mut self.pattern)
                    .hint_text("*.psd")
                    .desired_width(220.0),
            );
            let p = self.pattern.trim().to_string();
            if ui
                .add_enabled(!busy && !p.is_empty(), egui::Button::new("Track"))
                .clicked()
            {
                self.queue(tab, format!("LFS track {p}"), move |repo, _, _| {
                    repo.lfs_track(&p)
                });
                self.pattern.clear();
            }
        });
        hint(
            ui,
            "Tracking edits .gitattributes; commit it so others use LFS for these files too.",
        );
    }

    fn locks_ui(&mut self, ui: &mut egui::Ui, ctx: &Context, tab: &RepoTab, busy: bool) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Locks").strong());
            if ui
                .add_enabled(self.locks_task.is_none(), egui::Button::new("⟳").small())
                .on_hover_text("Reload locks")
                .clicked()
            {
                self.load_locks(ctx, tab);
            }
        });
        let mut unlock = None;
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(6)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("lfs-locks")
                    .max_height(150.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match &self.locks {
                        _ if self.locks_task.is_some() => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Asking the LFS server…").weak());
                            });
                        }
                        None => {}
                        Some(Err(e)) => error(
                            ui,
                            e.lines()
                                .next()
                                .unwrap_or("Could not list locks")
                                .to_string(),
                        ),
                        Some(Ok(l)) if l.is_empty() => {
                            ui.label(RichText::new("No locked files.").weak());
                        }
                        Some(Ok(l)) => {
                            egui::Grid::new("lfs-lock-grid")
                                .num_columns(3)
                                .striped(true)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    for (path, owner) in l {
                                        ui.label(RichText::new(path).monospace());
                                        ui.label(owner);
                                        if ui
                                            .add_enabled(!busy, egui::Button::new("Unlock").small())
                                            .clicked()
                                        {
                                            unlock = Some(path.clone());
                                        }
                                        ui.end_row();
                                    }
                                });
                        }
                    });
            });
        ui.checkbox(&mut self.force, "Force unlock (locks owned by others)");
        if let Some(path) = unlock {
            let force = self.force;
            self.queue(tab, format!("LFS unlock {path}"), move |repo, _, _| {
                repo.lfs_lock(&path, true, force)
            });
        }
        ui.horizontal(|ui| {
            ui.label("Lock file:");
            ui.add(
                egui::TextEdit::singleline(&mut self.lock_path)
                    .hint_text("path/in/repo.psd")
                    .desired_width(220.0),
            );
            if ui.button("Browse…").clicked()
                && let Some(p) = rfd::FileDialog::new().set_directory(&tab.path).pick_file()
                && let Ok(rel) = p.strip_prefix(&tab.path)
            {
                self.lock_path = rel.to_string_lossy().replace('\\', "/");
            }
            let p = self.lock_path.trim().to_string();
            if ui
                .add_enabled(!busy && !p.is_empty(), egui::Button::new("Lock"))
                .clicked()
            {
                self.queue(tab, format!("LFS lock {p}"), move |repo, _, _| {
                    repo.lfs_lock(&p, false, false)
                });
                self.lock_path.clear();
            }
        });
    }
}

impl TabDialog for Lfs {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.info.is_none() && self.info_task.is_none() {
            self.load_info(ctx, tab);
        }
        if let Some(t) = &mut self.info_task
            && let Some(info) = t.take()
        {
            self.info_task = None;
            if info.available && self.locks.is_none() && self.locks_task.is_none() {
                self.load_locks(ctx, tab);
            }
            self.info = Some(info);
        }
        if let Some(t) = &mut self.locks_task
            && let Some(l) = t.take()
        {
            self.locks_task = None;
            self.locks = Some(l);
        }
        if let Some((label, op)) = &mut self.pending
            && let Some(r) = op.take()
        {
            self.message = Some(r.map(|()| format!("{label}: done")));
            self.pending = None;
            self.load_info(ctx, tab);
            if self.locks_task.is_none() {
                self.load_locks(ctx, tab);
            }
        }
        let busy = self.pending.is_some();
        let mut pull = false;
        let (out, ()) = modal(ctx, "lfs", "Git LFS", 600.0, "OK", |ui, ctl| {
            ctl.show_ok = false;
            ctl.enter_ok = false;
            ctl.cancel_label = "Close";
            let Some(info) = &self.info else {
                ui.spinner();
                return;
            };
            if !info.available {
                ui.label(RichText::new("Git LFS is not installed").strong());
                ui.label("Git Large File Storage keeps big binary files outside the repository history and downloads them on checkout.");
                ui.add_space(4.0);
                ui.label("To install it:");
                ui.label(
                    "• Git for Windows: re-run the installer and keep \"Git LFS\" selected, or",
                );
                ui.horizontal(|ui| {
                    ui.label("• download it from");
                    ui.hyperlink("https://git-lfs.com");
                });
                ui.label(
                    "• macOS: brew install git-lfs  —  Linux: your package manager (git-lfs).",
                );
                ui.label("Then run  git lfs install  once and reopen this dialog.");
                return;
            }
            self.patterns_ui(ui, tab, busy);
            ui.separator();
            self.locks_ui(ui, ctx, tab, busy);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("⬇ Pull LFS objects"))
                    .clicked()
                {
                    pull = true;
                }
                hint(
                    ui,
                    "Downloads the LFS files of the current checkout (git lfs pull).",
                );
            });
            if busy {
                op_progress(ui, snap);
            }
            match &self.message {
                Some(Ok(m)) => {
                    ui.label(RichText::new(m).weak());
                }
                Some(Err(e)) => error(ui, e.clone()),
                None => {}
            }
        });
        if pull {
            self.queue(tab, "LFS pull".into(), |repo, cancel, p| {
                repo.lfs_pull(cancel, p)
            });
        }
        out == Outcome::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfs_patterns() {
        let text = "# comment\n*.psd filter=lfs diff=lfs merge=lfs -text\n*.txt text eol=lf\n\n  assets/** filter=lfs diff=lfs merge=lfs -text\n\"my file.bin\" filter=lfs -text\n*.zip filter=lfsx\n\"broken filter=lfs\n";
        assert_eq!(
            parse_lfs_patterns(text),
            ["*.psd", "assets/**", "my file.bin"]
        );
        assert!(parse_lfs_patterns("").is_empty());
    }
}
