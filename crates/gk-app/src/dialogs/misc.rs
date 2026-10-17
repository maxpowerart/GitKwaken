//! Patches, archives and the git console.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use std::path::PathBuf;

fn path_field(ui: &mut egui::Ui, path: &mut String, browse: impl FnOnce() -> Option<PathBuf>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(path).desired_width(ui.available_width() - 90.0));
        if ui.button("Browse…").clicked()
            && let Some(p) = browse()
        {
            *path = p.display().to_string();
        }
    });
}

// ---------------------------------------------------------------- patches

pub struct CreatePatch {
    ids: Vec<String>,
    path: String,
    sorted: bool,
    labels: RevLabels,
    /// Replace an existing file at `path`.
    overwrite: bool,
}

impl CreatePatch {
    pub fn new(ids: Vec<String>) -> Self {
        Self {
            ids,
            path: String::new(),
            sorted: false,
            labels: RevLabels::default(),
            overwrite: false,
        }
    }
}

impl TabDialog for CreatePatch {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if !self.sorted {
            // Oldest first so the patch series applies in order.
            let row = |id: &String| {
                gk_core::ObjectId::from_hex(id.as_bytes())
                    .ok()
                    .and_then(|o| snap.history.row(&o))
                    .unwrap_or(usize::MAX)
            };
            self.ids.sort_by_key(|id| std::cmp::Reverse(row(id)));
            self.sorted = true;
            if let Some(first) = self.ids.last() {
                let name = format!("{}.patch", crate::util::short_id(first));
                self.path = tab.path.join(name).display().to_string();
            }
        }
        let labels = self.labels.get(tab, snap, &self.ids).to_vec();
        let workdir = tab.path.clone();
        let (out, ()) = modal(
            ctx,
            "create-patch",
            "Create patch",
            560.0,
            "Save patch",
            |ui, ctl| {
                ui.label(format!("{} commit(s):", self.ids.len()));
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (id, l) in self.ids.iter().zip(&labels) {
                            ui.label(RichText::new(if l.is_empty() { id } else { l }).monospace());
                        }
                    });
                ui.label("Save to:");
                let current = self.path.clone();
                path_field(ui, &mut self.path, || {
                    let p = PathBuf::from(&current);
                    rfd::FileDialog::new()
                        .set_title("Save patch")
                        .set_directory(p.parent().map(|d| d.to_path_buf()).unwrap_or(workdir))
                        .set_file_name(
                            p.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        )
                        .add_filter("Patch", &["patch", "diff"])
                        .save_file()
                });
                if self.ids.is_empty() || self.path.trim().is_empty() {
                    ctl.invalid();
                } else if std::path::Path::new(self.path.trim()).exists() {
                    warning(ui, "A file with this name already exists.");
                    ui.checkbox(&mut self.overwrite, "Overwrite it");
                    if !self.overwrite {
                        ctl.invalid();
                    }
                }
            },
        );
        finish(out, || {
            let ids = self.ids.clone();
            let path = PathBuf::from(self.path.trim());
            tab.session.run_op("Create patch", move |repo, _, _| {
                let bytes = repo.format_patch(&ids)?;
                std::fs::write(&path, bytes)?;
                Ok(String::new())
            });
        })
    }
}

#[derive(Default)]
pub struct ApplyPatch {
    path: String,
    to_index: bool,
}

impl TabDialog for ApplyPatch {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let workdir = tab.path.clone();
        let (out, ()) = modal(
            ctx,
            "apply-patch",
            "Apply patch",
            540.0,
            "Apply",
            |ui, ctl| {
                ui.label("Patch file:");
                path_field(ui, &mut self.path, || {
                    rfd::FileDialog::new()
                        .set_title("Apply patch")
                        .set_directory(workdir)
                        .add_filter("Patch", &["patch", "diff"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                });
                ui.checkbox(
                    &mut self.to_index,
                    "Apply to the index only (staged changes)",
                );
                hint(
                    ui,
                    "Uses git apply --3way: conflicts are left for you to resolve.",
                );
                let p = self.path.trim();
                if p.is_empty() {
                    ctl.invalid();
                } else if !std::path::Path::new(p).is_file() {
                    error(ui, "File not found.");
                    ctl.invalid();
                }
            },
        );
        finish(out, || {
            let path = PathBuf::from(self.path.trim());
            let to_index = self.to_index;
            tab.session.run_op("Apply patch", move |repo, _, _| {
                repo.apply_patch(&path, to_index)?;
                Ok(String::new())
            });
        })
    }
}

// ---------------------------------------------------------------- archive

pub struct Archive {
    rev: String,
    path: String,
    label: RevLabel,
}

impl Archive {
    pub fn new(rev: String) -> Self {
        Self {
            rev,
            path: String::new(),
            label: RevLabel::default(),
        }
    }
}

impl TabDialog for Archive {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.path.is_empty() {
            let base = tab
                .path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| tab.path.clone());
            let name = format!(
                "{}-{}.zip",
                tab.name,
                crate::util::short_id(&self.rev).replace('/', "-")
            );
            self.path = base.join(name).display().to_string();
        }
        let label = self.label.get(tab, snap, &self.rev).to_string();
        let (out, ()) = modal(
            ctx,
            "archive",
            "Archive",
            560.0,
            "Create archive",
            |ui, ctl| {
                ui.horizontal(|ui| {
                    ui.label("Revision:");
                    ui.add(egui::TextEdit::singleline(&mut self.rev).desired_width(240.0));
                });
                if !label.is_empty() {
                    hint(ui, label);
                }
                ui.label("Save to (.zip, .tar, .tar.gz, .tgz):");
                let current = self.path.clone();
                path_field(ui, &mut self.path, || {
                    let p = PathBuf::from(&current);
                    let mut d = rfd::FileDialog::new()
                        .set_title("Save archive")
                        .add_filter("Zip", &["zip"])
                        .add_filter("Tar", &["tar"])
                        .add_filter("Tar.gz", &["tar.gz", "tgz"]);
                    if let Some(dir) = p.parent() {
                        d = d.set_directory(dir);
                    }
                    if let Some(n) = p.file_name() {
                        d = d.set_file_name(n.to_string_lossy());
                    }
                    d.save_file()
                });
                let lower = self.path.trim().to_lowercase();
                if self.rev.trim().is_empty() || lower.is_empty() {
                    ctl.invalid();
                } else if self.rev.trim().starts_with('-') {
                    error(ui, "A revision cannot start with '-'.");
                    ctl.invalid();
                } else if ![".zip", ".tar", ".tar.gz", ".tgz"]
                    .iter()
                    .any(|e| lower.ends_with(e))
                {
                    error(ui, "Unsupported extension: use .zip, .tar, .tar.gz or .tgz");
                    ctl.invalid();
                }
            },
        );
        finish(out, || {
            let rev = self.rev.trim().to_string();
            let path = PathBuf::from(self.path.trim());
            tab.session
                .run_op(format!("Archive {rev}"), move |repo, _, _| {
                    repo.archive(&rev, &path)?;
                    Ok(String::new())
                });
        })
    }
}

// ---------------------------------------------------------------- console

static HISTORY: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());
const MAX_OUTPUT: usize = 512 * 1024;

/// The git subcommand of an argument list, skipping global options.
fn subcommand(args: &[String]) -> Option<(usize, &str)> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(
            a,
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
        ) {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            return Some((i, a));
        }
    }
    None
}

/// Why a console command needs confirmation, when it can destroy work.
pub fn destructive_reason(args: &[String]) -> Option<&'static str> {
    let (i, verb) = subcommand(args)?;
    let rest: Vec<&str> = args[i + 1..].iter().map(String::as_str).collect();
    let has = |opts: &[&str]| rest.iter().any(|a| opts.contains(a));
    // Combined short flags such as -fd or -Df.
    let short = |c: char| {
        rest.iter()
            .any(|a| a.len() > 1 && a.starts_with('-') && !a.starts_with("--") && a.contains(c))
    };
    match verb {
        "reset" if has(&["--hard"]) => Some("reset --hard discards uncommitted changes"),
        "clean" if !has(&["-n", "--dry-run"]) && !short('n') => {
            Some("clean deletes untracked files")
        }
        "push"
            if has(&["--force", "--mirror", "--delete", "--prune"])
                || rest.iter().any(|a| {
                    a.starts_with("--force-with-lease")
                        || a.starts_with('+')
                        || (a.starts_with(':') && a.len() > 1)
                })
                || short('f')
                || short('d') =>
        {
            Some("this push can overwrite or delete remote history")
        }
        "branch"
            if short('D')
                || ((short('d') || has(&["--delete"])) && (short('f') || has(&["--force"]))) =>
        {
            Some("branch -D deletes a branch even when it is not merged")
        }
        "checkout" if has(&["--", ".", "--force"]) || short('f') => {
            Some("checkout can overwrite local changes")
        }
        "restore" if !has(&["--staged", "-S"]) || has(&["--worktree", "-W"]) => {
            Some("restore overwrites local changes")
        }
        "stash" if rest.first().is_some_and(|s| *s == "drop" || *s == "clear") => {
            Some("stash drop/clear deletes stashed changes")
        }
        "rm" if !has(&["--cached", "-n", "--dry-run"]) => {
            Some("rm deletes files from the working copy")
        }
        "reflog"
            if rest
                .first()
                .is_some_and(|s| *s == "expire" || *s == "delete") =>
        {
            Some("this removes reflog entries")
        }
        "update-ref" if short('d') => Some("update-ref -d deletes a ref"),
        "filter-branch" => Some("filter-branch rewrites history"),
        "gc" if rest.iter().any(|a| a.starts_with("--prune")) => {
            Some("gc --prune deletes unreachable objects")
        }
        _ => None,
    }
}

pub struct Console {
    input: String,
    output: String,
    running: Option<TrackedOp<String>>,
    /// Position while browsing the history with Up/Down.
    hist_pos: Option<usize>,
    /// A destructive command line waiting for confirmation, with the reason.
    confirm: Option<(String, &'static str)>,
}

impl Console {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            output: String::new(),
            running: None,
            hist_pos: None,
            confirm: None,
        }
    }

    /// Runs the input line, asking for confirmation first when destructive.
    fn submit(&mut self, tab: &RepoTab) {
        let line = self.input.trim().to_string();
        if line.is_empty() {
            return;
        }
        if let Ok(args) = split_args(&line)
            && let Some(reason) = destructive_reason(&args)
        {
            self.confirm = Some((line, reason));
            return;
        }
        self.run(tab, line);
    }

    fn run(&mut self, tab: &RepoTab, line: String) {
        self.confirm = None;
        {
            let mut h = HISTORY.lock();
            h.retain(|l| l != &line);
            h.push(line.clone());
            let len = h.len();
            if len > 200 {
                h.drain(..len - 200);
            }
        }
        self.hist_pos = None;
        self.input.clear();
        self.output.push_str(&format!("$ git {line}\n"));
        let args = match split_args(&line) {
            Ok(a) => a,
            Err(e) => {
                self.output.push_str(&format!("error: {e}\n\n"));
                return;
            }
        };
        // Queued with the other operations so it never races them; the
        // session refreshes everything afterwards.
        let label = format!(
            "git {}",
            subcommand(&args).map(|(_, v)| v).unwrap_or_default()
        );
        self.running = Some(TrackedOp::queue(tab, label, move |repo, _, _| {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            // No editor or pager can be shown: make git fail fast instead of hanging.
            let git = repo
                .git()
                .env("GIT_EDITOR", "true")
                .env("GIT_SEQUENCE_EDITOR", "true");
            Ok(match git.run_raw(&a, None) {
                Ok(o) => {
                    let mut t = o.stdout_str();
                    t.push_str(&o.stderr_str());
                    if !o.success() {
                        t.push_str(&format!(
                            "[exit code {}]\n",
                            o.code.map_or("?".to_string(), |c| c.to_string())
                        ));
                    }
                    t
                }
                Err(e) => format!("error: {e:#}\n"),
            })
        }));
    }

    fn browse_history(&mut self, ctx: &Context, id: egui::Id, up: bool) {
        let h = HISTORY.lock();
        if h.is_empty() {
            return;
        }
        let pos = match (self.hist_pos, up) {
            (None, true) => Some(h.len() - 1),
            (None, false) => None,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) => (p + 1 < h.len()).then_some(p + 1),
        };
        self.hist_pos = pos;
        self.input = pos.map(|p| h[p].clone()).unwrap_or_default();
        if let Some(mut st) = egui::TextEdit::load_state(ctx, id) {
            let end = egui::text::CCursor::new(self.input.chars().count());
            st.cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            st.store(ctx, id);
        }
    }
}

impl TabDialog for Console {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if let Some(t) = &mut self.running
            && let Some(r) = t.take()
        {
            let text = r.unwrap_or_else(|e| format!("error: {e}\n"));
            self.output.push_str(&text);
            if !text.ends_with('\n') {
                self.output.push('\n');
            }
            self.output.push('\n');
            if self.output.len() > MAX_OUTPUT {
                let cut = self.output.len() - MAX_OUTPUT;
                let cut = (cut..self.output.len())
                    .find(|&i| self.output.is_char_boundary(i))
                    .unwrap_or(0);
                self.output.drain(..cut);
            }
            self.running = None;
        }
        let input_id = egui::Id::new("git-console-input");
        let mut submit = false;
        let mut hist: Option<bool> = None;
        let busy = self.running.is_some() || self.confirm.is_some();
        let mut confirmed: Option<bool> = None;
        let title = format!("Git console — {}", tab.name);
        let (out, ()) = modal(ctx, "console", &title, 760.0, "Run", |ui, ctl| {
            ctl.show_ok = false;
            ctl.enter_ok = false;
            ctl.cancel_label = "Close";
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(6)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .min_scrolled_height(360.0)
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if self.output.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "Type git arguments below, e.g.  log --oneline -10",
                                    )
                                    .weak(),
                                );
                            }
                            ui.add(
                                egui::Label::new(RichText::new(&self.output).monospace())
                                    .selectable(true)
                                    .wrap(),
                            );
                        });
                });
            ui.horizontal(|ui| {
                ui.label(RichText::new("git").monospace().strong());
                let r = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut self.input)
                        .id(input_id)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width() - 120.0),
                );
                if ctl.first {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if r.has_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        hist = Some(true);
                    } else if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        hist = Some(false);
                    }
                }
                if self.running.is_some() {
                    ui.spinner();
                } else if ui.add_enabled(!busy, egui::Button::new("Run")).clicked() {
                    submit = true;
                }
                if ui.button("Clear").clicked() {
                    self.output.clear();
                }
            });
            if let Some((line, reason)) = &self.confirm {
                warning(ui, format!("Careful: {reason}."));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("git {line}")).monospace());
                    let run = egui::Button::new(
                        RichText::new("Run anyway")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(DANGER);
                    if ui.add(run).clicked() {
                        confirmed = Some(true);
                    }
                    if ui.button("Don't run").clicked() {
                        confirmed = Some(false);
                    }
                });
            }
        });
        if let Some(up) = hist {
            self.browse_history(ctx, input_id, up);
        }
        match confirmed {
            Some(true) => {
                if let Some((line, _)) = self.confirm.take() {
                    self.run(tab, line);
                }
                ctx.memory_mut(|m| m.request_focus(input_id));
            }
            Some(false) => {
                self.confirm = None;
                ctx.memory_mut(|m| m.request_focus(input_id));
            }
            None => {}
        }
        if submit && !busy {
            self.submit(tab);
            ctx.memory_mut(|m| m.request_focus(input_id));
        }
        out == Outcome::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(line: &str) -> Option<&'static str> {
        destructive_reason(&split_args(line).unwrap())
    }

    #[test]
    fn destructive_commands() {
        for bad in [
            "reset --hard HEAD~1",
            "-c core.x=y reset --hard",
            "clean -fd",
            "push --force origin main",
            "push -f",
            "push --force-with-lease",
            "push origin +main",
            "push origin :old",
            "push --delete origin x",
            "branch -D topic",
            "branch -d -f topic",
            "checkout -- file.txt",
            "checkout .",
            "restore file.txt",
            "restore --staged --worktree f",
            "stash drop",
            "stash clear",
            "rm file",
        ] {
            assert!(reason(bad).is_some(), "{bad}");
        }
        for ok in [
            "log --oneline",
            "reset --soft HEAD~1",
            "clean -n",
            "clean -nd",
            "push origin main",
            "branch -d topic",
            "checkout main",
            "restore --staged f",
            "stash list",
            "rm --cached f",
            "status",
            "",
        ] {
            assert!(reason(ok).is_none(), "{ok}");
        }
    }
}
