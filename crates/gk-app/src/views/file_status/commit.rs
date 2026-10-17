//! Commit box: message editor with subject guide, recent messages, amend,
//! sign-off, push-after-commit. The commit (and optional push) runs on the
//! session's operation thread; the outcome comes back through an atomic.

use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{RichText, Ui};
use gk_core::ops::{CommitOptions, PushOptions};
use gk_core::{RepoState, Snapshot};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 0;
const DONE: u8 = 1;
const FAILED: u8 = 2;

#[derive(Default)]
pub struct CommitBox {
    pub message: String,
    pub amend: bool,
    /// `None` until first shown; then initialized from settings.
    pub signoff: Option<bool>,
    pub push: Option<bool>,
    pub monospace: bool,
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
    /// Picks up background results: loaded messages and the commit outcome.
    fn poll(&mut self, state: &mut gk_settings::RepoState) {
        if let Some(slot) = &self.loading {
            let got = slot.lock().take();
            if let Some(m) = got {
                self.loading = None;
                if self.message.trim().is_empty() {
                    self.message = m.trim_end().to_string();
                    if self.amend {
                        self.loaded_head = Some(self.message.clone());
                    }
                }
            }
        }
        if let Some((flag, msg)) = &self.running {
            match flag.load(Ordering::SeqCst) {
                DONE => {
                    state.push_recent_message(msg);
                    if self.message.trim() == msg.trim() {
                        self.message.clear();
                    }
                    self.amend = false;
                    self.loaded_head = None;
                    self.running = None;
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
    cb.poll(state);
    if cb.signoff.is_none() {
        cb.signoff = Some(shared.settings.commit.signoff);
    }
    if cb.push.is_none() {
        cb.push = Some(shared.settings.commit.push_after_commit);
    }

    // Offer the prepared message of a merge / cherry-pick / revert once.
    let in_op = matches!(
        snap.refs.state,
        RepoState::Merging | RepoState::CherryPicking | RepoState::Reverting
    );
    if !in_op {
        cb.merge_msg_offered = false;
    } else if !cb.merge_msg_offered && cb.loading.is_none() && cb.message.trim().is_empty() {
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
                RichText::new(format!("{subject_len}/{guide}"))
                    .color(color)
                    .monospace(),
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
    let options_h = 30.0;
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
        painter.vline(gx, top..=top + row_h, egui::Stroke::new(1.0, faint));
        if body_col > 0 && cb.monospace {
            let bx = x0 + body_col as f32 * char_w;
            painter.vline(
                bx,
                top + row_h..=te.text_clip_rect.bottom(),
                egui::Stroke::new(1.0, faint.gamma_multiply(0.5)),
            );
        }
    }

    // ---- options + commit button
    let merging = snap.refs.state == RepoState::Merging;
    let push_target = push_target(snap);
    ui.horizontal(|ui| {
        let was_amend = cb.amend;
        ui.checkbox(&mut cb.amend, "Amend")
            .on_hover_text("Amend the last commit");
        if cb.amend != was_amend {
            if cb.amend {
                if cb.message.trim().is_empty() {
                    let repo = session.repo().clone();
                    cb.load_async(ui.ctx(), move || repo.head_message().ok());
                }
            } else if cb.loaded_head.as_deref().is_some_and(|h| h == cb.message) {
                cb.message.clear();
                cb.loaded_head = None;
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_msg = !cb.message.trim().is_empty();
            let enabled = cb.running.is_none()
                && (staged > 0 || cb.amend || merging)
                && (has_msg || cb.amend);
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
            message: cb.message.clone(),
            amend: cb.amend,
            signoff: cb.signoff.unwrap_or(false),
            sign: shared.settings.commit.sign,
            ..Default::default()
        };
        let push = push_target
            .filter(|_| cb.push == Some(true))
            .map(|(_, o)| o);
        let flag = Arc::new(AtomicU8::new(PENDING));
        cb.running = Some((flag.clone(), cb.message.clone()));
        let label = if cb.amend { "Amend commit" } else { "Commit" };
        session.run_op(label, move |repo, cancel, progress| {
            match repo.commit(&opts) {
                Ok(out) => {
                    flag.store(DONE, Ordering::SeqCst);
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
}
