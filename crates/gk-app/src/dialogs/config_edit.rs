//! Text editors for the repository's `.gitignore`, its `.git/config` and the
//! global `~/.gitconfig`. Config files are validated with
//! `git config -f <tmp> --list` before saving, and the previous version is
//! kept as `<file>.bak`. The `.gitignore` editor previews how many untracked
//! paths the edited rules ignore.

use super::common::*;
use super::{AppDialog, TabDialog};
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_cli::{CommandLog, Git};
use gk_core::Snapshot;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    Gitignore,
    RepoConfig,
    GlobalConfig,
}

impl ConfigKind {
    fn title(self) -> &'static str {
        match self {
            ConfigKind::Gitignore => "Edit .gitignore",
            ConfigKind::RepoConfig => "Edit repository config",
            ConfigKind::GlobalConfig => "Edit global git config",
        }
    }

    fn is_config(self) -> bool {
        self != ConfigKind::Gitignore
    }
}

/// Wait after the last keystroke before the ignore preview runs.
const PREVIEW_DELAY: Duration = Duration::from_millis(350);
/// Preview lines shown under the editor.
const PREVIEW_SHOWN: usize = 8;

/// `GIT_CONFIG_GLOBAL`, else `$HOME/.gitconfig`, else `%USERPROFILE%\.gitconfig`.
pub fn global_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GIT_CONFIG_GLOBAL").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".gitconfig"))
}

/// Path of the file to edit and its current content ("" when missing).
fn load(
    kind: ConfigKind,
    workdir: Option<&Path>,
    log: &CommandLog,
) -> Result<(PathBuf, String), String> {
    let path = match (kind, workdir) {
        (ConfigKind::Gitignore, Some(w)) => w.join(".gitignore"),
        (ConfigKind::RepoConfig, Some(w)) => {
            // Linked worktrees share the main repository's config.
            let out = Git::new(w, log.clone())
                .read(&["rev-parse", "--git-path", "config"])
                .map_err(|e| format!("{e:#}"))?;
            let p = PathBuf::from(out.trim());
            if p.is_absolute() { p } else { w.join(p) }
        }
        (ConfigKind::GlobalConfig, _) => {
            global_config_path().ok_or("Cannot find the home folder.")?
        }
        (_, None) => return Err("No repository.".into()),
    };
    let text = match std::fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    Ok((path, text))
}

fn temp_file(tag: &str, text: &str) -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "gitkwaken-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&p, text).map_err(|e| format!("Cannot write {}: {e}", p.display()))?;
    Ok(p)
}

/// `git config -f <tmp> --list` over `text`; the error is git's message.
pub fn validate_config(text: &str, log: &CommandLog) -> Result<(), String> {
    let tmp = temp_file("config-check", text)?;
    let git = Git::new(std::env::temp_dir(), log.clone());
    let f = tmp.to_string_lossy().into_owned();
    let r = git.run_raw(&["config", "-f", &f, "--list"], None);
    let _ = std::fs::remove_file(&tmp);
    match r {
        Ok(o) if o.success() => Ok(()),
        Ok(o) => Err(o.stderr_str().trim().replace(&f, "<config>")),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Untracked paths ignored by the rules in `text` alone (folders count once).
pub fn ignored_by(workdir: &Path, text: &str, log: &CommandLog) -> Result<Vec<String>, String> {
    let tmp = temp_file("ignore-preview", text)?;
    let arg = format!("--exclude-from={}", tmp.display());
    let r = Git::new(workdir, log.clone()).run_raw(
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--directory",
            "--no-empty-directory",
            &arg,
        ],
        None,
    );
    let _ = std::fs::remove_file(&tmp);
    match r {
        Ok(o) if o.success() => Ok(o
            .stdout_str()
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Ok(o) => Err(o.stderr_str().trim().to_string()),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Writes `text` to `path` (temp file + rename), keeping the old file as `<name>.bak`.
/// Returns the backup path when one was made.
pub fn save_with_backup(path: &Path, text: &str) -> Result<Option<PathBuf>, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let backup = path.with_file_name(format!("{name}.bak"));
    let made = if path.is_file() {
        std::fs::copy(path, &backup)
            .map_err(|e| format!("Cannot back up {}: {e}", path.display()))?;
        Some(backup)
    } else {
        None
    };
    let tmp = path.with_file_name(format!("{name}.gitkwaken-tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Cannot replace {}: {e}", path.display())
    })?;
    Ok(made)
}

pub struct ConfigEditor {
    kind: ConfigKind,
    loading: Option<Task<Result<(PathBuf, String), String>>>,
    path: Option<PathBuf>,
    original: String,
    text: String,
    load_error: Option<String>,
    preview: Option<Task<Result<Vec<String>, String>>>,
    /// Text of the last finished (or running) preview.
    preview_of: Option<String>,
    preview_result: Option<Result<Vec<String>, String>>,
    edited_at: Option<Instant>,
    saving: Option<Task<Result<Option<PathBuf>, String>>>,
    save_error: Option<String>,
    focus_end: bool,
}

impl ConfigEditor {
    pub fn new(kind: ConfigKind) -> Self {
        Self {
            kind,
            loading: None,
            path: None,
            original: String::new(),
            text: String::new(),
            load_error: None,
            preview: None,
            preview_of: None,
            preview_result: None,
            edited_at: None,
            saving: None,
            save_error: None,
            focus_end: true,
        }
    }

    fn poll(&mut self, ctx: &Context, workdir: Option<&Path>, log: &CommandLog) {
        if self.path.is_none() && self.loading.is_none() && self.load_error.is_none() {
            let (kind, w, log) = (self.kind, workdir.map(Path::to_path_buf), log.clone());
            self.loading = Some(Task::spawn(ctx, move || load(kind, w.as_deref(), &log)));
        }
        if let Some(t) = &mut self.loading
            && let Some(r) = t.take()
        {
            self.loading = None;
            match r {
                Ok((p, text)) => {
                    self.path = Some(p);
                    self.original = text.clone();
                    self.text = text;
                }
                Err(e) => self.load_error = Some(e),
            }
        }
        // Ignore preview: debounced, one at a time, latest text wins.
        if self.kind == ConfigKind::Gitignore && self.path.is_some() {
            if let Some(t) = &mut self.preview
                && let Some(r) = t.take()
            {
                self.preview = None;
                self.preview_result = Some(r);
            }
            let stale = self.preview_of.as_deref() != Some(self.text.as_str());
            let quiet = self.edited_at.is_none_or(|t| t.elapsed() >= PREVIEW_DELAY);
            if stale && self.preview.is_none() {
                if quiet && let Some(w) = workdir {
                    let (w, text, log) = (w.to_path_buf(), self.text.clone(), log.clone());
                    self.preview_of = Some(self.text.clone());
                    self.preview = Some(Task::spawn(ctx, move || ignored_by(&w, &text, &log)));
                } else if let Some(t) = self.edited_at {
                    ctx.request_repaint_after(PREVIEW_DELAY.saturating_sub(t.elapsed()));
                }
            }
        }
    }

    /// Shared body of the tab and app dialogs; returns false to close.
    fn ui(&mut self, ctx: &Context, workdir: Option<&Path>, log: &CommandLog) -> bool {
        self.poll(ctx, workdir, log);
        if let Some(t) = &mut self.saving {
            match t.take() {
                Some(Ok(_)) => return false,
                Some(Err(e)) => {
                    self.save_error = Some(e);
                    self.saving = None;
                }
                None => {}
            }
        }
        let busy = self.saving.is_some();
        let kind = self.kind;
        let editor_h = (ctx.content_rect().height() - 300.0).clamp(140.0, 420.0);
        let (out, ()) = modal(
            ctx,
            "config-edit",
            kind.title(),
            720.0,
            "Save",
            |ui, ctl| {
                if let Some(e) = &self.load_error {
                    error(ui, e.clone());
                    ctl.invalid();
                    return;
                }
                let Some(path) = self.path.clone() else {
                    ui.spinner();
                    ctl.invalid();
                    return;
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(path.display().to_string()).monospace().weak());
                    if !path.exists() {
                        hint(ui, "(new file)");
                    }
                });
                let id = egui::Id::new(("gk-config-editor", kind as u8));
                let r = egui::ScrollArea::vertical()
                    .id_salt("config-edit-scroll")
                    .max_height(editor_h)
                    .min_scrolled_height(editor_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_sized(
                            [ui.available_width(), editor_h],
                            egui::TextEdit::multiline(&mut self.text)
                                .id(id)
                                .font(egui::TextStyle::Monospace)
                                .code_editor()
                                .lock_focus(true)
                                .desired_width(f32::INFINITY),
                        )
                    })
                    .inner;
                ctl.multiline(&r);
                if self.focus_end {
                    self.focus_end = false;
                    r.request_focus();
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                        let end = egui::text::CCursor::new(self.text.chars().count());
                        st.cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(end)));
                        st.store(ui.ctx(), id);
                    }
                }
                if r.changed() {
                    self.edited_at = Some(Instant::now());
                }
                let changed = self.text != self.original;
                match kind {
                    ConfigKind::Gitignore => {
                        ui.horizontal(|ui| {
                        match &self.preview_result {
                            Some(Ok(paths)) => {
                                ui.label(format!(
                                    "{} untracked path(s) ignored by these rules",
                                    paths.len()
                                ))
                                .on_hover_text("Folders count once. Rules of other .gitignore files are not included.");
                            }
                            Some(Err(e)) => error(ui, e.clone()),
                            None => {}
                        }
                        if self.preview.is_some()
                            || self.preview_of.as_deref() != Some(self.text.as_str())
                        {
                            ui.spinner();
                        }
                    });
                        if let Some(Ok(paths)) = &self.preview_result
                            && !paths.is_empty()
                        {
                            let more = paths.len().saturating_sub(PREVIEW_SHOWN);
                            let mut list = paths
                                .iter()
                                .take(PREVIEW_SHOWN)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("   ");
                            if more > 0 {
                                list.push_str(&format!("   … and {more} more"));
                            }
                            hint(ui, list);
                        }
                    }
                    ConfigKind::RepoConfig | ConfigKind::GlobalConfig => {
                        hint(
                            ui,
                            "Checked with `git config --list` before saving; the previous file is kept as .bak.",
                        );
                    }
                }
                if let Some(e) = &self.save_error {
                    error(ui, e.clone());
                }
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Saving…");
                    });
                }
                if !changed || busy {
                    ctl.invalid();
                }
            },
        );
        match out {
            Outcome::None => true,
            Outcome::Cancel => busy,
            Outcome::Ok => {
                let Some(path) = self.path.clone() else {
                    return true;
                };
                let (text, log) = (self.text.clone(), log.clone());
                let check = kind.is_config();
                self.save_error = None;
                self.saving = Some(Task::spawn(ctx, move || {
                    if check {
                        validate_config(&text, &log)
                            .map_err(|e| format!("Not saved — the config is invalid: {e}"))?;
                    }
                    save_with_backup(&path, &text)
                }));
                true
            }
        }
    }
}

impl TabDialog for ConfigEditor {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        _snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        let w = tab.path.clone();
        let keep = self.ui(ctx, Some(&w), shared.log);
        if !keep && self.kind != ConfigKind::GlobalConfig && self.text != self.original {
            // Saved: the status (ignored files) and config readers refresh.
            tab.session.refresh_all();
        }
        keep
    }
}

impl AppDialog for ConfigEditor {
    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        self.ui(ctx, None, shared.log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success());
        assert!(matches!(ok, Ok(true)), "git {args:?} failed");
    }

    #[test]
    fn validate_save_and_preview() {
        let log = CommandLog::default();
        assert!(validate_config("[core]\n\tbare = false\n[gk]\n\tx = 1\n", &log).is_ok());
        let e = validate_config("[broken\n", &log).unwrap_err();
        assert!(!e.is_empty());

        let root = std::env::temp_dir().join(format!("gk-cfg-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build")).unwrap();
        git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.log"), "x").unwrap();
        std::fs::write(root.join("b.txt"), "x").unwrap();
        std::fs::write(root.join("build/out.o"), "x").unwrap();
        let ignored = ignored_by(&root, "*.log\nbuild/\n", &log).unwrap();
        assert_eq!(ignored, vec!["a.log".to_string(), "build/".to_string()]);
        assert!(ignored_by(&root, "", &log).unwrap().is_empty());

        let f = root.join("cfg");
        assert_eq!(save_with_backup(&f, "one").unwrap(), None);
        let b = save_with_backup(&f, "two").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(b).unwrap(), "one");

        let (p, text) = load(ConfigKind::RepoConfig, Some(&root), &log).unwrap();
        assert!(p.ends_with("config") && text.contains("[core]"), "{p:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
