//! Application dialogs: clone, init, bookmark, about.

use super::AppDialog;
use super::common::*;
use crate::app::{Action, Shared};
use egui::{Context, RichText};
use gk_cli::{CancelToken, Line};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn default_parent(shared: &Shared) -> String {
    shared
        .settings
        .recent_repos
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from))
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn folder_picker(ui: &mut egui::Ui, value: &mut String, title: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width() - 90.0));
        if ui.button("Browse…").clicked() {
            let mut d = rfd::FileDialog::new().set_title(title);
            if Path::new(value.as_str()).is_dir() {
                d = d.set_directory(value.as_str());
            }
            if let Some(p) = d.pick_folder() {
                *value = p.display().to_string();
            }
        }
    });
}

fn dir_is_empty(p: &Path) -> bool {
    std::fs::read_dir(p).map(|mut d| d.next().is_none()).unwrap_or(false)
}

// ---------------------------------------------------------------- clone

struct CloneRun {
    cancel: CancelToken,
    lines: Arc<Mutex<Vec<String>>>,
    task: Task<Result<(), String>>,
    dest: PathBuf,
}

pub struct CloneRepo {
    url: String,
    parent: Option<String>,
    name: String,
    /// Folder name typed by the user (stops deriving it from the URL).
    name_edited: bool,
    shallow: bool,
    depth: u32,
    partial: bool,
    submodules: bool,
    branch: String,
    run: Option<CloneRun>,
    error: Option<String>,
}

impl CloneRepo {
    pub fn new() -> Self {
        Self {
            url: String::new(),
            parent: None,
            name: String::new(),
            name_edited: false,
            shallow: false,
            depth: 1,
            partial: false,
            submodules: true,
            branch: String::new(),
            run: None,
            error: None,
        }
    }

    fn start(&mut self, ctx: &Context, shared: &Shared) {
        let parent = PathBuf::from(self.parent.clone().unwrap_or_default().trim());
        let dest = parent.join(self.name.trim());
        let mut args: Vec<String> = vec!["clone".into(), "--progress".into()];
        if self.shallow {
            args.push(format!("--depth={}", self.depth.max(1)));
        }
        if self.partial {
            args.push("--filter=blob:none".into());
        }
        if self.submodules {
            args.push("--recurse-submodules".into());
        }
        if !self.branch.trim().is_empty() {
            args.push("--branch".into());
            args.push(self.branch.trim().into());
        }
        args.push("--".into());
        args.push(self.url.trim().into());
        args.push(dest.display().to_string());
        let mut git = gk_cli::Git::new(&parent, shared.log.clone());
        for (k, v) in askpass_env() {
            git = git.env(k, v);
        }
        let cancel = CancelToken::new();
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let (c2, l2, ctx2) = (cancel.clone(), lines.clone(), ctx.clone());
        let task = Task::spawn(ctx, move || {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let on_line = |l: Line| {
                let (Line::Stdout(s) | Line::Stderr(s)) = l;
                let mut v = l2.lock();
                // Progress lines ("Receiving objects:  42% ...") replace their predecessor.
                let key = |t: &str| t.split_once(':').map(|(k, _)| k.to_string());
                if let (Some(last), Some(k)) = (v.last(), key(&s))
                    && key(last).as_ref() == Some(&k)
                    && s.contains('%')
                {
                    v.pop();
                }
                v.push(s);
                if v.len() > 500 {
                    v.remove(0);
                }
                ctx2.request_repaint();
            };
            match git.run_streaming(&a, None, &c2, on_line) {
                Ok(o) => o.check().map(|_| ()).map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("{e:#}")),
            }
        });
        self.error = None;
        self.run = Some(CloneRun { cancel, lines, task, dest });
    }
}

impl AppDialog for CloneRepo {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        let parent = self.parent.get_or_insert_with(|| default_parent(shared));
        if let Some(run) = &mut self.run {
            match run.task.take() {
                Some(Ok(())) => {
                    shared.actions.push(Action::OpenRepo(run.dest.clone()));
                    return false;
                }
                Some(Err(e)) => {
                    let cancelled = run.cancel.is_cancelled();
                    self.error = Some(if cancelled { "Clone cancelled.".into() } else { e });
                    // A cancelled or failed clone leaves a partial folder behind; git removes it
                    // itself on failure, but not when killed.
                    if cancelled && run.dest.is_dir() {
                        let _ = std::fs::remove_dir_all(&run.dest);
                    }
                    self.run = None;
                }
                None => {}
            }
        }
        let running = self.run.is_some();
        let mut cancel_clone = false;
        let (out, ()) = modal(ctx, "clone", "Clone repository", 600.0, "Clone", |ui, ctl| {
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("clone-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Source URL / path:");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.url)
                            .desired_width(420.0)
                            .hint_text("https://github.com/user/repo.git"),
                    );
                    ctl.focus(&r);
                    if r.changed() && !self.name_edited {
                        self.name = folder_from_url(&self.url);
                    }
                    ui.end_row();
                    ui.label("Parent folder:");
                    ui.vertical(|ui| {
                        ui.set_width(420.0);
                        folder_picker(ui, parent, "Clone into folder");
                    });
                    ui.end_row();
                    ui.label("Folder name:");
                    if ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(420.0)).changed() {
                        self.name_edited = !self.name.is_empty();
                    }
                    ui.end_row();
                    ui.label("Branch:");
                    ui.add(egui::TextEdit::singleline(&mut self.branch).desired_width(200.0).hint_text("default"));
                    ui.end_row();
                });
                ui.collapsing("Advanced options", |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.shallow, "Shallow clone, depth");
                        ui.add_enabled(self.shallow, egui::DragValue::new(&mut self.depth).range(1..=100_000));
                    });
                    ui.checkbox(&mut self.partial, "Partial clone (download file contents on demand: --filter=blob:none)");
                    ui.checkbox(&mut self.submodules, "Recurse submodules");
                });
            });
            let dest = Path::new(parent.trim()).join(self.name.trim());
            if self.url.trim().is_empty() || self.name.trim().is_empty() {
                ctl.invalid();
            } else if !Path::new(parent.trim()).is_dir() {
                error(ui, "The parent folder does not exist.");
                ctl.invalid();
            } else if dest.exists() && !(dest.is_dir() && dir_is_empty(&dest)) && !running {
                error(ui, format!("{} already exists and is not empty.", dest.display()));
                ctl.invalid();
            } else if !running {
                hint(ui, format!("Clone into {}", dest.display()));
            }
            if let Some(e) = &self.error {
                error(ui, e.clone());
            }
            if let Some(run) = &self.run {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Cloning…");
                    if ui.button("Cancel clone").clicked() {
                        cancel_clone = true;
                    }
                });
                egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(4).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                        for l in run.lines.lock().iter() {
                            ui.label(RichText::new(l).monospace().small());
                        }
                    });
                });
                ctl.invalid();
            }
        });
        if cancel_clone && let Some(r) = &self.run {
            r.cancel.cancel();
        }
        match out {
            Outcome::None => true,
            Outcome::Cancel => {
                // Closing while cloning cancels the clone.
                if let Some(r) = &self.run {
                    r.cancel.cancel();
                    return true;
                }
                false
            }
            Outcome::Ok => {
                self.start(ctx, shared);
                true
            }
        }
    }
}

// ---------------------------------------------------------------- init

#[derive(Default)]
pub struct InitRepo {
    path: String,
    task: Option<Task<Result<PathBuf, String>>>,
    error: Option<String>,
}

impl AppDialog for InitRepo {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        if let Some(t) = &mut self.task {
            match t.take() {
                Some(Ok(p)) => {
                    shared.actions.push(Action::OpenRepo(p));
                    return false;
                }
                Some(Err(e)) => {
                    self.error = Some(e);
                    self.task = None;
                }
                None => {}
            }
        }
        let busy = self.task.is_some();
        let (out, ()) = modal(ctx, "init", "Create new repository", 540.0, "Create", |ui, ctl| {
            ui.label("Folder (created if missing):");
            folder_picker(ui, &mut self.path, "Create repository in");
            let p = Path::new(self.path.trim());
            if self.path.trim().is_empty() {
                ctl.invalid();
            } else if p.join(".git").exists() {
                warning(ui, "This folder already is a git repository; it will just be opened.");
            } else if p.is_dir() && !dir_is_empty(p) {
                hint(ui, "The folder is not empty: existing files stay untracked.");
            }
            if let Some(e) = &self.error {
                error(ui, e.clone());
            }
            if busy {
                ui.spinner();
                ctl.invalid();
            }
        });
        match out {
            Outcome::None => true,
            Outcome::Cancel => busy,
            Outcome::Ok => {
                let path = PathBuf::from(self.path.trim());
                if path.join(".git").exists() {
                    shared.actions.push(Action::OpenRepo(path));
                    return false;
                }
                let log = shared.log.clone();
                self.error = None;
                self.task = Some(Task::spawn(ctx, move || {
                    gk_core::init(&path, log).map(|_| path).map_err(|e| format!("{e:#}"))
                }));
                true
            }
        }
    }
}

// ---------------------------------------------------------------- bookmark

pub struct Bookmark {
    path: PathBuf,
    name: String,
    group: String,
    new_group: String,
}

impl Bookmark {
    pub fn new(path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self { path, name, group: String::new(), new_group: String::new() }
    }
}

const NEW_GROUP: &str = "\u{1}new";

impl AppDialog for Bookmark {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        let exists = shared.settings.find_bookmark(&self.path).is_some();
        let groups = shared.settings.groups.clone();
        let (out, ()) = modal(ctx, "bookmark", "Add bookmark", 480.0, "Add", |ui, ctl| {
            hint(ui, self.path.display().to_string());
            egui::Grid::new("bookmark-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Name:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(300.0));
                ctl.focus(&r);
                ui.end_row();
                ui.label("Group:");
                let shown = match self.group.as_str() {
                    "" => "(none)",
                    NEW_GROUP => "New group…",
                    g => g,
                };
                egui::ComboBox::from_id_salt("bookmark-group").width(300.0).selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.group, String::new(), "(none)");
                    for g in &groups {
                        ui.selectable_value(&mut self.group, g.clone(), g);
                    }
                    ui.selectable_value(&mut self.group, NEW_GROUP.to_string(), "New group…");
                });
                ui.end_row();
                if self.group == NEW_GROUP {
                    ui.label("New group:");
                    ui.add(egui::TextEdit::singleline(&mut self.new_group).desired_width(300.0));
                    ui.end_row();
                }
            });
            if exists {
                error(ui, "This repository is already bookmarked.");
                ctl.invalid();
            }
            if self.group == NEW_GROUP && self.new_group.trim().is_empty() {
                ctl.invalid();
            }
        });
        finish(out, || {
            let group = match self.group.as_str() {
                "" => None,
                NEW_GROUP => Some(self.new_group.trim().to_string()),
                g => Some(g.to_string()),
            };
            shared.settings.add_bookmark(self.name.trim(), self.path.clone(), group);
        })
    }
}

// ---------------------------------------------------------------- about

#[derive(Default)]
pub struct About {
    git: Option<Task<String>>,
    git_text: String,
}

impl AppDialog for About {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        let log = shared.log.clone();
        let task = self.git.get_or_insert_with(|| {
            Task::spawn(ctx, move || {
                let git = gk_cli::Git::new(std::env::temp_dir(), log);
                let exe = git.exe().display().to_string();
                match git.version() {
                    Ok((a, b, c)) => {
                        let old = (a, b) < gk_cli::MIN_VERSION;
                        let note = if old {
                            format!("  (GitKwaken needs {}.{}+)", gk_cli::MIN_VERSION.0, gk_cli::MIN_VERSION.1)
                        } else {
                            String::new()
                        };
                        format!("{a}.{b}.{c}{note}\n{exe}")
                    }
                    Err(e) => format!("not found: {e:#}\n{exe}"),
                }
            })
        });
        if let Some(t) = task.take() {
            self.git_text = t;
        }
        let (out, ()) = modal(ctx, "about", "About GitKwaken", 460.0, "OK", |ui, ctl| {
            ctl.show_ok = false;
            ctl.cancel_label = "Close";
            ui.label(RichText::new(format!("GitKwaken {}", env!("CARGO_PKG_VERSION"))).strong().size(18.0));
            ui.label("A fast git client.");
            ui.add_space(6.0);
            egui::Grid::new("about-grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label("Renderer:");
                ui.label(gk_dx11::backend_info());
                ui.end_row();
                ui.label("Git:");
                if self.git_text.is_empty() {
                    ui.spinner();
                } else {
                    ui.label(&self.git_text);
                }
                ui.end_row();
                ui.label("Settings:");
                ui.label(gk_settings::Settings::path().display().to_string());
                ui.end_row();
            });
        });
        // About has no OK button: Enter closes too.
        out == Outcome::None && !ctx.input(|i| i.key_pressed(egui::Key::Enter))
    }
}
