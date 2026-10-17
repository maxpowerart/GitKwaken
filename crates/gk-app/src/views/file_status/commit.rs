//! Commit box: message editor with subject guide, recent messages, amend,
//! sign-off, push-after-commit, plus a SourceTree-like "Options ▾" menu:
//! GPG/SSH signing, skip hooks, author override and the commit template
//! (`commit.template`, prefilled into an empty message). The commit (and
//! optional push) runs on the session's operation thread; the outcome comes
//! back through an atomic, and hook output is shown as an info message.

use crate::app::Shared;
use crate::tab::RepoTab;
use anyhow::Context as _;
use egui::{RichText, Ui};
use gk_cli::Line;
use gk_core::ops::{CommitOptions, PushOptions};
use gk_core::{MessageKind, Repo, RepoState, Snapshot};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 0;
const DONE: u8 = 1;
const FAILED: u8 = 2;

/// Commit-related git config read once per tab in the background.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitConfig {
    /// Content of the `commit.template` file.
    pub template: Option<String>,
    /// `commit.gpgsign`.
    pub gpgsign: bool,
    /// "Name <email>" git would use as author (placeholder for the override).
    pub ident: String,
}

impl CommitConfig {
    pub fn load(repo: &Repo) -> CommitConfig {
        let git = repo.git();
        let get = |args: &[&str]| {
            git.run_raw(args, None)
                .ok()
                .filter(|o| o.success())
                .map(|o| o.stdout_str().trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let template = get(&["config", "--path", "commit.template"]).and_then(|p| {
            let p = std::path::PathBuf::from(p);
            let p = if p.is_absolute() {
                p
            } else {
                repo.workdir().join(p)
            };
            std::fs::read(p)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).replace("\r\n", "\n"))
        });
        let gpgsign = get(&["config", "--bool", "commit.gpgsign"]).is_some_and(|v| v == "true");
        let ident = get(&["var", "GIT_AUTHOR_IDENT"])
            .and_then(|s| s.rfind('>').map(|i| s[..=i].to_string()))
            .unwrap_or_default();
        CommitConfig {
            template: template.filter(|t| !t.trim().is_empty()),
            gpgsign,
            ident,
        }
    }
}

/// The message as git will store it with `--cleanup=strip`: no comment
/// lines, no surrounding blank lines.
pub fn stripped(msg: &str) -> String {
    msg.lines()
        .filter(|l| !l.starts_with('#'))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Validates an author override "Name <email>"; returns it normalized.
pub fn parse_author(s: &str) -> Result<String, String> {
    let s = s.trim();
    let bad = || "Use the form: Name <email@example.com>".to_string();
    let (name, rest) = s.split_once('<').ok_or_else(bad)?;
    let email = rest.strip_suffix('>').ok_or_else(bad)?.trim();
    let name = name.trim();
    if name.is_empty() {
        return Err("The author name is missing.".into());
    }
    if name.contains(['<', '>']) || email.contains(['<', '>']) {
        return Err(bad());
    }
    if email.is_empty() || email.contains(char::is_whitespace) || !email.contains('@') {
        return Err("The email address is not valid.".into());
    }
    Ok(format!("{name} <{email}>"))
}

/// Output lines of `git commit` that are its normal summary.
fn is_summary(line: &str) -> bool {
    let t = line.trim_start();
    line.trim().is_empty()
        || (line.starts_with('[') && line.contains("] "))
        || t.starts_with("Date:")
        || t.starts_with("Author:")
        || t.starts_with("create mode ")
        || t.starts_with("delete mode ")
        || t.starts_with("rename ")
        || t.starts_with("copy ")
        || t.starts_with("mode change ")
        || (line.starts_with(' ') && (t.contains(" file changed") || t.contains(" files changed")))
}

/// What `git commit` printed beyond its summary: hook output, warnings.
pub fn extra_output(stdout: &str, stderr: &str) -> Option<String> {
    let mut lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    lines.extend(stdout.lines().filter(|l| !is_summary(l)));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// `git commit` like [`Repo::commit`], streaming its output to `progress`
/// and returning (summary, extra output such as hook messages).
fn run_commit(
    repo: &Repo,
    opts: &CommitOptions,
    cancel: &gk_cli::CancelToken,
    progress: &mut (dyn FnMut(Line) + Send),
) -> anyhow::Result<(String, Option<String>)> {
    let empty = opts.message.trim().is_empty();
    if empty && !opts.amend {
        anyhow::bail!("commit message is empty");
    }
    let mut args: Vec<String> = if empty {
        vec!["commit".into(), "--no-edit".into()]
    } else {
        vec![
            "commit".into(),
            "-F".into(),
            "-".into(),
            "--cleanup=strip".into(),
        ]
    };
    for (on, flag) in [
        (opts.amend, "--amend"),
        (opts.signoff, "--signoff"),
        (opts.no_verify, "--no-verify"),
        (opts.allow_empty, "--allow-empty"),
    ] {
        if on {
            args.push(flag.into());
        }
    }
    match opts.sign {
        Some(true) => args.push("-S".into()),
        Some(false) => args.push("--no-gpg-sign".into()),
        None => {}
    }
    if let Some(a) = &opts.author {
        args.push(format!("--author={a}"));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdin = (!empty).then_some(opts.message.as_bytes());
    let out = repo
        .git()
        .run_streaming(&a, stdin, cancel, &mut *progress)
        .context("git commit")?;
    let (stdout, stderr) = (out.stdout_str(), out.stderr_str());
    out.check()?;
    let extra = extra_output(&stdout, &stderr);
    Ok((stdout, extra))
}

#[derive(Default)]
pub struct CommitBox {
    pub message: String,
    pub amend: bool,
    /// `None` until first shown; then initialized from settings.
    pub signoff: Option<bool>,
    pub push: Option<bool>,
    pub monospace: bool,
    /// Sign the commit (GPG/SSH); `None` until the config is known.
    sign: Option<bool>,
    /// The user changed `sign` (else settings / git config decide).
    sign_touched: bool,
    /// `--no-verify`.
    skip_hooks: bool,
    /// Author override "Name <email>" (when `author_on`).
    author_on: bool,
    author: String,
    /// Commit config (template, gpgsign, identity) being loaded / loaded.
    config_loading: Option<Arc<Mutex<Option<CommitConfig>>>>,
    config: Option<CommitConfig>,
    focus_requested: bool,
    /// HEAD (or MERGE_MSG) message being loaded in the background.
    loading: Option<Arc<Mutex<Option<String>>>>,
    /// Message loaded for amend; cleared again when amend is turned off.
    loaded_head: Option<String>,
    /// MERGE_MSG was offered for the current in-progress operation.
    merge_msg_offered: bool,
    /// Running commit: outcome flag and the committed message.
    running: Option<(Arc<AtomicU8>, String)>,
}

impl CommitBox {
    /// Focuses the message editor on the next frame (keyboard shortcut).
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    fn template(&self) -> Option<&str> {
        self.config.as_ref()?.template.as_deref()
    }

    /// Prefills the commit template into an empty message.
    fn offer_template(&mut self) {
        if self.message.trim().is_empty()
            && !self.amend
            && self.loading.is_none()
            && let Some(t) = self.template()
        {
            self.message = t.trim_end().to_string() + "\n";
        }
    }

    /// The message is the template, untouched (git would abort the commit).
    fn is_unedited_template(&self) -> bool {
        self.template()
            .is_some_and(|t| !stripped(t).is_empty() && stripped(t) == stripped(&self.message))
    }

    /// Picks up background results: loaded messages and the commit outcome.
    fn poll(&mut self, state: &mut gk_settings::RepoState) {
        let got = self.loading.as_ref().and_then(|slot| slot.lock().take());
        if let Some(m) = got {
            self.loading = None;
            if self.message.trim().is_empty() || self.is_unedited_template() {
                self.message = m.trim_end().to_string();
                if self.amend {
                    self.loaded_head = Some(self.message.clone());
                }
            }
        }
        let got = self
            .config_loading
            .as_ref()
            .and_then(|slot| slot.lock().take());
        if let Some(c) = got {
            self.config_loading = None;
            self.config = Some(c);
            self.offer_template();
        }
        if let Some((flag, msg)) = &self.running {
            match flag.load(Ordering::SeqCst) {
                DONE => {
                    state.push_recent_message(stripped(msg));
                    if self.message.trim() == msg.trim() {
                        self.message.clear();
                    }
                    self.amend = false;
                    self.loaded_head = None;
                    self.running = None;
                    self.offer_template();
                }
                FAILED => self.running = None,
                _ => {}
            }
        }
    }

    fn load_async(
        &mut self,
        ctx: &egui::Context,
        f: impl FnOnce() -> Option<String> + Send + 'static,
    ) {
        let slot: Arc<Mutex<Option<String>>> = Arc::default();
        self.loading = Some(slot.clone());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            *slot.lock() = Some(f().unwrap_or_default());
            ctx.request_repaint();
        });
    }

    fn load_config(&mut self, ctx: &egui::Context, repo: &Repo) {
        let slot: Arc<Mutex<Option<CommitConfig>>> = Arc::default();
        self.config_loading = Some(slot.clone());
        let (ctx, repo) = (ctx.clone(), repo.clone());
        std::thread::spawn(move || {
            *slot.lock() = Some(CommitConfig::load(&repo));
            ctx.request_repaint();
        });
    }

    /// The `sign` option to pass: the user's choice, else settings (None = git config).
    fn sign_option(&self, settings: Option<bool>) -> Option<bool> {
        if self.sign_touched {
            self.sign
        } else {
            settings
        }
    }
}

/// Where "push after commit" goes: label and options.
fn push_target(snap: &Snapshot) -> Option<(String, PushOptions)> {
    let refs = &snap.refs;
    let b = refs.current_branch()?;
    if let Some(u) = b.upstream.as_ref().filter(|_| !b.upstream_gone) {
        let (remote, rb) = u.split_once('/')?;
        let opts = PushOptions {
            remote: remote.to_string(),
            refspecs: vec![(b.name.clone(), rb.to_string())],
            ..Default::default()
        };
        return Some((u.clone(), opts));
    }
    let remote = refs
        .remotes
        .iter()
        .find(|r| r.name == "origin")
        .or_else(|| refs.remotes.first())?;
    let opts = PushOptions {
        remote: remote.name.clone(),
        set_upstream: true,
        ..Default::default()
    };
    Some((format!("{}/{} (new)", remote.name, b.name), opts))
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    let RepoTab {
        status,
        state,
        session,
        ..
    } = tab;
    let cb = &mut status.commit;
    let p = shared.palette;
    if cb.config.is_none() && cb.config_loading.is_none() {
        cb.load_config(ui.ctx(), session.repo());
    }
    cb.poll(state);
    if cb.signoff.is_none() {
        cb.signoff = Some(shared.settings.commit.signoff);
    }
    if cb.push.is_none() {
        cb.push = Some(shared.settings.commit.push_after_commit);
    }
    if !cb.sign_touched {
        cb.sign = match (shared.settings.commit.sign, &cb.config) {
            (Some(s), _) => Some(s),
            (None, Some(c)) => Some(c.gpgsign),
            (None, None) => None,
        };
    }

    // Offer the prepared message of a merge / cherry-pick / revert once.
    let in_op = matches!(
        snap.refs.state,
        RepoState::Merging | RepoState::CherryPicking | RepoState::Reverting
    );
    if !in_op {
        cb.merge_msg_offered = false;
    } else if !cb.merge_msg_offered
        && cb.loading.is_none()
        && (cb.message.trim().is_empty() || cb.is_unedited_template())
    {
        cb.merge_msg_offered = true;
        let f = session.repo().git_dir().join("MERGE_MSG");
        cb.load_async(ui.ctx(), move || {
            let s = std::fs::read_to_string(f).ok()?;
            // Drop git's comment lines.
            Some(
                s.lines()
                    .filter(|l| !l.starts_with('#'))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        });
    }

    let staged = snap.status.staged.len();
    let msg_id = ui.make_persistent_id("gk-commit-message");
    if std::mem::take(&mut cb.focus_requested) {
        ui.memory_mut(|m| m.request_focus(msg_id));
    }
    let has_focus = ui.memory(|m| m.has_focus(msg_id));
    let mut want_commit =
        has_focus && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));

    // ---- header: subject counter
    let guide = shared.settings.commit.subject_guide_column.max(1) as usize;
    let body_col = shared.settings.commit.body_wrap_column as usize;
    let subject_len = cb
        .message
        .lines()
        .next()
        .map(|l| l.chars().count())
        .unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(if cb.amend {
                "Amend last commit"
            } else {
                "Commit message"
            })
            .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let color = if subject_len > guide {
                p.removed_fg
            } else {
                p.muted
            };
            ui.label(
                // Proportional: monospace sits higher than the neighbours.
                RichText::new(format!("{subject_len}/{guide}")).color(color),
            )
            .on_hover_text("Subject line length");
            ui.toggle_value(&mut cb.monospace, RichText::new("Aa").monospace())
                .on_hover_text("Monospace font");
            let recent = &state.recent_messages;
            ui.add_enabled_ui(!recent.is_empty(), |ui| {
                ui.menu_button("Recent ▾", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for m in recent {
                                let first = m.lines().next().unwrap_or("");
                                let short: String = first.chars().take(80).collect();
                                if ui.button(short).on_hover_text(m.as_str()).clicked() {
                                    cb.message = m.clone();
                                    ui.close();
                                }
                            }
                        });
                });
            });
        });
    });

    // ---- editor
    let font = if cb.monospace {
        egui::TextStyle::Monospace
    } else {
        egui::TextStyle::Body
    };
    let font_id = font.resolve(ui.style());
    let author_h = if cb.author_on { 28.0 } else { 0.0 };
    let options_h = 30.0 + author_h;
    let edit_h = (ui.available_height() - options_h).max(48.0);
    let out = egui::ScrollArea::vertical()
        .id_salt("gk-commit-scroll")
        .max_height(edit_h)
        .show(ui, |ui| {
            egui::TextEdit::multiline(&mut cb.message)
                .id(msg_id)
                .font(font)
                .desired_width(f32::INFINITY)
                .min_size(egui::vec2(0.0, edit_h - 4.0))
                .hint_text("Commit message — first line is the subject (Ctrl+Enter to commit)")
                .show(ui)
        });
    let te = out.inner;
    {
        // Guides: subject column on the first line, body wrap column below.
        let (row_h, char_w) = ui
            .ctx()
            .fonts_mut(|f| (f.row_height(&font_id), f.glyph_width(&font_id, 'n')));
        let painter = ui.painter().with_clip_rect(te.text_clip_rect);
        let x0 = te.galley_pos.x;
        let top = te.galley_pos.y;
        let faint = ui
            .visuals()
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .gamma_multiply(0.8);
        let gx = x0 + guide as f32 * char_w;
        // Not over the hint text of an empty message.
        if !cb.message.is_empty() {
            painter.vline(gx, top..=top + row_h, egui::Stroke::new(1.0, faint));
        }
        if body_col > 0 && cb.monospace {
            let bx = x0 + body_col as f32 * char_w;
            painter.vline(
                bx,
                top + row_h..=te.text_clip_rect.bottom(),
                egui::Stroke::new(1.0, faint.gamma_multiply(0.5)),
            );
        }
    }

    // ---- author override
    let author = if cb.author_on {
        let parsed = parse_author(&cb.author);
        ui.horizontal(|ui| {
            ui.label("Author:");
            let placeholder = cb
                .config
                .as_ref()
                .map(|c| c.ident.clone())
                .filter(|i| !i.is_empty())
                .unwrap_or_else(|| "Name <email@example.com>".into());
            let w = (ui.available_width() - 180.0).max(120.0);
            ui.add(
                egui::TextEdit::singleline(&mut cb.author)
                    .hint_text(placeholder)
                    .desired_width(w),
            );
            match &parsed {
                Err(e) if !cb.author.trim().is_empty() => {
                    ui.colored_label(p.removed_fg, e);
                }
                Err(_) => {
                    ui.label(RichText::new("required").color(p.muted));
                }
                Ok(_) => {}
            }
        });
        Some(parsed)
    } else {
        None
    };

    // ---- options + commit button
    let merging = snap.refs.state == RepoState::Merging;
    let push_target = push_target(snap);
    let unedited = cb.is_unedited_template();
    ui.horizontal(|ui| {
        let was_amend = cb.amend;
        ui.checkbox(&mut cb.amend, "Amend")
            .on_hover_text("Amend the last commit");
        if cb.amend != was_amend {
            if cb.amend {
                if cb.message.trim().is_empty() || cb.is_unedited_template() {
                    cb.message.clear();
                    let repo = session.repo().clone();
                    cb.load_async(ui.ctx(), move || repo.head_message().ok());
                }
            } else if cb.loaded_head.as_deref().is_some_and(|h| h == cb.message) {
                cb.message.clear();
                cb.loaded_head = None;
                cb.offer_template();
            }
        }
        if let Some(s) = &mut cb.signoff {
            ui.checkbox(s, "Sign-off")
                .on_hover_text("Add a Signed-off-by trailer");
        }
        if let Some(push) = &mut cb.push {
            match &push_target {
                Some((label, _)) => {
                    ui.checkbox(push, format!("Push to {label}"))
                        .on_hover_text("Push changes immediately after committing");
                }
                None => {
                    ui.add_enabled(false, egui::Checkbox::new(push, "Push"))
                        .on_disabled_hover_text("No remote to push to");
                }
            }
        }
        options_menu(ui, cb);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_msg = !stripped(&cb.message).is_empty();
            let author_ok = !matches!(author, Some(Err(_)));
            let enabled = cb.running.is_none()
                && (staged > 0 || cb.amend || merging)
                && ((has_msg && !unedited) || cb.amend)
                && author_ok;
            let label = if cb.amend { "Amend" } else { "Commit" };
            let r = ui.add_enabled(
                enabled,
                egui::Button::new(RichText::new(label).strong()).min_size(egui::vec2(80.0, 0.0)),
            );
            let r = if cb.running.is_some() {
                ui.spinner();
                r
            } else if staged == 0 && !cb.amend && !merging {
                r.on_disabled_hover_text("Nothing staged")
            } else if !author_ok {
                r.on_disabled_hover_text("Fix the author (Name <email>)")
            } else if unedited {
                r.on_disabled_hover_text("Edit the commit template first")
            } else {
                r.on_disabled_hover_text("Enter a commit message")
            };
            if r.clicked() {
                want_commit = true;
            }
            want_commit &= enabled;
        });
    });

    if want_commit {
        let opts = CommitOptions {
            message: if cb.amend && stripped(&cb.message).is_empty() {
                String::new()
            } else {
                cb.message.clone()
            },
            amend: cb.amend,
            signoff: cb.signoff.unwrap_or(false),
            no_verify: cb.skip_hooks,
            sign: cb.sign_option(shared.settings.commit.sign),
            author: author.and_then(Result::ok),
            ..Default::default()
        };
        let push = push_target
            .filter(|_| cb.push == Some(true))
            .map(|(_, o)| o);
        let flag = Arc::new(AtomicU8::new(PENDING));
        cb.running = Some((flag.clone(), cb.message.clone()));
        let label = if cb.amend { "Amend commit" } else { "Commit" };
        let s2 = session.clone();
        session.run_op(label, move |repo, cancel, progress| {
            match run_commit(repo, &opts, cancel, progress) {
                Ok((out, extra)) => {
                    flag.store(DONE, Ordering::SeqCst);
                    if let Some(extra) = extra {
                        s2.push_message(
                            MessageKind::Info,
                            format!("Commit created — output of git and its hooks:\n{extra}"),
                        );
                    }
                    match push {
                        Some(p) => {
                            let pushed = repo.push(&p, cancel, progress).map_err(|e| {
                                e.context("the commit was created, but pushing it failed")
                            })?;
                            Ok(format!("{out}\n{pushed}"))
                        }
                        None => Ok(out),
                    }
                }
                Err(e) => {
                    flag.store(FAILED, Ordering::SeqCst);
                    Err(e)
                }
            }
        });
    }
}

/// "Options ▾": signing, hooks, author override, template.
fn options_menu(ui: &mut Ui, cb: &mut CommitBox) {
    let active = cb.skip_hooks || cb.author_on || (cb.sign_touched && cb.sign.is_some());
    let text = if active {
        "Options ●▾"
    } else {
        "Options ▾"
    };
    ui.menu_button(text, |ui| {
        let mut sign = cb.sign.unwrap_or(false);
        let r = ui
            .add_enabled(
                cb.sign.is_some(),
                egui::Checkbox::new(&mut sign, "Sign commit (GPG / SSH)"),
            )
            .on_hover_text(
                "git commit -S (key from user.signingkey / gpg.format); default from \
                 Settings › Commit, else commit.gpgsign",
            );
        if r.changed() {
            cb.sign = Some(sign);
            cb.sign_touched = true;
        }
        ui.checkbox(&mut cb.skip_hooks, "Skip hooks (--no-verify)")
            .on_hover_text("Do not run the pre-commit and commit-msg hooks");
        ui.checkbox(&mut cb.author_on, "Override author…")
            .on_hover_text("Commit with another author (Name <email>)");
        ui.separator();
        let tpl = cb.template().map(str::to_string);
        let b = ui
            .add_enabled(tpl.is_some(), egui::Button::new("Insert commit template"))
            .on_disabled_hover_text("No commit.template is configured");
        if b.clicked()
            && let Some(t) = tpl
        {
            if !cb.message.trim().is_empty() && !cb.message.ends_with('\n') {
                cb.message.push('\n');
            }
            cb.message.push_str(t.trim_end());
            cb.message.push('\n');
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_core::refs::{Branch, Refs, Remote};

    fn snap_with(upstream: Option<&str>, remotes: &[&str]) -> Snapshot {
        let refs = Refs {
            branches: vec![Branch {
                name: "main".into(),
                target: "x".into(),
                upstream: upstream.map(String::from),
                ahead: 0,
                behind: 0,
                upstream_gone: false,
                is_head: true,
            }],
            remotes: remotes
                .iter()
                .map(|r| Remote {
                    name: r.to_string(),
                    fetch_url: String::new(),
                    push_url: String::new(),
                })
                .collect(),
            ..Default::default()
        };
        Snapshot {
            refs: Arc::new(refs),
            ..Default::default()
        }
    }

    #[test]
    fn push_target_uses_upstream_or_first_remote() {
        let (label, o) = push_target(&snap_with(Some("origin/feature/x"), &["origin"])).unwrap();
        assert_eq!(label, "origin/feature/x");
        assert_eq!(o.remote, "origin");
        assert_eq!(
            o.refspecs,
            vec![("main".to_string(), "feature/x".to_string())]
        );
        let (_, o) = push_target(&snap_with(None, &["up", "origin"])).unwrap();
        assert!(o.set_upstream && o.remote == "origin");
        assert!(push_target(&snap_with(None, &[])).is_none());
    }

    #[test]
    fn author_validation() {
        assert_eq!(
            parse_author("  Jane Doe <jane@x.org> ").unwrap(),
            "Jane Doe <jane@x.org>"
        );
        assert_eq!(parse_author("Jane<jane@x>").unwrap(), "Jane <jane@x>");
        for bad in [
            "",
            "Jane",
            "<jane@x.org>",
            "Jane <jane x@y>",
            "Jane <jane>",
            "Jane <a@b",
            "J <a@b> x",
        ] {
            assert!(parse_author(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn template_stripping_and_hook_output() {
        assert_eq!(
            stripped("Subject\n# comment\n\nBody  \n# x\n"),
            "Subject\n\nBody"
        );
        assert_eq!(stripped("# only\n#comments\n"), "");
        let summary =
            "[main 1a2b3c4] Subject\n 1 file changed, 2 insertions(+)\n create mode 100644 a.txt\n";
        assert_eq!(extra_output(summary, ""), None);
        assert_eq!(
            extra_output(summary, "lint: ok\n"),
            Some("lint: ok".to_string())
        );
        assert_eq!(
            extra_output(&format!("hook says hi\n{summary}"), ""),
            Some("hook says hi".to_string())
        );
    }

    #[test]
    fn commit_runs_hooks_and_reports_output() {
        let dir = std::env::temp_dir().join(format!("gk-commit-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?}"
            )
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(
            dir.join(".git/hooks/pre-commit"),
            "#!/bin/sh\necho 'pre-commit: checked'\n",
        )
        .unwrap();
        // git ignores hooks that are not executable (Unix only; Windows has no x bit).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join(".git/hooks/pre-commit"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"]);
        let repo = Repo::open(&dir, gk_cli::CommandLog::default()).unwrap();
        let opts = CommitOptions {
            message: "Subject\n# dropped\n".into(),
            author: Some("Other Person <other@example.com>".into()),
            ..Default::default()
        };
        let (_, extra) =
            run_commit(&repo, &opts, &gk_cli::CancelToken::new(), &mut |_| {}).unwrap();
        assert_eq!(extra.as_deref(), Some("pre-commit: checked"));
        let log = std::process::Command::new("git")
            .args(["log", "-1", "--format=%an <%ae>|%B"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert!(
            log.starts_with("Other Person <other@example.com>|Subject"),
            "{log}"
        );
        assert!(!log.contains("dropped"));
        // no_verify skips the hook.
        std::fs::write(dir.join("a.txt"), "b\n").unwrap();
        git(&["add", "a.txt"]);
        let opts = CommitOptions {
            message: "Second".into(),
            no_verify: true,
            ..Default::default()
        };
        let (_, extra) =
            run_commit(&repo, &opts, &gk_cli::CancelToken::new(), &mut |_| {}).unwrap();
        assert_eq!(extra, None);
        let cfg = CommitConfig::load(&repo);
        assert!(cfg.template.is_none() && !cfg.gpgsign);
        assert!(cfg.ident.starts_with("T <t@t>"), "{}", cfg.ident);
        std::fs::write(dir.join("tpl.txt"), "Title\n\n# Explain why\n").unwrap();
        git(&["config", "commit.template", "tpl.txt"]);
        assert_eq!(
            CommitConfig::load(&repo).template.as_deref(),
            Some("Title\n\n# Explain why\n")
        );
        drop(repo);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
