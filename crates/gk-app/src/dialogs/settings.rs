//! Application settings editor.

use super::AppDialog;
use super::common::*;
use crate::app::{Action, Shared};
use crate::keymap::{self, Act, Keymap, Shortcut};
use egui::{Context, RichText, Ui};
use gk_settings::{ExternalTool, Settings, StatusEngine, Theme};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Commit,
    Diff,
    History,
    Tools,
    Confirmations,
    Identity,
    Keyboard,
    Ssh,
}

const SECTIONS: [(Section, &str); 9] = [
    (Section::General, "General"),
    (Section::Commit, "Commit"),
    (Section::Diff, "Diff"),
    (Section::History, "History"),
    (Section::Tools, "External tools"),
    (Section::Confirmations, "Confirmations"),
    (Section::Identity, "Git identity"),
    (Section::Keyboard, "Keyboard"),
    // Last, so the other entries keep their positions.
    (Section::Ssh, "SSH"),
];

/// Monospace families offered in the font combo (any installed name can be typed).
const MONO_FONTS: [&str; 7] = [
    "Consolas",
    "Cascadia Mono",
    "Cascadia Code",
    "JetBrains Mono",
    "Fira Code",
    "Lucida Console",
    "Courier New",
];

#[derive(Clone, Default, PartialEq, Eq)]
struct Identity {
    name: String,
    email: String,
}

/// `core.sshCommand` in the global config and in the open repository.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct SshCommands {
    global: String,
    repo: String,
}

/// One git config value to write (`repo`: `--local` there, else `--global`);
/// an empty value unsets the key.
#[derive(Debug, PartialEq, Eq)]
struct ConfigChange {
    repo: Option<PathBuf>,
    key: &'static str,
    value: String,
}

/// The git config writes a Save needs: (loaded, edited) pairs of the
/// identity and the ssh commands; unloaded ones are skipped.
fn config_changes(
    identity: Option<(&Identity, &Identity)>,
    ssh: Option<(&SshCommands, &SshCommands)>,
    repo: Option<&Path>,
) -> Vec<ConfigChange> {
    let mut out = Vec::new();
    let mut add = |repo: Option<&Path>, key: &'static str, old: &str, new: &str| {
        if old.trim() != new.trim() {
            out.push(ConfigChange {
                repo: repo.map(Path::to_path_buf),
                key,
                value: new.trim().to_string(),
            });
        }
    };
    if let Some((old, new)) = identity {
        add(None, "user.name", &old.name, &new.name);
        add(None, "user.email", &old.email, &new.email);
    }
    if let Some((old, new)) = ssh {
        add(None, "core.sshCommand", &old.global, &new.global);
        if let Some(r) = repo {
            add(Some(r), "core.sshCommand", &old.repo, &new.repo);
        }
    }
    out
}

/// A `core.sshCommand` text field with presets.
fn ssh_field(ui: &mut Ui, value: &mut String) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(300.0)
                .hint_text("default (Git's ssh)"),
        );
        if ui
            .small_button("Windows OpenSSH")
            .on_hover_text(super::diagnostics::WINDOWS_SSH)
            .clicked()
        {
            *value = super::diagnostics::WINDOWS_SSH.to_string();
        }
        if ui.small_button("Default").clicked() {
            value.clear();
        }
    });
}

pub struct SettingsDialog {
    section: Section,
    draft: Option<Settings>,
    diff_tool: ExternalTool,
    merge_tool: ExternalTool,
    terminal: ExternalTool,
    git_path: String,
    identity_task: Option<Task<Identity>>,
    identity_loaded: Option<Identity>,
    identity: Identity,
    saving: Option<Task<Result<(), String>>>,
    save_error: Option<String>,
    /// Keymap being edited (one entry per action).
    keys: Vec<(Act, Option<Shortcut>)>,
    /// Action waiting for a key press.
    recording: Option<Act>,
    record_error: Option<String>,
    /// Repository of the active tab (repository-level `core.sshCommand`).
    repo: Option<PathBuf>,
    ssh_task: Option<Task<SshCommands>>,
    ssh_loaded: Option<SshCommands>,
    ssh: SshCommands,
    /// Styles offered under General › Style (re-read when the dialog opens).
    styles: Vec<gk_settings::style::Listed>,
    style_error: Option<String>,
}

impl SettingsDialog {
    pub fn new() -> Self {
        Self {
            section: Section::General,
            draft: None,
            diff_tool: ExternalTool::default(),
            merge_tool: ExternalTool::default(),
            terminal: ExternalTool::default(),
            git_path: String::new(),
            identity_task: None,
            identity_loaded: None,
            identity: Identity::default(),
            saving: None,
            save_error: None,
            keys: Vec::new(),
            recording: None,
            record_error: None,
            repo: None,
            ssh_task: None,
            ssh_loaded: None,
            ssh: SshCommands::default(),
            styles: Vec::new(),
            style_error: None,
        }
    }

    /// Opens on the section whose name contains `section` (case-insensitive).
    pub fn at(section: &str) -> Self {
        let mut d = Self::new();
        let want = section.trim().to_lowercase();
        if let Some((s, _)) = SECTIONS
            .iter()
            .find(|(_, n)| !want.is_empty() && n.to_lowercase().contains(&want))
        {
            d.section = *s;
        }
        d
    }

    fn init(&mut self, ctx: &Context, shared: &Shared) {
        let s = shared.settings.clone();
        self.diff_tool = s.external_diff_tool.clone().unwrap_or_default();
        self.merge_tool = s.external_merge_tool.clone().unwrap_or_default();
        self.terminal = s.terminal.clone().unwrap_or_default();
        self.git_path = s
            .git_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.keys = Keymap::from_overrides(&s.keymap).bindings().to_vec();
        self.styles = gk_settings::style::list();
        self.draft = Some(s);
        let log = shared.log.clone();
        self.identity_task = Some(Task::spawn(ctx, move || {
            let git = gk_cli::Git::new(std::env::temp_dir(), log);
            let get = |k: &str| {
                git.run_raw(&["config", "--global", "--get", k], None)
                    .ok()
                    .filter(|o| o.success())
                    .map(|o| o.stdout_str().trim().to_string())
                    .unwrap_or_default()
            };
            Identity {
                name: get("user.name"),
                email: get("user.email"),
            }
        }));
        let log = shared.log.clone();
        let repo = self.repo.clone();
        self.ssh_task = Some(Task::spawn(ctx, move || {
            let get = |cwd: PathBuf, scope: &str| {
                gk_cli::Git::new(cwd, log.clone())
                    .run_raw(&["config", scope, "--get", "core.sshCommand"], None)
                    .ok()
                    .filter(|o| o.success())
                    .map(|o| o.stdout_str().trim().to_string())
                    .unwrap_or_default()
            };
            SshCommands {
                global: get(std::env::temp_dir(), "--global"),
                repo: repo.map(|r| get(r, "--local")).unwrap_or_default(),
            }
        }));
    }

    /// Copies the edited fields (not window/tabs/bookmarks state) into `dst`.
    fn apply(&self, dst: &mut Settings) {
        let Some(src) = &self.draft else { return };
        dst.theme = src.theme;
        dst.style = src.style.clone();
        dst.ui_scale = src.ui_scale;
        dst.font_size = src.font_size;
        dst.mono_font_size = src.mono_font_size;
        dst.mono_font_family = src.mono_font_family.trim().to_string();
        dst.compact = src.compact;
        dst.keymap = keymap::to_overrides(&self.keys);
        dst.language = src.language.clone();
        dst.auto_fetch_minutes = src.auto_fetch_minutes;
        dst.update_url = src.update_url.trim().to_string();
        dst.check_updates = src.check_updates;
        dst.show_untracked = src.show_untracked;
        dst.staged_first = src.staged_first;
        dst.status_engine = src.status_engine;
        dst.commit = src.commit.clone();
        dst.diff = src.diff.clone();
        dst.history = src.history.clone();
        dst.confirmations = src.confirmations;
        let tool = |t: &ExternalTool| (!t.command.trim().is_empty()).then(|| t.clone());
        dst.external_diff_tool = tool(&self.diff_tool);
        dst.external_merge_tool = tool(&self.merge_tool);
        dst.terminal = tool(&self.terminal);
        let gp = self.git_path.trim();
        dst.git_path = (!gp.is_empty()).then(|| gp.into());
    }
}

impl SettingsDialog {
    /// While recording a shortcut, takes the next key press (before the
    /// modal sees Enter / Escape). Escape cancels.
    fn capture_key(&mut self, ctx: &Context) {
        let Some(act) = self.recording else { return };
        let pressed = ctx.input_mut(|i| {
            let mut got = None;
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if got.is_none() => {
                    got = Some((*key, *modifiers));
                    false
                }
                egui::Event::Key { .. } | egui::Event::Text(_) => false,
                _ => true,
            });
            got
        });
        let Some((key, mods)) = pressed else { return };
        if key == egui::Key::Escape && !mods.any() {
            self.recording = None;
            self.record_error = None;
            return;
        }
        let sc = Shortcut::from_event(key, mods);
        // Scoped shortcuts only fire while their list / diff has focus.
        if sc.is_typing_key() && keymap::scope_of(act) == keymap::Scope::Global {
            self.record_error = Some(format!(
                "{sc} would be typed into text fields: add Ctrl or Alt."
            ));
            return;
        }
        if let Some(b) = self.keys.iter_mut().find(|b| b.0 == act) {
            b.1 = Some(sc);
        }
        self.recording = None;
        self.record_error = None;
    }
}

/// Settings › Keyboard: one row per action; click a shortcut, press keys.
fn keyboard_section(
    ui: &mut Ui,
    keys: &mut [(Act, Option<Shortcut>)],
    recording: &mut Option<Act>,
    record_error: &Option<String>,
    ctl: &mut Ctl,
) {
    ui.label("Click a shortcut, then press the new key combination (Esc cancels).");
    ui.add_space(4.0);
    let conflicts = Keymap::from_overrides(&keymap::to_overrides(keys)).conflicts();
    let in_conflict = |a: Act| conflicts.iter().any(|c| c.0 == a || c.1 == a);
    egui::Grid::new("set-keys")
        .num_columns(4)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (act, sc) in keys.iter_mut() {
                ui.label(keymap::label_of(*act));
                let rec = *recording == Some(*act);
                let text = if rec {
                    "Press keys…".to_string()
                } else {
                    sc.map(|s| s.to_string()).unwrap_or_else(|| "—".into())
                };
                let mut rt = RichText::new(text).monospace();
                if in_conflict(*act) {
                    rt = rt.color(DANGER);
                }
                let b = ui
                    .add_sized([150.0, 20.0], egui::Button::new(rt).selected(rec))
                    .on_hover_text(format!("Settings id: {}", keymap::id_of(*act)));
                if b.clicked() {
                    *recording = if rec { None } else { Some(*act) };
                }
                let def = keymap::default_of(*act);
                if ui
                    .add_enabled(*sc != def, egui::Button::new("Reset").small())
                    .on_hover_text(def.map(|d| format!("Default: {d}")).unwrap_or_default())
                    .clicked()
                {
                    *sc = def;
                }
                if ui
                    .add_enabled(sc.is_some(), egui::Button::new("Clear").small())
                    .clicked()
                {
                    *sc = None;
                }
                ui.end_row();
            }
        });
    if let Some(e) = record_error {
        warning(ui, e.clone());
    }
    for (a, b, sc) in &conflicts {
        error(
            ui,
            format!(
                "{sc} is used by both \"{}\" and \"{}\".",
                keymap::label_of(*a),
                keymap::label_of(*b)
            ),
        );
    }
    if !conflicts.is_empty() {
        ctl.invalid();
    }
    if recording.is_some() {
        // Enter must not save while waiting for keys.
        ctl.enter_ok = false;
    }
}

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, add);
}

fn tool_editor(ui: &mut Ui, id: &str, t: &mut ExternalTool, help: &str) {
    grid(ui, id, |ui| {
        ui.label("Name:");
        ui.add(egui::TextEdit::singleline(&mut t.name).desired_width(320.0));
        ui.end_row();
        ui.label("Command:");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut t.command)
                    .desired_width(240.0)
                    .hint_text("program path"),
            );
            if ui.button("Browse…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("Programs", &["exe", "cmd", "bat"])
                    .pick_file()
            {
                t.command = p.display().to_string();
            }
        });
        ui.end_row();
        ui.label("Arguments:");
        ui.add(
            egui::TextEdit::singleline(&mut t.args)
                .desired_width(320.0)
                .font(egui::TextStyle::Monospace),
        );
        ui.end_row();
    });
    hint(ui, help);
}

impl AppDialog for SettingsDialog {
    fn set_repo(&mut self, repo: Option<PathBuf>) {
        self.repo = repo;
    }

    fn show(&mut self, ctx: &Context, shared: &mut Shared) -> bool {
        if self.draft.is_none() {
            self.init(ctx, shared);
        }
        if let Some(t) = &mut self.identity_task
            && let Some(id) = t.take()
        {
            self.identity = id.clone();
            self.identity_loaded = Some(id);
            self.identity_task = None;
        }
        if let Some(t) = &mut self.ssh_task
            && let Some(s) = t.take()
        {
            self.ssh = s.clone();
            self.ssh_loaded = Some(s);
            self.ssh_task = None;
        }
        if let Some(t) = &mut self.saving {
            match t.take() {
                Some(Ok(())) => return false,
                Some(Err(e)) => {
                    self.save_error = Some(e);
                    self.saving = None;
                }
                None => {}
            }
        }
        self.capture_key(ctx);
        let saving = self.saving.is_some();
        // Fixed-height pane (scrolls inside) so switching sections does not
        // resize the dialog; smaller on small windows.
        let pane_h = (ctx.content_rect().height() - 220.0).clamp(160.0, 440.0);
        let (out, ()) = modal(ctx, "settings", "Settings", 720.0, "Save", |ui, ctl| {
            let Some(d) = self.draft.as_mut() else { return };
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(150.0, pane_h),
                    egui::Layout::top_down_justified(egui::Align::LEFT),
                    |ui| {
                        ui.set_min_size(egui::vec2(150.0, pane_h));
                        for (s, name) in SECTIONS {
                            ui.selectable_value(&mut self.section, s, name);
                        }
                    },
                );
                // A vertical separator would take the full available height.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(9.0, pane_h), egui::Sense::hover());
                ui.painter().vline(
                    rect.center().x,
                    rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().id_salt("settings-pane").max_height(pane_h).min_scrolled_height(pane_h).auto_shrink([false, false]).show(ui, |ui| {
                        match self.section {
                            Section::General => {
                                grid(ui, "set-general", |ui| {
                                    ui.label("Theme:");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(&mut d.theme, Theme::Dark, "Dark");
                                        ui.radio_value(&mut d.theme, Theme::Light, "Light");
                                        ui.radio_value(&mut d.theme, Theme::System, "System");
                                    });
                                    ui.end_row();
                                    ui.label("Style:");
                                    ui.horizontal(|ui| {
                                        egui::ComboBox::from_id_salt("set-style").selected_text(d.style.as_str()).show_ui(ui, |ui| {
                                            for s in &self.styles {
                                                let label = if s.path.is_some() { format!("{}  (your style)", s.name) } else { s.name.clone() };
                                                match &s.error {
                                                    None => {
                                                        ui.selectable_value(&mut d.style, s.name.clone(), label);
                                                    }
                                                    Some(e) => {
                                                        ui.add_enabled(false, egui::Button::selectable(false, format!("{label}  (error)"))).on_disabled_hover_text(e.as_str());
                                                    }
                                                }
                                            }
                                        });
                                        if ui.add(crate::icons::IconButton::new(crate::icons::Icon::Add).text("New style from this one")).on_hover_text("Copies the selected style into your styles folder as a file to edit, and selects it").clicked() {
                                            let (style, _) = gk_settings::style::load(&d.style);
                                            match gk_settings::style::create_copy(&style) {
                                                Ok((path, name)) => {
                                                    d.style = name;
                                                    self.styles = gk_settings::style::list();
                                                    self.style_error = None;
                                                    let _ = open::that(&path);
                                                }
                                                Err(e) => self.style_error = Some(format!("{e:#}")),
                                            }
                                        }
                                        if ui.add(crate::icons::IconButton::new(crate::icons::Icon::Folder)).on_hover_text("Open the styles folder").clicked() {
                                            let dir = gk_settings::style::styles_dir();
                                            let _ = std::fs::create_dir_all(&dir);
                                            let _ = open::that(&dir);
                                        }
                                        if ui.add(crate::icons::IconButton::new(crate::icons::Icon::Refresh)).on_hover_text("Reload the style files (Save applies the changes)").clicked() {
                                            self.styles = gk_settings::style::list();
                                        }
                                    });
                                    ui.end_row();
                                    if let Some(e) = &self.style_error {
                                        ui.label("");
                                        ui.colored_label(ui.visuals().error_fg_color, e);
                                        ui.end_row();
                                    }
                                    ui.label("UI scale:");
                                    ui.add(egui::Slider::new(&mut d.ui_scale, 0.75..=2.5).step_by(0.05));
                                    ui.end_row();
                                    ui.label("Font size:");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::DragValue::new(&mut d.font_size).range(8.0..=32.0).speed(0.25));
                                        hint(ui, "menus, lists, toolbar, dialogs");
                                    });
                                    ui.end_row();
                                    ui.label("Code font size:");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::DragValue::new(&mut d.mono_font_size).range(8.0..=32.0).speed(0.25));
                                        hint(ui, "diffs, blame, console; also Ctrl+wheel over a diff");
                                    });
                                    ui.end_row();
                                    ui.label("Language:");
                                    egui::ComboBox::from_id_salt("set-lang").selected_text("English").show_ui(ui, |ui| {
                                        ui.selectable_value(&mut d.language, "en".to_string(), "English");
                                    });
                                    ui.end_row();
                                    ui.label("Auto fetch (minutes):");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::DragValue::new(&mut d.auto_fetch_minutes).range(0..=1440));
                                        hint(ui, "0 = off");
                                    });
                                    ui.end_row();
                                    ui.label("Untracked files:");
                                    ui.checkbox(&mut d.show_untracked, "Show untracked files");
                                    ui.end_row();
                                    ui.label("File Status lists:");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(&mut d.staged_first, true, "Staged on top");
                                        ui.radio_value(&mut d.staged_first, false, "Unstaged on top");
                                    });
                                    ui.end_row();
                                    ui.label("Status engine:");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(&mut d.status_engine, StatusEngine::Gix, "Built-in (gitoxide)");
                                        ui.radio_value(&mut d.status_engine, StatusEngine::Cli, "git status (CLI)");
                                    });
                                    ui.end_row();
                                    ui.label("Git executable:");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut self.git_path).desired_width(260.0).hint_text("auto-detect"));
                                        // The git binary has no extension outside Windows.
                                        let picker = if cfg!(windows) { rfd::FileDialog::new().add_filter("git", &["exe"]) } else { rfd::FileDialog::new() };
                                        if ui.button("Browse…").clicked()
                                            && let Some(p) = picker.pick_file()
                                        {
                                            self.git_path = p.display().to_string();
                                        }
                                    });
                                    ui.end_row();
                                    ui.label("Updates:");
                                    ui.checkbox(&mut d.check_updates, "Check for updates automatically");
                                    ui.end_row();
                                    ui.label("Update channel:");
                                    ui.add(egui::TextEdit::singleline(&mut d.update_url).desired_width(260.0).hint_text("default (GitHub releases)"));
                                    ui.end_row();
                                    ui.label("Monospace font:");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut d.mono_font_family).desired_width(180.0).hint_text("Consolas"));
                                        egui::ComboBox::from_id_salt("set-mono-font").selected_text("").width(24.0).show_ui(ui, |ui| {
                                            for f in MONO_FONTS {
                                                ui.selectable_value(&mut d.mono_font_family, f.to_string(), f);
                                            }
                                        });
                                    });
                                    ui.end_row();
                                    ui.label("Density:");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(&mut d.compact, true, "Compact");
                                        ui.radio_value(&mut d.compact, false, "Comfortable");
                                    });
                                    ui.end_row();
                                });
                                hint(ui, "Status settings apply to newly opened tabs; the git executable after a restart.");
                                let gp = self.git_path.trim();
                                if !gp.is_empty() && !std::path::Path::new(gp).is_file() {
                                    error(ui, "Git executable not found.");
                                    ctl.invalid();
                                }
                            }
                            Section::Commit => {
                                let c = &mut d.commit;
                                ui.checkbox(&mut c.signoff, "Add Signed-off-by line");
                                ui.horizontal(|ui| {
                                    ui.label("Sign commits:");
                                    ui.radio_value(&mut c.sign, None, "Follow git config");
                                    ui.radio_value(&mut c.sign, Some(true), "Always");
                                    ui.radio_value(&mut c.sign, Some(false), "Never");
                                });
                                grid(ui, "set-commit", |ui| {
                                    ui.label("Subject guide column:");
                                    ui.add(egui::DragValue::new(&mut c.subject_guide_column).range(0..=200));
                                    ui.end_row();
                                    ui.label("Body wrap column:");
                                    ui.add(egui::DragValue::new(&mut c.body_wrap_column).range(0..=200));
                                    ui.end_row();
                                });
                                ui.checkbox(&mut c.push_after_commit, "Push to the tracked branch after committing");
                            }
                            Section::Diff => {
                                let df = &mut d.diff;
                                grid(ui, "set-diff", |ui| {
                                    ui.label("Context lines:");
                                    ui.add(egui::DragValue::new(&mut df.context_lines).range(0..=100));
                                    ui.end_row();
                                    ui.label("Tab width:");
                                    ui.add(egui::DragValue::new(&mut df.tab_width).range(1..=16));
                                    ui.end_row();
                                    ui.label("Max inline file size (KB):");
                                    ui.add(egui::DragValue::new(&mut df.max_file_kb).range(16..=1_048_576).speed(64));
                                    ui.end_row();
                                });
                                ui.checkbox(&mut df.ignore_whitespace, "Ignore whitespace");
                                ui.checkbox(&mut df.word_diff, "Highlight changed words");
                                ui.checkbox(&mut df.side_by_side, "Side-by-side view");
                                ui.checkbox(&mut df.syntax_highlight, "Syntax highlighting");
                                ui.checkbox(&mut df.renames, "Detect renames in commit file lists");
                                hint(ui, "Files larger than the maximum size show a \"Show anyway\" button instead of the diff.");
                                ui.horizontal(|ui| {
                                    ui.label("Rename similarity:");
                                    ui.add(egui::Slider::new(&mut df.rename_similarity, 1..=100).suffix("%"));
                                });
                                hint(ui, "How similar a deleted and an added file must be to count as a rename, in File Status and in commit file lists (git's default: 50%).");
                            }
                            Section::History => {
                                let h = &mut d.history;
                                grid(ui, "set-history", |ui| {
                                    ui.label("Date format:");
                                    ui.add(egui::TextEdit::singleline(&mut h.date_format).desired_width(200.0).font(egui::TextStyle::Monospace));
                                    ui.end_row();
                                    ui.label("Row height:");
                                    ui.add(egui::DragValue::new(&mut h.row_height).range(14.0..=60.0).speed(0.5));
                                    ui.end_row();
                                    ui.label("Graph lane width:");
                                    ui.add(egui::DragValue::new(&mut h.graph_lane_width).range(6.0..=40.0).speed(0.5));
                                    ui.end_row();
                                });
                                hint(ui, "Date format: %Y %m %d %H %M %S (strftime).");
                                ui.checkbox(&mut h.relative_dates, "Relative dates (\"3 days ago\")");
                                ui.checkbox(&mut h.show_all_branches, "Show all branches by default");
                                ui.checkbox(&mut h.show_remote_branches, "Show remote branches");
                                ui.checkbox(&mut h.show_tags, "Show tags");
                                ui.checkbox(&mut h.show_stashes, "Show stashes");
                            }
                            Section::Tools => {
                                let help = "Arguments: $LOCAL, $REMOTE, $BASE and $MERGED are replaced with quoted file paths.";
                                ui.label(RichText::new("Diff tool").strong());
                                tool_editor(ui, "set-difftool", &mut self.diff_tool, help);
                                ui.separator();
                                ui.label(RichText::new("Merge tool").strong());
                                tool_editor(ui, "set-mergetool", &mut self.merge_tool, help);
                                ui.separator();
                                ui.label(RichText::new("Terminal").strong());
                                tool_editor(ui, "set-terminal", &mut self.terminal, "Arguments: $PWD is replaced with the repository folder. Empty = Git Bash / cmd.");
                            }
                            Section::Confirmations => {
                                let c = &mut d.confirmations;
                                ui.label("Ask for confirmation before:");
                                ui.checkbox(&mut c.discard, "Discarding changes");
                                ui.checkbox(&mut c.hard_reset, "Hard reset with uncommitted changes");
                                ui.checkbox(&mut c.force_push, "Force pushing");
                                ui.checkbox(&mut c.delete_branch, "Deleting branches");
                            }
                            Section::Identity => {
                                ui.label("Global git identity (~/.gitconfig), used for new commits:");
                                if self.identity_task.is_some() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label("Reading git config…");
                                    });
                                } else {
                                    grid(ui, "set-identity", |ui| {
                                        ui.label("Name (user.name):");
                                        ui.add(egui::TextEdit::singleline(&mut self.identity.name).desired_width(300.0));
                                        ui.end_row();
                                        ui.label("Email (user.email):");
                                        ui.add(egui::TextEdit::singleline(&mut self.identity.email).desired_width(300.0));
                                        ui.end_row();
                                    });
                                    hint(ui, "Written to the global git config only when you click Save.");
                                }
                            }
                            Section::Keyboard => keyboard_section(ui, &mut self.keys, &mut self.recording, &self.record_error, ctl),
                            Section::Ssh => {
                                ui.label("The ssh program git runs for ssh remotes (core.sshCommand):");
                                if self.ssh_task.is_some() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label("Reading git config…");
                                    });
                                } else {
                                    grid(ui, "set-ssh", |ui| {
                                        ui.label("Global (~/.gitconfig):");
                                        ssh_field(ui, &mut self.ssh.global);
                                        ui.end_row();
                                        if self.repo.is_some() {
                                            ui.label("This repository:");
                                            ssh_field(ui, &mut self.ssh.repo);
                                            ui.end_row();
                                        }
                                    });
                                    if let Some(r) = &self.repo {
                                        hint(ui, format!("The repository value ({}) overrides the global one.", r.display()));
                                    } else {
                                        hint(ui, "Open a repository to set a value for it only.");
                                    }
                                    hint(ui, "Empty uses git's default: on Windows, the OpenSSH bundled with Git (agent through SSH_AUTH_SOCK). Windows OpenSSH uses the Windows ssh-agent service. Written to git config only when you click Save.");
                                    hint(ui, "Agent, keys and a connection test: More ▸ Authentication… in a repository.");
                                }
                            }
                        }
                    });
                });
            });
            if let Some(e) = &self.save_error {
                error(ui, format!("Saving git config failed: {e}"));
            }
            if saving {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving…");
                });
                ctl.invalid();
            }
        });
        match out {
            Outcome::None => true,
            Outcome::Cancel => saving,
            Outcome::Ok => {
                self.apply(shared.settings);
                shared.actions.push(Action::ApplySettings);
                let changes = config_changes(
                    self.identity_loaded.as_ref().map(|l| (l, &self.identity)),
                    self.ssh_loaded.as_ref().map(|l| (l, &self.ssh)),
                    self.repo.as_deref(),
                );
                if changes.is_empty() {
                    return false;
                }
                let log = shared.log.clone();
                self.save_error = None;
                self.saving = Some(Task::spawn(ctx, move || {
                    for c in changes {
                        let (cwd, scope) = match &c.repo {
                            Some(r) => (r.clone(), "--local"),
                            None => (std::env::temp_dir(), "--global"),
                        };
                        let git = gk_cli::Git::new(cwd, log.clone());
                        let r = if c.value.is_empty() {
                            git.run(&["config", scope, "--unset", c.key])
                        } else {
                            git.run(&["config", scope, c.key, &c.value])
                        };
                        r.map_err(|e| format!("{}: {e:#}", c.key))?;
                    }
                    Ok(())
                }));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_changes_only_for_edited_values() {
        let id = Identity {
            name: "A".into(),
            email: "a@x".into(),
        };
        let ssh = SshCommands {
            global: String::new(),
            repo: "ssh -v".into(),
        };
        let repo = Path::new("C:/r");
        assert!(config_changes(Some((&id, &id)), Some((&ssh, &ssh)), Some(repo)).is_empty());
        assert!(config_changes(None, None, Some(repo)).is_empty());
        let id2 = Identity {
            name: " A ".into(),
            email: "b@x".into(),
        };
        let ssh2 = SshCommands {
            global: "C:/Windows/System32/OpenSSH/ssh.exe".into(),
            repo: String::new(),
        };
        let c = config_changes(Some((&id, &id2)), Some((&ssh, &ssh2)), Some(repo));
        assert_eq!(
            c,
            [
                ConfigChange {
                    repo: None,
                    key: "user.email",
                    value: "b@x".into()
                },
                ConfigChange {
                    repo: None,
                    key: "core.sshCommand",
                    value: "C:/Windows/System32/OpenSSH/ssh.exe".into()
                },
                ConfigChange {
                    repo: Some(repo.to_path_buf()),
                    key: "core.sshCommand",
                    value: String::new()
                },
            ]
        );
        // Without an open repository, the repository value is never written.
        assert_eq!(config_changes(None, Some((&ssh, &ssh2)), None).len(), 1);
    }
}
