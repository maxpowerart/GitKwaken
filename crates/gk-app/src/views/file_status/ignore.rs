//! "Ignore › Custom pattern…": a pattern editor with validation and a live
//! preview of the untracked paths it would ignore (`git ls-files -o -i
//! --exclude=<pattern>`, debounced, on a worker thread).

use egui::{Id, RichText};
use gk_core::Repo;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_millis(300);
const SHOWN: usize = 12;

type Preview = Result<Vec<String>, String>;

/// Checks a single .gitignore line.
pub fn check_pattern(p: &str) -> Result<(), String> {
    if p.trim().is_empty() {
        return Err("Enter a pattern.".into());
    }
    if p.contains(['\n', '\r']) {
        return Err("One pattern per line.".into());
    }
    if p.starts_with('#') {
        return Err("A line starting with '#' is a comment (escape it as \\#).".into());
    }
    if p.trim() == "!" || p.trim() == "/" {
        return Err("This pattern matches nothing.".into());
    }
    if p.contains("***") {
        return Err("Use ** to match folders at any depth.".into());
    }
    if p.ends_with('\\') {
        return Err("A trailing backslash is not allowed.".into());
    }
    Ok(())
}

/// Untracked paths matched by `pattern` (folders count once).
pub fn matching(repo: &Repo, pattern: &str) -> Preview {
    let arg = format!("--exclude={pattern}");
    let o = repo
        .git()
        .run_raw(
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--directory",
                "--no-empty-directory",
                &arg,
            ],
            None,
        )
        .map_err(|e| format!("{e:#}"))?;
    if !o.success() {
        return Err(o.stderr_str().trim().to_string());
    }
    Ok(o.stdout_str()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub struct IgnoreCustom {
    pub pattern: String,
    first: bool,
    edited: Instant,
    /// Pattern of the running / last preview.
    preview_of: Option<String>,
    running: Option<Arc<Mutex<Option<Preview>>>>,
    result: Option<Preview>,
}

pub enum Outcome {
    Open,
    Cancel,
    Ignore(String),
}

impl IgnoreCustom {
    pub fn new(pattern: String) -> Self {
        Self {
            pattern,
            first: true,
            edited: Instant::now()
                .checked_sub(DELAY)
                .unwrap_or_else(Instant::now),
            preview_of: None,
            running: None,
            result: None,
        }
    }

    fn poll(&mut self, ctx: &egui::Context, repo: &Repo) {
        let done = self.running.as_ref().and_then(|slot| slot.lock().take());
        if let Some(r) = done {
            self.running = None;
            self.result = Some(r);
        }
        let p = self.pattern.trim().to_string();
        if self.preview_of.as_deref() == Some(p.as_str()) || self.running.is_some() {
            return;
        }
        if check_pattern(&p).is_err() {
            self.preview_of = Some(p);
            self.result = None;
            return;
        }
        let since = self.edited.elapsed();
        if since < DELAY {
            ctx.request_repaint_after(DELAY - since);
            return;
        }
        let slot: Arc<Mutex<Option<Preview>>> = Arc::default();
        let (s2, repo, ctx2, pat) = (slot.clone(), repo.clone(), ctx.clone(), p.clone());
        std::thread::spawn(move || {
            *s2.lock() = Some(matching(&repo, &pat));
            ctx2.request_repaint();
        });
        self.preview_of = Some(p);
        self.running = Some(slot);
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        repo: &Repo,
        muted: egui::Color32,
        err: egui::Color32,
    ) -> Outcome {
        self.poll(ctx, repo);
        let mut out = Outcome::Open;
        let valid = check_pattern(self.pattern.trim());
        let resp = egui::Modal::new(Id::new("fs-ignore-custom")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Ignore by pattern");
            ui.label("Pattern to add to .gitignore:");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.pattern)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("e.g. *.log, build/, /secret.txt")
                    .desired_width(f32::INFINITY),
            );
            if self.first {
                self.first = false;
                r.request_focus();
            }
            if r.changed() {
                self.edited = Instant::now();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            match &valid {
                Err(e) if !self.pattern.is_empty() => {
                    ui.colored_label(err, e);
                }
                Err(_) => {}
                Ok(()) => match &self.result {
                    Some(Ok(paths)) => {
                        ui.label(format!("Matches {} untracked path(s):", paths.len()));
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for p in paths.iter().take(SHOWN) {
                                    ui.label(RichText::new(p).monospace().small());
                                }
                                if paths.len() > SHOWN {
                                    ui.label(
                                        RichText::new(format!(
                                            "… and {} more",
                                            paths.len() - SHOWN
                                        ))
                                        .color(muted),
                                    );
                                }
                            });
                    }
                    Some(Err(e)) => {
                        ui.colored_label(err, e);
                    }
                    None => {}
                },
            }
            if self.running.is_some() {
                ui.spinner();
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ok = ui.add_enabled(
                    valid.is_ok(),
                    egui::Button::new(RichText::new("Ignore").strong()),
                );
                if (ok.clicked() || enter) && valid.is_ok() {
                    out = Outcome::Ignore(self.pattern.trim().to_string());
                }
                if ui.button("Cancel").clicked() {
                    out = Outcome::Cancel;
                }
            });
        });
        if resp.should_close() && matches!(out, Outcome::Open) {
            out = Outcome::Cancel;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_validation() {
        assert!(check_pattern("*.log").is_ok());
        assert!(check_pattern("build/").is_ok());
        assert!(check_pattern("\\#notes").is_ok());
        assert!(check_pattern("").is_err());
        assert!(check_pattern("  ").is_err());
        assert!(check_pattern("#x").is_err());
        assert!(check_pattern("a\nb").is_err());
        assert!(check_pattern("a\\").is_err());
        assert!(check_pattern("/").is_err());
    }
}
