//! Repository maintenance dialogs: sparse checkout, clean, optimization and
//! adding submodules.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Color32, Context, RichText};
use gk_core::Snapshot;
use gk_core::advanced::Optimization;

const GOOD: Color32 = Color32::from_rgb(0x3F, 0xA3, 0x4D);

// ---------------------------------------------------------------- sparse checkout

/// Cone directories from the editor: one per line, normalized to
/// "dir/sub" (forward slashes, no leading "./" or "/", no trailing "/").
pub fn parse_sparse_dirs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let mut d = l.trim().replace('\\', "/");
        if d.is_empty() || d.starts_with('#') {
            continue;
        }
        while let Some(r) = d.strip_prefix("./").or_else(|| d.strip_prefix('/')) {
            d = r.to_string();
        }
        let d = d.trim_end_matches('/').to_string();
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// Non-cone (gitignore-style) patterns cannot be expressed as directories.
fn non_cone(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains(['*', '!', '?', '[']))
}

pub struct Sparse {
    task: Option<Task<Result<Option<Vec<String>>, String>>>,
    current: Option<Result<Option<Vec<String>>, String>>,
    text: String,
}

impl Sparse {
    pub fn new(_snap: &Snapshot) -> Self {
        Self {
            task: None,
            current: None,
            text: String::new(),
        }
    }
}

impl TabDialog for Sparse {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.current.is_none() && self.task.is_none() {
            let repo = tab.session.repo().clone();
            self.task = Some(Task::spawn(ctx, move || {
                repo.sparse_list().map_err(|e| format!("{e:#}"))
            }));
        }
        if let Some(t) = &mut self.task
            && let Some(r) = t.take()
        {
            self.task = None;
            if let Ok(Some(dirs)) = &r {
                self.text = dirs.join("\n");
            }
            self.current = Some(r);
        }
        let mut disable = false;
        let (out, ()) = modal(
            ctx,
            "sparse",
            "Sparse checkout",
            560.0,
            "Apply",
            |ui, ctl| {
                let enabled = match &self.current {
                    None => {
                        ui.spinner();
                        ctl.invalid();
                        return;
                    }
                    Some(Err(e)) => {
                        error(ui, e.clone());
                        ctl.invalid();
                        return;
                    }
                    Some(Ok(c)) => c.clone(),
                };
                match &enabled {
                    Some(_) => {
                        ui.label(
                            RichText::new("● Sparse checkout is enabled")
                                .color(GOOD)
                                .strong(),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new(
                                "○ Sparse checkout is disabled: the whole tree is checked out",
                            )
                            .strong(),
                        );
                    }
                }
                ui.label("Only these directories (and every file at the top level) are checked out. One directory per line:");
                let r = ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("src/app\ndocs")
                        .desired_rows(8)
                        .desired_width(f32::INFINITY),
                );
                ctl.multiline(&r);
                ctl.focus(&r);
                let dirs = parse_sparse_dirs(&self.text);
                if non_cone(&dirs) {
                    warning(
                        ui,
                        "Wildcards are not supported: cone mode takes plain directory paths.",
                    );
                    ctl.invalid();
                }
                if let Some(cur) = &enabled
                    && non_cone(cur)
                {
                    hint(
                        ui,
                        "The current patterns use non-cone mode; applying switches to cone mode.",
                    );
                }
                ui.add_space(4.0);
                warning(
                    ui,
                    "Files outside these directories are removed from the working copy (they stay in the repository and come back when you widen the list or disable sparse checkout).",
                );
                if worktree_dirty(snap) {
                    hint(
                        ui,
                        "Modified files outside the list are kept until you commit or discard them.",
                    );
                }
                if enabled.is_some() {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Disable sparse checkout")
                            .on_hover_text("Check out the full tree again")
                            .clicked()
                        {
                            disable = true;
                        }
                    });
                } else {
                    ctl.ok_label = "Enable".into();
                }
            },
        );
        if disable {
            tab.session.run_op("Disable sparse checkout", |repo, _, _| {
                repo.sparse_disable().map(|_| String::new())
            });
            return false;
        }
        finish(out, || {
            let dirs = parse_sparse_dirs(&self.text);
            tab.session.run_op("Sparse checkout", move |repo, _, _| {
                repo.sparse_set(&dirs).map(|_| String::new())
            });
        })
    }
}

// ---------------------------------------------------------------- clean

type Preview = Result<Vec<String>, String>;

pub struct Clean {
    ignored: bool,
    dirs: bool,
    task: Option<Task<Preview>>,
    loaded: Option<(bool, bool)>,
    items: Option<Result<Vec<(String, bool)>, String>>,
    confirm: bool,
}

impl Clean {
    pub fn new() -> Self {
        Self {
            ignored: false,
            dirs: true,
            task: None,
            loaded: None,
            items: None,
            confirm: false,
        }
    }
}

/// Selected paths of the preview list.
pub fn selected_paths(items: &[(String, bool)]) -> Vec<String> {
    items
        .iter()
        .filter(|(_, s)| *s)
        .map(|(p, _)| p.clone())
        .collect()
}

impl TabDialog for Clean {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let want = (self.ignored, self.dirs);
        // Reload when the options change.
        let key_changed = self.loaded != Some(want);
        if (key_changed || self.items.is_none()) && self.task.is_none() && !self.confirm {
            let repo = tab.session.repo().clone();
            let (ignored, dirs) = want;
            self.loaded = Some(want);
            self.task = Some(Task::spawn(ctx, move || {
                repo.clean_preview(ignored, dirs)
                    .map_err(|e| format!("{e:#}"))
            }));
        }
        if let Some(t) = &mut self.task
            && let Some(r) = t.take()
        {
            self.task = None;
            self.items = Some(r.map(|v| v.into_iter().map(|p| (p, true)).collect()));
        }
        let (out, ()) = modal(
            ctx,
            "clean",
            "Clean working copy",
            600.0,
            "Delete selected",
            |ui, ctl| {
                ctl.danger = true;
                if self.confirm {
                    let n = self
                        .items
                        .as_ref()
                        .and_then(|r| r.as_ref().ok())
                        .map(|v| selected_paths(v).len())
                        .unwrap_or(0);
                    ui.label(
                        RichText::new(format!("Permanently delete {n} untracked item(s)?"))
                            .strong(),
                    );
                    warning(ui, "They are not in git: this cannot be undone.");
                    ctl.ok_label = format!("Delete {n} item(s)");
                    if ui.small_button("◀ Back").clicked() {
                        self.confirm = false;
                    }
                    return;
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.dirs, "Include untracked directories");
                    ui.checkbox(&mut self.ignored, "Include ignored files");
                });
                if self.ignored {
                    warning(
                        ui,
                        "Ignored files include build outputs, local settings and secrets such as .env files.",
                    );
                }
                ui.separator();
                match &mut self.items {
                    _ if self.task.is_some() => {
                        ui.spinner();
                        ctl.invalid();
                    }
                    None => ctl.invalid(),
                    Some(Err(e)) => {
                        error(ui, e.clone());
                        ctl.invalid();
                    }
                    Some(Ok(items)) if items.is_empty() => {
                        ui.label(RichText::new("Nothing to clean.").weak());
                        ctl.invalid();
                    }
                    Some(Ok(items)) => {
                        let n = items.iter().filter(|(_, s)| *s).count();
                        let mut all = n == items.len();
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut all, "Select all").changed() {
                                items.iter_mut().for_each(|(_, s)| *s = all);
                            }
                            hint(ui, format!("{n} of {} selected", items.len()));
                        });
                        egui::Frame::new()
                            .fill(ui.visuals().extreme_bg_color)
                            .inner_margin(6)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(300.0)
                                    .auto_shrink([false, true])
                                    .show_rows(
                                        ui,
                                        ui.spacing().interact_size.y,
                                        items.len(),
                                        |ui, range| {
                                            for (path, sel) in &mut items[range] {
                                                ui.checkbox(
                                                    sel,
                                                    RichText::new(path.as_str()).monospace(),
                                                );
                                            }
                                        },
                                    );
                            });
                        if n == 0 {
                            ctl.invalid();
                        }
                    }
                }
            },
        );
        match out {
            Outcome::None => true,
            Outcome::Cancel => false,
            Outcome::Ok if !self.confirm => {
                self.confirm = true;
                true
            }
            Outcome::Ok => {
                let paths = self
                    .items
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map(|v| selected_paths(v))
                    .unwrap_or_default();
                let label = format!("Clean {} item(s)", paths.len());
                tab.session.run_op(label, move |repo, _, _| {
                    repo.clean(&paths).map(|_| String::new())
                });
                false
            }
        }
    }
}

// ---------------------------------------------------------------- optimize

/// (enabled, title, explanation) rows for the optimization checklist.
pub fn optimization_rows(o: &Optimization) -> [(bool, &'static str, &'static str); 5] {
    [
        (
            o.fsmonitor,
            "File system monitor (core.fsmonitor)",
            "A background daemon reports changed files, so status no longer scans the whole working tree: status time follows the number of changes, not the number of files.",
        ),
        (
            o.untracked_cache,
            "Untracked cache (core.untrackedCache)",
            "Remembers directory timestamps so the search for untracked files skips unchanged directories.",
        ),
        (
            o.many_files,
            "Many-files mode (feature.manyFiles)",
            "Index version 4 and skip-hash: a smaller index that is faster to read and write in large repositories.",
        ),
        (
            o.commit_graph,
            "Commit graph with Bloom filters",
            "Precomputed history: much faster graph, log, blame and file history (log -- path).",
        ),
        (
            o.maintenance,
            "Background maintenance (git maintenance)",
            "Hourly prefetch, commit-graph updates and pack compaction keep the repository fast over time.",
        ),
    ]
}

pub struct Optimize {
    task: Option<Task<Optimization>>,
    status: Option<Optimization>,
    pending: Option<TrackedOp<()>>,
    result: Option<Result<(), String>>,
    /// Diagnostics: object write speed measurement.
    diag: Option<Task<Result<super::diagnostics::WriteSpeed, String>>>,
    diag_result: Option<Result<super::diagnostics::WriteSpeed, String>>,
    /// fsmonitor daemon state and Start / Stop.
    fsmon: super::fsmonitor::DaemonPanel,
}

impl Optimize {
    pub fn new(_snap: &Snapshot) -> Self {
        Self {
            task: None,
            status: None,
            pending: None,
            result: None,
            diag: None,
            diag_result: None,
            fsmon: Default::default(),
        }
    }

    fn load(&mut self, ctx: &Context, tab: &RepoTab) {
        let repo = tab.session.repo().clone();
        self.task = Some(Task::spawn(ctx, move || repo.optimization_status()));
    }
}

impl TabDialog for Optimize {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        if self.status.is_none() && self.task.is_none() {
            self.load(ctx, tab);
        }
        if let Some(t) = &mut self.diag
            && let Some(r) = t.take()
        {
            self.diag = None;
            self.diag_result = Some(r);
        }
        let workdir = tab.path.clone();
        let git_dir = tab.session.repo().git_dir().to_path_buf();
        let log = shared.log.clone();
        if let Some(t) = &mut self.task
            && let Some(s) = t.take()
        {
            self.task = None;
            self.status = Some(s);
        }
        if let Some(op) = &mut self.pending
            && let Some(r) = op.take()
        {
            self.pending = None;
            self.result = Some(r);
            self.load(ctx, tab);
        }
        let busy = self.pending.is_some();
        let (out, ()) = modal(
            ctx,
            "optimize",
            "Optimize repository",
            600.0,
            "Optimize now",
            |ui, ctl| {
                ctl.cancel_label = "Close";
                let Some(st) = &self.status else {
                    ui.spinner();
                    ctl.invalid();
                    return;
                };
                ui.label("Settings that make git (and GitKwaken) fast in large repositories:");
                ui.add_space(4.0);
                for (on, title, text) in optimization_rows(st) {
                    ui.horizontal_top(|ui| {
                        let (icon, color) = if on {
                            ("✔", GOOD)
                        } else {
                            ("✖", ui.visuals().weak_text_color())
                        };
                        ui.label(RichText::new(icon).color(color).strong().size(15.0));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(title).strong());
                            hint(ui, text);
                        });
                    });
                    ui.add_space(2.0);
                }
                let all = optimization_rows(st).iter().all(|r| r.0);
                ui.add_space(4.0);
                if all {
                    ui.label(RichText::new("✔ This repository is fully optimized.").color(GOOD));
                } else {
                    hint(
                        ui,
                        "\"Optimize now\" enables all of the above for this repository and writes the commit graph and multi-pack index (can take a while on big repositories).",
                    );
                }
                if cfg!(not(any(windows, target_os = "macos"))) && !st.fsmonitor {
                    hint(
                        ui,
                        "The built-in file system monitor is only available on Windows and macOS.",
                    );
                }
                self.fsmon
                    .ui(ui, ctx, tab.session.repo().git(), st.fsmonitor);
                if busy {
                    ctl.invalid();
                    op_progress(ui, snap);
                }
                match &self.result {
                    Some(Ok(())) => {
                        ui.label(RichText::new("Optimization finished.").color(GOOD));
                    }
                    Some(Err(e)) => error(ui, e.clone()),
                    None => {}
                }
                if diagnostics_ui(ui, &self.diag, &self.diag_result, &workdir) {
                    let (w, g, log) = (workdir.clone(), git_dir.clone(), log.clone());
                    self.diag_result = None;
                    self.diag = Some(Task::spawn(ctx, move || {
                        super::diagnostics::measure(&w, &g, &log)
                    }));
                }
            },
        );
        match out {
            Outcome::None => true,
            Outcome::Cancel => false,
            Outcome::Ok => {
                self.result = None;
                self.pending = Some(TrackedOp::queue(
                    tab,
                    "Optimize repository",
                    |repo, cancel, p| repo.optimize(cancel, p),
                ));
                true
            }
        }
    }
}

/// Optimize › Diagnostics. Returns true when a measurement should start.
fn diagnostics_ui(
    ui: &mut egui::Ui,
    running: &Option<Task<Result<super::diagnostics::WriteSpeed, String>>>,
    result: &Option<Result<super::diagnostics::WriteSpeed, String>>,
    workdir: &std::path::Path,
) -> bool {
    use super::diagnostics as diag;
    let mut start = false;
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new("Diagnostics").strong());
    ui.horizontal(|ui| {
        let b = ui
            .add_enabled(
                running.is_none(),
                egui::Button::new("Measure object write speed"),
            )
            .on_hover_text(format!(
                "Writes {} small objects into a throw-away repository on this volume",
                diag::OBJECTS
            ));
        if b.clicked() {
            start = true;
        }
        if running.is_some() {
            ui.spinner();
            ui.label(RichText::new("Measuring…").weak());
        }
    });
    match result {
        Some(Ok(s)) => {
            let line = format!(
                "{:.2} ms per object ({} objects in {:.0} ms){}",
                s.per_object_ms,
                diag::OBJECTS,
                s.total_ms,
                match s.dev_drive {
                    Some(true) => " · Dev Drive",
                    Some(false) => " · not a Dev Drive",
                    None => "",
                }
            );
            if s.slow() {
                warning(ui, format!("Slow: {line}"));
                ui.label(
                    "Writing objects should take well under 2 ms each. On Windows the usual cause is \
                     real-time antivirus scanning (Microsoft Defender) of every file git creates.",
                );
                hint(
                    ui,
                    "Exclude the repository and git from scanning (PowerShell as administrator):",
                );
                let cmds = diag::defender_commands(workdir, &gk_cli::find_git());
                command_box(ui, &cmds);
                hint(
                    ui,
                    "Or keep repositories on a Dev Drive (ReFS; Defender scans it in performance mode):",
                );
                command_box(ui, &diag::dev_drive_hint(workdir));
                hint(ui, "GitKwaken never runs these commands itself.");
            } else {
                ui.label(RichText::new(format!("✔ {line}")).color(GOOD));
            }
        }
        Some(Err(e)) => error(ui, e.clone()),
        None => {}
    }
    start
}

/// Read-only monospace command text with a copy button.
fn command_box(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(4)
            .show(ui, |ui| {
                ui.label(RichText::new(text).monospace().small());
            });
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(text.to_string());
        }
    });
}

// ---------------------------------------------------------------- submodules

#[derive(Default)]
pub struct SubmoduleAdd {
    url: String,
    path: String,
    /// The path was typed by the user: stop deriving it from the URL.
    path_edited: bool,
}

impl TabDialog for SubmoduleAdd {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        let root = tab.path.clone();
        let (out, ()) = modal(
            ctx,
            "submodule-add",
            "Add submodule",
            560.0,
            "Add",
            |ui, ctl| {
                egui::Grid::new("submodule-add-grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Repository URL:");
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.url)
                                .hint_text("https://host/user/lib.git")
                                .desired_width(400.0),
                        );
                        ctl.focus(&r);
                        if r.changed() && !self.path_edited {
                            self.path = folder_from_url(&self.url);
                        }
                        ui.end_row();
                        ui.label("Path:");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.path).desired_width(310.0),
                                )
                                .changed()
                            {
                                self.path_edited = true;
                            }
                            if ui.button("Browse…").clicked()
                                && let Some(p) =
                                    rfd::FileDialog::new().set_directory(&root).pick_folder()
                                && let Ok(rel) = p.strip_prefix(&root)
                            {
                                self.path = rel.to_string_lossy().replace('\\', "/");
                                self.path_edited = true;
                            }
                        });
                        ui.end_row();
                    });
                hint(
                    ui,
                    "Clones the repository into the path and records it in .gitmodules; commit both afterwards.",
                );
                let url = self.url.trim();
                let path = self.path.trim();
                if url.is_empty() || path.is_empty() {
                    ctl.invalid();
                    return;
                }
                let abs = root.join(path);
                let non_empty_dir = abs.is_dir()
                    && std::fs::read_dir(&abs)
                        .map(|mut d| d.next().is_some())
                        .unwrap_or(false);
                if std::path::Path::new(path).is_absolute()
                    || path.split(['/', '\\']).any(|c| c == "..")
                {
                    error(ui, "The path must be inside the repository.");
                    ctl.invalid();
                } else if abs.is_file() || non_empty_dir {
                    error(ui, format!("'{path}' already exists."));
                    ctl.invalid();
                }
            },
        );
        finish(out, || {
            let url = self.url.trim().to_string();
            let path = self.path.trim().replace('\\', "/");
            tab.session
                .run_op(format!("Add submodule {path}"), move |repo, cancel, p| {
                    repo.submodule_add(&url, &path, cancel, p)?;
                    Ok(String::new())
                });
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_dirs() {
        let text = "src/app\n  ./docs/ \n\n# comment\n/tools\\build\nsrc/app\n/\n";
        assert_eq!(parse_sparse_dirs(text), ["src/app", "docs", "tools/build"]);
        assert!(non_cone(&["src/*".to_string()]));
        assert!(!non_cone(&["src".to_string()]));
    }

    #[test]
    fn clean_selection() {
        let items = vec![
            ("a".to_string(), true),
            ("b/".to_string(), false),
            ("c".to_string(), true),
        ];
        assert_eq!(selected_paths(&items), ["a", "c"]);
    }

    #[test]
    fn optimization_checklist() {
        let o = Optimization {
            fsmonitor: true,
            commit_graph: true,
            ..Default::default()
        };
        let rows = optimization_rows(&o);
        assert_eq!(rows.iter().filter(|r| r.0).count(), 2);
        assert!(rows[0].0 && !rows[1].0 && rows[3].0);
    }
}
