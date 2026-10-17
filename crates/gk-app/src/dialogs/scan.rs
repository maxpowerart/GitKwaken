//! "Scan folder for repositories": walks a folder tree in the background
//! (depth ≤ 6, skipping build output and dependency folders, not descending
//! into repositories) and lists what it finds, live, with Open / Bookmark.

use super::AppDialog;
use super::common::*;
use crate::app::{Action, Shared};
use egui::{Context, RichText};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Deepest folder level searched below the chosen folder.
pub const MAX_DEPTH: usize = 6;

/// Folder names never searched: dependencies, build output, system folders.
const SKIP: [&str; 7] = [
    ".git",
    "node_modules",
    "target",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".cargo",
    ".rustup",
];

/// A working copy: has a `.git` folder (or file, for worktrees/submodules).
pub fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Walks `root` breadth-first and calls `found` for every repository (whose
/// subfolders are then skipped). `visited` counts folders; stops early when
/// `cancel` is set.
pub fn scan(
    root: &Path,
    max_depth: usize,
    cancel: &AtomicBool,
    visited: &AtomicUsize,
    current: &Mutex<String>,
    found: &mut dyn FnMut(PathBuf),
) {
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        visited.fetch_add(1, Ordering::Relaxed);
        if is_repo(&dir) {
            found(dir);
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        *current.lock() = dir.display().to_string();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| {
                // Symlinks and junctions are not followed (loops, duplicates).
                e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink())
            })
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name))
            })
            .map(|e| e.path())
            .collect();
        subs.sort();
        queue.extend(subs.into_iter().map(|p| (p, depth + 1)));
    }
}

struct Scan {
    found: Arc<Mutex<Vec<PathBuf>>>,
    current: Arc<Mutex<String>>,
    visited: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl Scan {
    fn start(ctx: &Context, root: PathBuf) -> Self {
        // One separator style in the paths found ("D:/x" typed by hand).
        let root = if cfg!(windows) {
            PathBuf::from(root.to_string_lossy().replace('/', "\\"))
        } else {
            root
        };
        let s = Scan {
            found: Arc::default(),
            current: Arc::default(),
            visited: Arc::default(),
            done: Arc::default(),
            cancel: Arc::default(),
        };
        let (found, current, visited, done, cancel) = (
            s.found.clone(),
            s.current.clone(),
            s.visited.clone(),
            s.done.clone(),
            s.cancel.clone(),
        );
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("gk-scan".into())
            .spawn(move || {
                let mut last = std::time::Instant::now();
                scan(&root, MAX_DEPTH, &cancel, &visited, &current, &mut |p| {
                    found.lock().push(p);
                    if last.elapsed().as_millis() > 50 {
                        last = std::time::Instant::now();
                        ctx.request_repaint();
                    }
                });
                done.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            })
            .ok();
        s
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

pub struct ScanRepos {
    folder: String,
    group: String,
    scan: Option<Scan>,
}

impl ScanRepos {
    pub fn new() -> Self {
        Self {
            folder: String::new(),
            group: String::new(),
            scan: None,
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

impl AppDialog for ScanRepos {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        let mut open = None;
        let mut bookmark: Vec<PathBuf> = Vec::new();
        let groups = shared.settings.groups.clone();
        let (out, ()) = modal(
            ctx,
            "scan-repos",
            "Scan folder for repositories",
            680.0,
            "",
            |ui, ctl| {
                ctl.show_ok = false;
                ctl.enter_ok = false;
                ctl.cancel_label = "Close";
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.folder)
                            .hint_text("e.g. D:\\Projects")
                            .desired_width(ui.available_width() - 190.0),
                    );
                    ctl.focus(&r);
                    if ui.button("Browse…").clicked()
                        && let Some(p) = rfd::FileDialog::new()
                            .set_title("Scan folder for repositories")
                            .pick_folder()
                    {
                        self.folder = p.display().to_string();
                        self.scan = Some(Scan::start(ctx, p));
                    }
                    let running = self
                        .scan
                        .as_ref()
                        .is_some_and(|s| !s.done.load(Ordering::SeqCst));
                    if running {
                        if ui.button("Cancel").clicked()
                            && let Some(s) = &self.scan
                        {
                            s.cancel.store(true, Ordering::SeqCst);
                        }
                    } else {
                        let dir = PathBuf::from(self.folder.trim());
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let clicked = ui
                            .add_enabled(dir.is_dir(), egui::Button::new("Scan"))
                            .clicked();
                        if clicked || (enter && dir.is_dir()) {
                            self.scan = Some(Scan::start(ctx, dir));
                        }
                    }
                });
                hint(
                    ui,
                    format!(
                        "Searches {MAX_DEPTH} folder levels deep; skips node_modules, target and the inside of repositories."
                    ),
                );
                let Some(s) = &self.scan else {
                    return;
                };
                let found = s.found.lock().clone();
                let done = s.done.load(Ordering::SeqCst);
                let cancelled = s.cancel.load(Ordering::SeqCst);
                ui.horizontal(|ui| {
                    let n = s.visited.load(Ordering::Relaxed);
                    if !done {
                        ui.spinner();
                        let cur = s.current.lock().clone();
                        ui.label(RichText::new(format!("{n} folders… {cur}")).weak());
                    } else if cancelled {
                        ui.label(format!(
                            "Cancelled: {} repositories in {n} folders.",
                            found.len()
                        ));
                    } else {
                        ui.label(format!(
                            "Found {} repositories in {n} folders.",
                            found.len()
                        ));
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Bookmark into group:");
                    let mut opts = vec![String::new()];
                    opts.extend(groups.iter().cloned());
                    egui::ComboBox::from_id_salt("scan-group")
                        .selected_text(if self.group.is_empty() {
                            "(none)"
                        } else {
                            self.group.as_str()
                        })
                        .show_ui(ui, |ui| {
                            for g in &opts {
                                let text = if g.is_empty() { "(none)" } else { g.as_str() };
                                ui.selectable_value(&mut self.group, g.clone(), text);
                            }
                        });
                    let missing: Vec<PathBuf> = found
                        .iter()
                        .filter(|p| shared.settings.find_bookmark(p).is_none())
                        .cloned()
                        .collect();
                    if ui
                        .add_enabled(!missing.is_empty(), egui::Button::new("Bookmark all"))
                        .clicked()
                    {
                        bookmark = missing;
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("scan-results")
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for p in &found {
                            ui.horizontal(|ui| {
                                if ui.button("Open").clicked() {
                                    open = Some(p.clone());
                                }
                                let marked = shared.settings.find_bookmark(p).is_some();
                                let text = if marked { "Bookmarked" } else { "Bookmark" };
                                if ui
                                    .add_enabled(
                                        !marked,
                                        egui::Button::new(text).min_size(egui::vec2(90.0, 0.0)),
                                    )
                                    .clicked()
                                {
                                    bookmark.push(p.clone());
                                }
                                ui.label(RichText::new(name_of(p)).strong());
                                ui.add(
                                    egui::Label::new(RichText::new(p.display().to_string()).weak())
                                        .truncate(),
                                );
                            });
                        }
                        if done && found.is_empty() {
                            ui.label(RichText::new("No repositories found.").weak());
                        }
                    });
            },
        );
        let group = (!self.group.is_empty()).then(|| self.group.clone());
        for p in bookmark {
            shared.settings.add_bookmark(name_of(&p), p, group.clone());
        }
        if let Some(p) = open {
            shared.actions.push(Action::OpenRepo(p));
        }
        finish(out, || {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_repositories_without_descending() {
        let root = std::env::temp_dir().join(format!("gk-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            "a/.git",
            "a/nested/.git",
            "b/c/d/.git",
            "node_modules/x/.git",
            "e/target/y/.git",
            "1/2/3/4/5/6/7/.git",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("b/wt.git"), "x").unwrap();
        let mut v = Vec::new();
        let (c, n, cur) = (
            AtomicBool::new(false),
            AtomicUsize::new(0),
            Mutex::new(String::new()),
        );
        scan(&root, MAX_DEPTH, &c, &n, &cur, &mut |p| v.push(p));
        let rel: Vec<String> = v
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(rel, ["a", "b/c/d"]);
        c.store(true, Ordering::SeqCst);
        let mut none = Vec::new();
        scan(&root, MAX_DEPTH, &c, &n, &cur, &mut |p| none.push(p));
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
