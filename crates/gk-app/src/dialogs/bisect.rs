//! `git bisect`: start a session between a bad and a good revision, then mark
//! commits good / bad / skip until git names the first bad commit.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Color32, Context, RichText};
use gk_core::advanced::BisectMark;
use gk_core::{RepoState, Snapshot};

const GOOD: Color32 = Color32::from_rgb(0x3F, 0xA3, 0x4D);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Good,
    Bad,
    Skip,
}

/// One decision of `git bisect log` ("# good: [id] summary").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub kind: EntryKind,
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BisectLog {
    pub entries: Vec<LogEntry>,
    /// (id, summary) once git has found the culprit.
    pub first_bad: Option<(String, String)>,
}

/// "[id] summary" → (id, summary).
fn bracket_id(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim().strip_prefix('[')?;
    let (id, summary) = rest.split_once(']')?;
    let id = id.trim();
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| (id.to_string(), summary.trim().to_string()))
}

/// Parses the comment lines of `git bisect log`.
pub fn parse_bisect_log(log: &str) -> BisectLog {
    let mut out = BisectLog::default();
    for line in log.lines() {
        let Some(c) = line.strip_prefix('#') else {
            continue;
        };
        let Some((key, rest)) = c.trim_start().split_once(':') else {
            continue;
        };
        let kind = match key.trim() {
            "good" => EntryKind::Good,
            "bad" => EntryKind::Bad,
            "skip" => EntryKind::Skip,
            "first bad commit" => {
                out.first_bad = bracket_id(rest);
                continue;
            }
            // Custom terms (`--term-old/--term-new`) are rare; ignore them.
            _ => continue,
        };
        if let Some((id, summary)) = bracket_id(rest) {
            out.entries.push(LogEntry { kind, id, summary });
        }
    }
    out
}

/// The commit id from "<id> is the first bad commit" in bisect output.
pub fn parse_first_bad(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let id = l.trim().strip_suffix(" is the first bad commit")?;
        (id.len() >= 7 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_string())
    })
}

/// "Bisecting: 3 revisions left to test after this (roughly 2 steps)".
pub fn parse_remaining(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Bisecting: ").map(|s| s.to_string()))
}

enum Pending {
    Start,
    Mark,
}

pub struct Bisect {
    bad: String,
    good: String,
    bad_label: RevLabel,
    good_label: RevLabel,
    head_label: RevLabel,
    log: Option<Task<Result<String, String>>>,
    parsed: Option<Result<BisectLog, String>>,
    /// HEAD the log was loaded for.
    log_head: Option<String>,
    pending: Option<(Pending, TrackedOp<String>)>,
    output: String,
    error: Option<String>,
}

impl Bisect {
    pub fn new(_snap: &Snapshot) -> Self {
        Self {
            bad: "HEAD".into(),
            good: String::new(),
            bad_label: RevLabel::default(),
            good_label: RevLabel::default(),
            head_label: RevLabel::default(),
            log: None,
            parsed: None,
            log_head: None,
            pending: None,
            output: String::new(),
            error: None,
        }
    }

    fn load_log(&mut self, ctx: &Context, tab: &RepoTab, snap: &Snapshot) {
        let repo = tab.session.repo().clone();
        self.log_head = snap.refs.head_id.clone();
        self.log = Some(Task::spawn(ctx, move || {
            repo.git()
                .run(&["bisect", "log"])
                .map(|o| o.stdout_str())
                .map_err(|e| format!("{e:#}"))
        }));
    }

    fn mark(&mut self, tab: &RepoTab, mark: BisectMark) {
        let word = match mark {
            BisectMark::Good => "good",
            BisectMark::Bad => "bad",
            BisectMark::Skip => "skip",
        };
        self.error = None;
        let op = TrackedOp::queue(tab, format!("Bisect: mark {word}"), move |repo, _, _| {
            repo.bisect_mark(mark, None)
        });
        self.pending = Some((Pending::Mark, op));
    }

    fn poll(&mut self, ctx: &Context, tab: &RepoTab, snap: &Snapshot) {
        if let Some((kind, op)) = &mut self.pending
            && let Some(r) = op.take()
        {
            match r {
                Ok(text) => {
                    if matches!(kind, Pending::Mark) || !text.trim().is_empty() {
                        self.output = text;
                    }
                }
                Err(e) => self.error = Some(e),
            }
            self.pending = None;
            self.load_log(ctx, tab, snap);
        }
        if let Some(t) = &mut self.log
            && let Some(r) = t.take()
        {
            self.parsed = Some(r.map(|l| parse_bisect_log(&l)));
            self.log = None;
        }
        let bisecting = snap.refs.state == RepoState::Bisecting;
        if bisecting
            && self.log.is_none()
            && (self.parsed.is_none() || self.log_head != snap.refs.head_id)
        {
            self.load_log(ctx, tab, snap);
        }
    }

    fn start_form(&mut self, ui: &mut egui::Ui, ctl: &mut Ctl, tab: &RepoTab, snap: &Snapshot) {
        ui.label("Find the commit that introduced a bug by binary search between a bad and a good revision.");
        ui.add_space(4.0);
        egui::Grid::new("bisect-start")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Bad (has the bug):");
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.bad).desired_width(260.0));
                    ctl.focus(&r);
                    rev_combo(ui, "bisect-bad", &mut self.bad, snap, Some("HEAD"), 140.0);
                });
                ui.end_row();
                ui.label("");
                hint(
                    ui,
                    self.bad_label.get(tab, snap, self.bad.trim()).to_string(),
                );
                ui.end_row();
                ui.label("Good (no bug):");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.good)
                            .hint_text("commit id, branch or tag")
                            .desired_width(260.0),
                    );
                    rev_combo(ui, "bisect-good", &mut self.good, snap, None, 140.0);
                });
                ui.end_row();
                ui.label("");
                hint(
                    ui,
                    self.good_label.get(tab, snap, self.good.trim()).to_string(),
                );
                ui.end_row();
            });
        if worktree_dirty(snap) {
            warning(
                ui,
                "The working copy has uncommitted changes; bisect checks out other commits.",
            );
        }
        if snap.refs.state != RepoState::Clean {
            warning(
                ui,
                format!("An operation is in progress ({}).", snap.refs.state.label()),
            );
        }
        let (bad, good) = (self.bad.trim(), self.good.trim());
        if bad.is_empty() || good.is_empty() {
            ctl.invalid();
        } else if bad == good {
            error(ui, "Good and bad must be different revisions.");
            ctl.invalid();
        }
    }

    fn session_ui(
        &mut self,
        ui: &mut egui::Ui,
        tab: &RepoTab,
        snap: &Snapshot,
        busy: bool,
    ) -> Option<BisectMark> {
        let mut clicked = None;
        let parsed = self.parsed.as_ref().and_then(|p| p.as_ref().ok());
        let first_bad = parsed
            .and_then(|p| p.first_bad.clone())
            .or_else(|| parse_first_bad(&self.output).map(|id| (id, String::new())));
        if let Some((id, summary)) = &first_bad {
            let summary = if summary.is_empty() {
                describe_rev(tab, snap, id)
            } else {
                format!("{}  {summary}", crate::util::short_id(id))
            };
            egui::Frame::new()
                .fill(DANGER.gamma_multiply(0.18))
                .stroke((1.0, DANGER))
                .inner_margin(10)
                .corner_radius(4)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("First bad commit found")
                            .strong()
                            .size(16.0)
                            .color(DANGER),
                    );
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&summary).monospace().strong());
                        if ui.small_button("Copy id").clicked() {
                            ui.ctx().copy_text(id.clone());
                        }
                    });
                    hint(
                        ui,
                        "Reset to end the bisect session and return to the original branch.",
                    );
                });
            ui.add_space(6.0);
        }
        let head = snap.refs.head_id.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Current commit:");
            let text = if head.is_empty() {
                "(unknown)".to_string()
            } else {
                self.head_label.get(tab, snap, &head).to_string()
            };
            ui.label(RichText::new(text).monospace().strong());
        });
        if let Some(r) = parse_remaining(&self.output) {
            hint(ui, r);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let b = |t: &str, c: Color32| {
                    egui::Button::new(RichText::new(t).strong().color(Color32::WHITE))
                        .fill(c)
                        .min_size(egui::vec2(90.0, 28.0))
                };
                if ui
                    .add(b("✔ Good", GOOD))
                    .on_hover_text("This commit does not have the bug")
                    .clicked()
                {
                    clicked = Some(BisectMark::Good);
                }
                if ui
                    .add(b("✖ Bad", DANGER))
                    .on_hover_text("This commit has the bug")
                    .clicked()
                {
                    clicked = Some(BisectMark::Bad);
                }
                if ui
                    .add(egui::Button::new("Skip").min_size(egui::vec2(90.0, 28.0)))
                    .on_hover_text("This commit cannot be tested")
                    .clicked()
                {
                    clicked = Some(BisectMark::Skip);
                }
            });
            if busy {
                ui.spinner();
            }
        });
        if worktree_dirty(snap) {
            warning(
                ui,
                "Uncommitted changes may prevent checking out the next commit.",
            );
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Bisect log").strong());
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(6)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match &self.parsed {
                        None => {
                            ui.spinner();
                        }
                        Some(Err(e)) => error(ui, e.clone()),
                        Some(Ok(log)) if log.entries.is_empty() => {
                            ui.label(RichText::new("No commits marked yet.").weak());
                        }
                        Some(Ok(log)) => {
                            for e in &log.entries {
                                let (word, color) = match e.kind {
                                    EntryKind::Good => ("good", GOOD),
                                    EntryKind::Bad => ("bad ", DANGER),
                                    EntryKind::Skip => ("skip", ui.visuals().weak_text_color()),
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(word).monospace().color(color));
                                    ui.label(
                                        RichText::new(crate::util::short_id(&e.id)).monospace(),
                                    );
                                    ui.label(&e.summary);
                                });
                            }
                        }
                    });
            });
        if !self.output.trim().is_empty() {
            egui::CollapsingHeader::new("git output")
                .id_salt("bisect-output")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(self.output.trim_end()).monospace())
                                    .selectable(true),
                            );
                        });
                });
        }
        clicked
    }
}

impl TabDialog for Bisect {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        self.poll(ctx, tab, snap);
        let bisecting = snap.refs.state == RepoState::Bisecting;
        let busy = self.pending.is_some();
        let mut mark = None;
        let mut reset = false;
        let (out, ()) = modal(ctx, "bisect", "Bisect", 600.0, "Start bisect", |ui, ctl| {
            if bisecting {
                ctl.show_ok = false;
                ctl.enter_ok = false;
                ctl.cancel_label = "Close";
                mark = self.session_ui(ui, tab, snap, busy);
            } else {
                self.start_form(ui, ctl, tab, snap);
                if busy {
                    ctl.invalid();
                    op_progress(ui, snap);
                }
            }
            if let Some(e) = &self.error {
                error(ui, e.clone());
            }
            if bisecting {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Reset"))
                        .on_hover_text("End bisecting and check out the original branch")
                        .clicked()
                    {
                        reset = true;
                    }
                    hint(ui, "git bisect reset");
                });
            }
        });
        if let Some(m) = mark {
            self.mark(tab, m);
        }
        if reset {
            tab.session.run_op("Bisect reset", |repo, _, _| {
                repo.bisect_reset().map(|_| String::new())
            });
            return false;
        }
        match out {
            Outcome::Cancel => false,
            Outcome::Ok if !bisecting && !busy => {
                let bad = self.bad.trim().to_string();
                let good = self.good.trim().to_string();
                self.error = None;
                self.output.clear();
                self.parsed = None;
                let op = TrackedOp::queue(tab, "Bisect start", move |repo, _, _| {
                    repo.bisect_start(&bad, &good)
                });
                self.pending = Some((Pending::Start, op));
                true
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn log_parsing() {
        let log = format!(
            "git bisect start 'HEAD' 'v1.0'\n# status: waiting for good commit(s), bad commit known\n# bad: [{A}] Break things\ngit bisect bad {A}\n# good: [{B}] Release 1.0\ngit bisect good {B}\n# skip: [{B}] Release 1.0\n# first bad commit: [{A}] Break things\n"
        );
        let l = parse_bisect_log(&log);
        assert_eq!(l.entries.len(), 3);
        assert_eq!(
            l.entries[0],
            LogEntry {
                kind: EntryKind::Bad,
                id: A.into(),
                summary: "Break things".into()
            }
        );
        assert_eq!(l.entries[1].kind, EntryKind::Good);
        assert_eq!(l.entries[2].kind, EntryKind::Skip);
        assert_eq!(
            l.first_bad,
            Some((A.to_string(), "Break things".to_string()))
        );
        assert_eq!(parse_bisect_log("git bisect start\n").first_bad, None);
    }

    #[test]
    fn output_parsing() {
        let out = format!("{A} is the first bad commit\ncommit {A}\nAuthor: X <x@y>\n");
        assert_eq!(parse_first_bad(&out).as_deref(), Some(A));
        let out =
            "Bisecting: 3 revisions left to test after this (roughly 2 steps)\n[abc] Subject\n";
        assert_eq!(parse_first_bad(out), None);
        assert_eq!(
            parse_remaining(out).as_deref(),
            Some("3 revisions left to test after this (roughly 2 steps)")
        );
    }
}
