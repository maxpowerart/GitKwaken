//! Git Flow (built in, no git-flow extension needed): initialize the branch
//! model, start feature / release / hotfix branches and finish them.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;
use gk_core::advanced::{FlowConfig, FlowKind};

/// Production branch for a fresh flow setup: "main" or "master", whichever
/// exists (main wins), else the current branch, else "main".
pub fn default_master(branches: &[&str], current: Option<&str>) -> String {
    for name in ["main", "master"] {
        if branches.contains(&name) {
            return name.to_string();
        }
    }
    current.unwrap_or("main").to_string()
}

pub fn kind_name(k: FlowKind) -> &'static str {
    match k {
        FlowKind::Feature => "feature",
        FlowKind::Release => "release",
        FlowKind::Hotfix => "hotfix",
    }
}

/// Branch a new flow branch starts from by default.
pub fn default_base(cfg: &FlowConfig, k: FlowKind) -> &str {
    match k {
        FlowKind::Hotfix => &cfg.master,
        _ => &cfg.develop,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Start(FlowKind),
    Finish,
}

pub struct GitFlow {
    load: Option<Task<Option<FlowConfig>>>,
    /// None while loading; Some(None) when not initialized.
    cfg: Option<Option<FlowConfig>>,
    /// Init/settings form, shown when not initialized or when editing.
    form: Option<FlowConfig>,
    default_master: String,
    action: Action,
    name: String,
    base: String,
    keep: bool,
    tag_message: String,
    pending: Option<(FlowConfig, TrackedOp<()>)>,
    error: Option<String>,
}

impl GitFlow {
    pub fn new(snap: &Snapshot) -> Self {
        let names: Vec<&str> = snap.refs.branches.iter().map(|b| b.name.as_str()).collect();
        Self {
            load: None,
            cfg: None,
            form: None,
            default_master: default_master(&names, snap.refs.head.branch()),
            action: Action::Start(FlowKind::Feature),
            name: String::new(),
            base: String::new(),
            keep: false,
            tag_message: String::new(),
            pending: None,
            error: None,
        }
    }

    fn set_action(&mut self, a: Action, cfg: &FlowConfig) {
        self.action = a;
        if let Action::Start(k) = a {
            self.base = default_base(cfg, k).to_string();
        }
    }

    fn init_form(&mut self, ui: &mut egui::Ui, ctl: &mut Ctl, snap: &Snapshot, editing: bool) {
        let Some(f) = &mut self.form else { return };
        if !editing {
            ui.label("Git Flow is not set up in this repository. Choose the branch names and prefixes to use:");
            ui.add_space(4.0);
        }
        egui::Grid::new("flow-init").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new("Branches").strong());
            ui.end_row();
            ui.label("Production releases:");
            ui.add(egui::TextEdit::singleline(&mut f.master).desired_width(240.0));
            ui.end_row();
            ui.label("Next release development:");
            ui.add(egui::TextEdit::singleline(&mut f.develop).desired_width(240.0));
            ui.end_row();
            ui.label(RichText::new("Prefixes").strong());
            ui.end_row();
            ui.label("Feature:");
            ui.add(egui::TextEdit::singleline(&mut f.feature).desired_width(240.0));
            ui.end_row();
            ui.label("Release:");
            ui.add(egui::TextEdit::singleline(&mut f.release).desired_width(240.0));
            ui.end_row();
            ui.label("Hotfix:");
            ui.add(egui::TextEdit::singleline(&mut f.hotfix).desired_width(240.0));
            ui.end_row();
            ui.label("Version tag:");
            ui.add(egui::TextEdit::singleline(&mut f.version_tag).hint_text("e.g. v").desired_width(240.0));
            ui.end_row();
        });
        let master = f.master.trim();
        let develop = f.develop.trim();
        let mut problem = None;
        for (what, b) in [("Production branch", master), ("Development branch", develop)] {
            if let Err(e) = check_branch_name(b) {
                problem = Some(format!("{what}: {e}"));
            }
        }
        if problem.is_none() && master == develop {
            problem = Some("Production and development branches must differ.".into());
        }
        if problem.is_none() && snap.refs.branch(master).is_none() {
            problem = Some(format!("Branch '{master}' does not exist."));
        }
        if problem.is_none() && [&f.feature, &f.release, &f.hotfix].iter().any(|p| p.trim().is_empty()) {
            problem = Some("Prefixes cannot be empty.".into());
        }
        match problem {
            Some(p) => {
                error(ui, p);
                ctl.invalid();
            }
            None if snap.refs.branch(develop).is_none() => hint(ui, format!("'{develop}' will be created from '{master}'.")),
            None => {}
        }
    }

    /// Start / finish UI; returns true when the settings form was requested.
    fn actions(&mut self, ui: &mut egui::Ui, ctl: &mut Ctl, snap: &Snapshot, cfg: &FlowConfig) -> bool {
        let current = snap.refs.head.branch().unwrap_or_default().to_string();
        let finishable = cfg.classify(&current);
        let mut settings = false;
        ui.horizontal(|ui| {
            for k in [FlowKind::Feature, FlowKind::Release, FlowKind::Hotfix] {
                let a = Action::Start(k);
                if ui.selectable_label(self.action == a, format!("Start {}", kind_name(k))).clicked() {
                    self.set_action(a, cfg);
                }
            }
            let label = match finishable {
                Some((k, _)) => format!("Finish {} ", kind_name(k)),
                None => "Finish".to_string(),
            };
            let r = ui.add_enabled(finishable.is_some(), egui::Button::selectable(self.action == Action::Finish, label));
            let r = r.on_disabled_hover_text(format!(
                "Check out a {}…, {}… or {}… branch to finish it",
                cfg.feature, cfg.release, cfg.hotfix
            ));
            if r.clicked() {
                self.set_action(Action::Finish, cfg);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Settings…").clicked() {
                    settings = true;
                }
            });
        });
        ui.separator();
        match self.action {
            Action::Start(k) => {
                let prefix = cfg.prefix(k).to_string();
                ui.horizontal(|ui| {
                    ui.label(format!("{} name:", kind_name(k)));
                    ui.label(RichText::new(&prefix).monospace().weak());
                    let hint_text = if k == FlowKind::Feature { "my-feature" } else { "1.2.0" };
                    let r = ui.add(egui::TextEdit::singleline(&mut self.name).hint_text(hint_text).desired_width(260.0));
                    ctl.focus(&r);
                });
                ui.horizontal(|ui| {
                    ui.label("Start at:");
                    ui.add(egui::TextEdit::singleline(&mut self.base).desired_width(200.0));
                    rev_combo(ui, "flow-base", &mut self.base, snap, Some(default_base(cfg, k)), 140.0);
                });
                let name = self.name.trim();
                let full = format!("{prefix}{name}");
                if name.is_empty() || self.base.trim().is_empty() {
                    ctl.invalid();
                } else if let Err(e) = check_new_branch(snap, &full) {
                    error(ui, e);
                    ctl.invalid();
                } else {
                    hint(ui, format!("Creates and checks out '{full}' from '{}'.", self.base.trim()));
                }
                if resolve_rev(snap, self.base.trim()).is_none() && snap.refs.branch(self.base.trim()).is_none() {
                    warning(ui, format!("'{}' is not a known branch, tag or commit.", self.base.trim()));
                }
                if worktree_dirty(snap) {
                    hint(ui, "Uncommitted changes are carried over to the new branch.");
                }
                ctl.ok_label = format!("Start {}", kind_name(k));
            }
            Action::Finish => {
                let Some((k, name)) = finishable else {
                    ctl.invalid();
                    return settings;
                };
                ui.label(RichText::new(format!("Finish {} '{name}'", kind_name(k))).strong());
                match k {
                    FlowKind::Feature => {
                        hint(ui, format!("Merges '{current}' into '{}' (--no-ff).", cfg.develop));
                    }
                    FlowKind::Release | FlowKind::Hotfix => {
                        let tag = format!("{}{name}", cfg.version_tag);
                        hint(ui, format!("Merges '{current}' into '{}', tags it '{tag}', then merges into '{}' (--no-ff).", cfg.master, cfg.develop));
                        ui.label("Tag message:");
                        let r = ui.add(egui::TextEdit::multiline(&mut self.tag_message).hint_text(tag.as_str()).desired_rows(2).desired_width(f32::INFINITY));
                        ctl.multiline(&r);
                        if snap.refs.tags.iter().any(|t| t.name == tag) {
                            error(ui, format!("Tag '{tag}' already exists."));
                            ctl.invalid();
                        }
                    }
                }
                ui.checkbox(&mut self.keep, "Keep the branch after finishing");
                if worktree_dirty(snap) {
                    warning(ui, "Commit or stash your changes first: finishing checks out other branches.");
                    ctl.invalid();
                }
                ctl.ok_label = format!("Finish {}", kind_name(k));
            }
        }
        settings
    }
}

impl TabDialog for GitFlow {
    fn show(&mut self, ctx: &Context, tab: &mut RepoTab, snap: &Snapshot, _shared: &mut Shared) -> bool {
        if self.cfg.is_none() && self.load.is_none() {
            let repo = tab.session.repo().clone();
            self.load = Some(Task::spawn(ctx, move || repo.flow_config()));
        }
        if let Some(t) = &mut self.load
            && let Some(c) = t.take()
        {
            self.load = None;
            if c.is_none() {
                self.form = Some(FlowConfig { master: self.default_master.clone(), ..FlowConfig::default() });
            } else if let Some(cfg) = &c {
                let current = snap.refs.head.branch().unwrap_or_default();
                let a = if cfg.classify(current).is_some() { Action::Finish } else { Action::Start(FlowKind::Feature) };
                self.set_action(a, cfg);
            }
            self.cfg = Some(c);
        }
        if let Some((cfg, op)) = &mut self.pending
            && let Some(r) = op.take()
        {
            match r {
                Ok(()) => {
                    let cfg = cfg.clone();
                    self.set_action(Action::Start(FlowKind::Feature), &cfg);
                    self.cfg = Some(Some(cfg));
                    self.form = None;
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
            self.pending = None;
        }
        let busy = self.pending.is_some();
        let cfg = self.cfg.clone();
        let mut open_settings = false;
        let mut cancel_settings = false;
        let (out, ()) = modal(ctx, "git-flow", "Git Flow", 560.0, "OK", |ui, ctl| {
            match &cfg {
                None => {
                    ctl.invalid();
                    ui.spinner();
                }
                Some(c) if self.form.is_some() => {
                    self.init_form(ui, ctl, snap, c.is_some());
                    ctl.ok_label = if c.is_some() { "Save".into() } else { "Initialize".into() };
                    if c.is_some() && ui.small_button("◀ Back").clicked() {
                        cancel_settings = true;
                    }
                }
                Some(Some(c)) => open_settings = self.actions(ui, ctl, snap, c),
                Some(None) => ctl.invalid(),
            }
            if busy {
                ctl.invalid();
                op_progress(ui, snap);
            }
            if let Some(e) = &self.error {
                error(ui, e.clone());
            }
        });
        if open_settings
            && let Some(Some(c)) = &self.cfg
        {
            self.form = Some(c.clone());
        }
        if cancel_settings {
            self.form = None;
        }
        match out {
            Outcome::None => true,
            Outcome::Cancel => false,
            Outcome::Ok => {
                let Some(c) = cfg else { return true };
                if let Some(f) = &self.form {
                    let f = FlowConfig {
                        master: f.master.trim().into(),
                        develop: f.develop.trim().into(),
                        feature: f.feature.trim().into(),
                        release: f.release.trim().into(),
                        hotfix: f.hotfix.trim().into(),
                        version_tag: f.version_tag.trim().into(),
                    };
                    let f2 = f.clone();
                    self.error = None;
                    let op = TrackedOp::queue(tab, "Git Flow init", move |repo, _, _| repo.flow_init(&f2));
                    self.pending = Some((f, op));
                    return true;
                }
                let Some(c) = c else { return true };
                match self.action {
                    Action::Start(k) => {
                        let name = self.name.trim().to_string();
                        let base = self.base.trim().to_string();
                        let label = format!("Start {} {name}", kind_name(k));
                        tab.session.run_op(label, move |repo, _, _| {
                            repo.flow_start(&c, k, &name, Some(&base))?;
                            Ok(String::new())
                        });
                    }
                    Action::Finish => {
                        let current = snap.refs.head.branch().unwrap_or_default().to_string();
                        let Some((k, name)) = c.classify(&current).map(|(k, n)| (k, n.to_string())) else { return true };
                        let msg = self.tag_message.trim().to_string();
                        let keep = self.keep;
                        let label = format!("Finish {} {name}", kind_name(k));
                        tab.session.run_op(label, move |repo, _, _| {
                            let msg = (!msg.is_empty()).then_some(msg.as_str());
                            repo.flow_finish(&c, k, &name, msg, keep)
                        });
                    }
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_choice() {
        assert_eq!(default_master(&["dev", "master"], Some("dev")), "master");
        assert_eq!(default_master(&["master", "main"], None), "main");
        assert_eq!(default_master(&["trunk"], Some("trunk")), "trunk");
        assert_eq!(default_master(&[], None), "main");
    }

    #[test]
    fn bases() {
        let c = FlowConfig { master: "master".into(), ..FlowConfig::default() };
        assert_eq!(default_base(&c, FlowKind::Hotfix), "master");
        assert_eq!(default_base(&c, FlowKind::Feature), "develop");
        assert_eq!(default_base(&c, FlowKind::Release), "develop");
    }
}
