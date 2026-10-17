//! Top-level application: tabs, global dialogs, askpass prompts, settings.

use crate::keymap::{self, Act, Keymap};
use crate::tab::RepoTab;
use crate::theme::{self, Palette};
use crate::{dialogs, views};
use gk_cli::CommandLog;
use gk_settings::{Autosave, Settings};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Requests views make of the application; handled after the frame's UI.
pub enum Action {
    OpenRepo(PathBuf),
    /// Asks for a folder, then opens it.
    OpenRepoDialog,
    CloseTab(usize),
    SelectTab(usize),
    Global(dialogs::GlobalDialog),
    /// Re-applies theme and fonts after settings changed.
    ApplySettings,
    /// Tracks a background clone (progress panel; opens the tab when done).
    StartClone(Box<dialogs::CloneJob>),
}

/// What views get besides their tab.
pub struct Shared<'a> {
    pub settings: &'a mut Settings,
    pub palette: Palette,
    pub log: &'a CommandLog,
    pub actions: &'a mut Vec<Action>,
    pub show_log: &'a mut bool,
}

pub struct GkApp {
    pub settings: Settings,
    autosave: Autosave<Settings>,
    pub tabs: Vec<RepoTab>,
    pub active: usize,
    pub log: CommandLog,
    pub askpass: Option<Arc<gk_askpass::Server>>,
    askpass_reply: String,
    palette: Palette,
    global: Option<dialogs::GlobalDialog>,
    actions: Vec<Action>,
    show_log: bool,
    errors: Vec<String>,
    command_palette: views::palette::PaletteState,
    updater: crate::update::Updater,
    /// Active shortcuts and the `settings.keymap` they were built from.
    keymap: Arc<Keymap>,
    keymap_src: BTreeMap<String, String>,
    /// Monospace family currently installed (fonts reload when it changes).
    mono_family: String,
    /// Background clones (non-modal progress panel).
    clones: Vec<dialogs::CloneJob>,
    /// Startup git check: the warning to show, once known.
    git_check: Arc<Mutex<Option<String>>>,
    git_warning_dismissed: bool,
}

/// Download page linked from the git version banner.
const GIT_DOWNLOAD_URL: &str = "https://git-scm.com/downloads/win";

/// [`gk_cli::Git::check_minimum`] (missing or too old git), as a warning.
/// `GITKWAKEN_FAKE_GIT_VERSION` (e.g. "2.30.1") replaces the installed
/// version, for screenshots and tests.
fn check_git() -> Option<String> {
    if let Ok(fake) = std::env::var("GITKWAKEN_FAKE_GIT_VERSION") {
        let v = gk_cli::parse_version(&format!("git version {fake}"))?;
        let (a, b) = gk_cli::MIN_VERSION;
        return ((v.0, v.1) < (a, b)).then(|| {
            format!(
                "Git {}.{}.{} is too old: GitKwaken needs {a}.{b} or newer. Some operations will fail.",
                v.0, v.1, v.2
            )
        });
    }
    gk_cli::Git::check_minimum().err()
}

impl GkApp {
    pub fn new(
        ctx: &egui::Context,
        settings: Settings,
        initial: Vec<PathBuf>,
        staged: crate::update::Staged,
    ) -> Self {
        let mono_family = mono_family(&settings);
        install_fonts(ctx, &mono_family);
        let palette = theme::apply_with(
            ctx,
            settings.theme,
            settings.ui_scale,
            settings.font_size,
            settings.mono_font_size,
        );
        theme::apply_density(ctx, theme::Density::of(settings.compact));
        let git_check: Arc<Mutex<Option<String>>> = Arc::default();
        {
            let (slot, c) = (git_check.clone(), ctx.clone());
            let _ = std::thread::Builder::new()
                .name("gk-git-version".into())
                .spawn(move || {
                    if let Some(w) = check_git() {
                        *slot.lock() = Some(w);
                        c.request_repaint();
                    }
                });
        }
        let c = ctx.clone();
        let askpass = gk_askpass::Server::start(Box::new(move || c.request_repaint()))
            .ok()
            .map(Arc::new);
        gk_core::content::set_detect_renames(settings.diff.renames);
        let updater = crate::update::Updater::new(ctx, staged, &settings);
        let keymap_src = settings.keymap.clone();
        let mut app = Self {
            autosave: Autosave::new(&settings),
            keymap: Arc::new(Keymap::from_overrides(&keymap_src)),
            keymap_src,
            mono_family,
            clones: Vec::new(),
            git_check,
            git_warning_dismissed: false,
            settings,
            tabs: Vec::new(),
            active: 0,
            log: CommandLog::default(),
            askpass,
            askpass_reply: String::new(),
            palette,
            global: None,
            actions: Vec::new(),
            show_log: false,
            errors: Vec::new(),
            command_palette: std::env::var("GITKWAKEN_PALETTE")
                .map(views::palette::PaletteState::run_on_start)
                .unwrap_or_default(),
            updater,
        };
        let mut to_open: Vec<PathBuf> = initial;
        if to_open.is_empty() {
            to_open = app.settings.open_tabs.clone();
        }
        let active = app.settings.active_tab;
        for p in to_open {
            app.open_repo(ctx, p);
        }
        app.active = active.min(app.tabs.len().saturating_sub(1));
        dialogs::set_askpass_env(app.askpass_env());
        app
    }

    fn askpass_env(&self) -> Vec<(String, String)> {
        match (&self.askpass, std::env::current_exe()) {
            (Some(s), Ok(exe)) => s.env(&exe),
            _ => Vec::new(),
        }
    }

    pub fn open_repo(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let Some(i) = self.tabs.iter().position(|t| same_path(&t.path, &path)) {
            self.active = i;
            return;
        }
        match RepoTab::open(
            &path,
            ctx,
            self.log.clone(),
            self.askpass_env(),
            &self.settings,
        ) {
            Ok(tab) => {
                self.settings.add_recent(tab.path.clone());
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
            Err(e) => self
                .errors
                .push(format!("Cannot open {}: {e:#}", path.display())),
        }
    }

    fn handle_actions(&mut self, ctx: &egui::Context) {
        for a in std::mem::take(&mut self.actions) {
            match a {
                Action::OpenRepo(p) => self.open_repo(ctx, p),
                Action::OpenRepoDialog => {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Open repository")
                        .pick_folder()
                    {
                        self.open_repo(ctx, p);
                    }
                }
                Action::CloseTab(i) => {
                    if i < self.tabs.len() {
                        let mut t = self.tabs.remove(i);
                        t.save_state();
                        if self.active >= self.tabs.len() {
                            self.active = self.tabs.len().saturating_sub(1);
                        }
                    }
                }
                Action::SelectTab(i) => self.active = i.min(self.tabs.len().saturating_sub(1)),
                Action::Global(mut d) => {
                    d.set_repo(self.tabs.get(self.active).map(|t| t.path.clone()));
                    self.global = Some(d);
                }
                Action::StartClone(job) => self.clones.push(*job),
                Action::ApplySettings => {
                    gk_core::content::set_detect_renames(self.settings.diff.renames);
                    let family = mono_family(&self.settings);
                    if family != self.mono_family {
                        install_fonts(ctx, &family);
                        self.mono_family = family;
                    }
                    self.palette = theme::apply_with(
                        ctx,
                        self.settings.theme,
                        self.settings.ui_scale,
                        self.settings.font_size,
                        self.settings.mono_font_size,
                    );
                    theme::apply_density(ctx, theme::Density::of(self.settings.compact));
                    // Status options apply to open tabs right away.
                    for t in &self.tabs {
                        let mut o = t.session.options();
                        o.status.engine = match self.settings.status_engine {
                            gk_settings::StatusEngine::Gix => gk_status::Engine::Gix,
                            gk_settings::StatusEngine::Cli => gk_status::Engine::Cli,
                        };
                        o.status.untracked = if self.settings.show_untracked {
                            gk_status::Untracked::All
                        } else {
                            gk_status::Untracked::No
                        };
                        t.session.set_options(o);
                    }
                }
            }
        }
    }

    fn persist(&mut self) {
        self.settings.open_tabs = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.settings.active_tab = self.active;
        let s = &self.settings;
        self.autosave.maybe_save(s, |s| s.save());
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (i, t) in self.tabs.iter().enumerate() {
                let selected = i == self.active;
                let r = ui
                    .selectable_label(selected, &t.name)
                    .on_hover_text(t.path.display().to_string());
                if r.clicked() {
                    self.actions.push(Action::SelectTab(i));
                }
                if r.middle_clicked() {
                    self.actions.push(Action::CloseTab(i));
                }
                r.context_menu(|ui| {
                    if ui.button("Close").clicked() {
                        self.actions.push(Action::CloseTab(i));
                        ui.close();
                    }
                    if ui.button("Open in Explorer").clicked() {
                        let _ = open::that(&t.path);
                        ui.close();
                    }
                });
                if ui.small_button("×").on_hover_text("Close tab").clicked() {
                    self.actions.push(Action::CloseTab(i));
                }
                ui.separator();
            }
            if ui
                .button("+")
                .on_hover_text(format!(
                    "Open repository ({})",
                    self.keymap.text(Act::OpenRepo)
                ))
                .clicked()
            {
                self.actions.push(Action::OpenRepoDialog);
            }
            if !self.tabs.is_empty() {
                // Bookmarks and recent repositories while tabs are open.
                let current = self.tabs.get(self.active).map(|t| t.path.clone());
                let mut shared = Shared {
                    settings: &mut self.settings,
                    palette: self.palette,
                    log: &self.log,
                    actions: &mut self.actions,
                    show_log: &mut self.show_log,
                };
                views::welcome::repositories_menu(ui, &mut shared, current.as_deref());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.updater.ui(ui);
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.actions
                        .push(Action::Global(dialogs::GlobalDialog::settings()));
                }
                if ui.selectable_label(self.show_log, "Output").clicked() {
                    self.show_log = !self.show_log;
                }
                if ui.button("Clone…").clicked() {
                    self.actions
                        .push(Action::Global(dialogs::GlobalDialog::clone_repo()));
                }
            });
        });
    }

    fn askpass_prompt(&mut self, ctx: &egui::Context) {
        let Some(server) = self.askpass.clone() else {
            return;
        };
        let Some(p) = server.pending() else { return };
        let mut answer: Option<Option<String>> = None;
        egui::Modal::new(egui::Id::new(("askpass", p.id))).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Git needs input");
            ui.label(&p.text);
            match p.kind {
                gk_askpass::PromptKind::YesNo => {
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            answer = Some(Some("yes".into()));
                        }
                        if ui.button("No").clicked() {
                            answer = Some(None);
                        }
                    });
                }
                kind => {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.askpass_reply)
                            .password(kind.is_secret())
                            .desired_width(f32::INFINITY),
                    );
                    r.request_focus();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            answer = Some(Some(std::mem::take(&mut self.askpass_reply)));
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(None);
                        }
                    });
                }
            }
        });
        if let Some(a) = answer {
            self.askpass_reply.clear();
            server.answer(p.id, a);
        }
    }

    /// Rebuilds the keymap when `settings.keymap` changed and publishes it
    /// for this frame (suspended while a global dialog or modal is open).
    fn publish_keymap(&mut self, ctx: &egui::Context) {
        if self.settings.keymap != self.keymap_src {
            self.keymap_src = self.settings.keymap.clone();
            self.keymap = Arc::new(Keymap::from_overrides(&self.keymap_src));
        }
        let suspended = self.global.is_some()
            || self.command_palette.open
            || !self.errors.is_empty()
            || self.askpass.as_ref().is_some_and(|s| s.pending().is_some());
        keymap::publish(ctx, self.keymap.clone(), suspended);
    }

    /// Application-level shortcuts from the keymap.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        // A global dialog (e.g. the keymap editor recording keys) owns the keyboard.
        if self.global.is_some() || !self.errors.is_empty() {
            return;
        }
        let km = self.keymap.clone();
        let [open, close, next, prev, palette, settings] = ctx.input_mut(|i| {
            [
                Act::OpenRepo,
                Act::CloseTab,
                Act::NextTab,
                Act::PrevTab,
                Act::Palette,
                Act::Settings,
            ]
            .map(|a| km.consume(i, a))
        });
        // Never open over a dialog: its unsaved input would be lost.
        let dialog_open = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.dialog.is_some());
        if palette && !dialog_open {
            self.command_palette.toggle();
        }
        if self.command_palette.open {
            return;
        }
        if open {
            self.actions.push(Action::OpenRepoDialog);
        }
        if close && !self.tabs.is_empty() {
            self.actions.push(Action::CloseTab(self.active));
        }
        if next && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        if prev && !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
        if settings && !dialog_open {
            self.actions
                .push(Action::Global(dialogs::GlobalDialog::settings()));
        }
    }

    /// Non-modal warning when git is missing or older than the minimum.
    fn git_banner(&mut self, ui: &mut egui::Ui) {
        if self.git_warning_dismissed {
            return;
        }
        let Some(text) = self.git_check.lock().clone() else {
            return;
        };
        let fill = self.palette.conflict.gamma_multiply(0.18);
        egui::Panel::top("git-version-banner")
            .frame(
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("⚠")
                            .color(self.palette.conflict)
                            .strong(),
                    );
                    ui.label(text);
                    ui.hyperlink_to("Install the latest Git", GIT_DOWNLOAD_URL);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            self.git_warning_dismissed = true;
                        }
                    });
                });
            });
    }

    /// Floating, non-modal panel with background clones (top right).
    fn jobs_panel(&mut self, ctx: &egui::Context) {
        let mut open: Vec<PathBuf> = Vec::new();
        for j in &mut self.clones {
            if let Some(Ok(dest)) = j.poll() {
                open.push(dest);
            }
        }
        self.clones.retain(|j| j.running() || j.error.is_some());
        for p in open {
            self.actions.push(Action::OpenRepo(p));
        }
        if self.clones.is_empty() {
            return;
        }
        let p = self.palette;
        let mut dismiss: Option<usize> = None;
        egui::Area::new(egui::Id::new("gk-jobs"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 76.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(380.0);
                    for (i, j) in self.clones.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        ui.horizontal(|ui| {
                            if j.running() {
                                ui.spinner();
                                ui.label(
                                    egui::RichText::new(format!("Cloning {}", j.name())).strong(),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(format!("Clone of {} failed", j.name()))
                                        .strong()
                                        .color(p.removed_fg),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if j.running() {
                                        if ui.small_button("Cancel").clicked() {
                                            j.cancel();
                                        }
                                    } else if ui.small_button("Dismiss").clicked() {
                                        dismiss = Some(i);
                                    }
                                },
                            );
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(&j.url).small().color(p.muted))
                                .truncate(),
                        );
                        match &j.error {
                            Some(e) => {
                                ui.label(egui::RichText::new(e).small().color(p.removed_fg));
                            }
                            None => {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(j.last_line()).monospace().small(),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(j.output());
                            }
                        }
                    }
                });
            });
        if let Some(i) = dismiss {
            self.clones.remove(i);
        }
    }
}

/// Installs the UI fonts with `family` as monospace (`theme::install_fonts`
/// itself honors the `GITKWAKEN_MONO_FONT` override).
fn install_fonts(ctx: &egui::Context, family: &str) {
    if std::env::var_os("GITKWAKEN_MONO_FONT").is_some() {
        theme::install_fonts(ctx);
    } else {
        theme::install_fonts_with(ctx, Some(family));
    }
}

/// Monospace family to install: `GITKWAKEN_MONO_FONT` (dev override) or the setting.
fn mono_family(s: &Settings) -> String {
    std::env::var("GITKWAKEN_MONO_FONT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| s.mono_font_family.clone())
}

impl gk_dx11::App for GkApp {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.publish_keymap(&ctx);
        self.shortcuts(&ctx);
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        self.git_banner(ui);
        if self.show_log {
            egui::Panel::bottom("output")
                .resizable(true)
                .default_size(180.0)
                .show(ui, |ui| views::log::show(ui, &self.log));
        }
        let palette = self.palette;
        let mut shared = Shared {
            settings: &mut self.settings,
            palette,
            log: &self.log,
            actions: &mut self.actions,
            show_log: &mut self.show_log,
        };
        if self.tabs.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| views::welcome::show(ui, &mut shared));
        } else {
            let i = self.active.min(self.tabs.len() - 1);
            self.tabs[i].show(ui, &mut shared);
            if crate::startup::active() {
                crate::startup::mark("first_frame", false);
                let tab = &self.tabs[i];
                let snap = tab.session.snapshot();
                if tab.view == crate::tab::View::History && !snap.history.is_empty() {
                    let label = if snap.from_cache {
                        "history_rows_from_cache"
                    } else {
                        "history_rows"
                    };
                    crate::startup::mark(label, true);
                }
            }
        }
        if let Some(mut g) = self.global.take() {
            let mut shared = Shared {
                settings: &mut self.settings,
                palette,
                log: &self.log,
                actions: &mut self.actions,
                show_log: &mut self.show_log,
            };
            if g.show(&ctx, &mut shared) {
                self.global = Some(g);
            }
        }
        if !self.errors.is_empty() {
            let mut close = false;
            egui::Modal::new(egui::Id::new("app-errors")).show(&ctx, |ui| {
                for e in &self.errors {
                    ui.colored_label(self.palette.removed_fg, e);
                }
                close = ui.button("OK").clicked();
            });
            if close {
                self.errors.clear();
            }
        }
        {
            let mut shared = Shared {
                settings: &mut self.settings,
                palette,
                log: &self.log,
                actions: &mut self.actions,
                show_log: &mut self.show_log,
            };
            let i = self.active.min(self.tabs.len().saturating_sub(1));
            let tab = self.tabs.get_mut(i);
            views::palette::show(&ctx, &mut self.command_palette, tab, &mut shared);
        }
        self.jobs_panel(&ctx);
        self.askpass_prompt(&ctx);
        self.handle_actions(&ctx);
        self.persist();
    }

    fn on_close(&mut self, geometry: gk_dx11::WindowGeometry) {
        for t in &mut self.tabs {
            t.save_state();
        }
        self.settings.window.position = geometry.position;
        self.settings.window.size = geometry.size;
        self.settings.window.maximized = geometry.maximized;
        self.settings.open_tabs = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.settings.active_tab = self.active;
        let _ = self.settings.save();
    }

    fn clear_color(&self) -> [f32; 4] {
        if self.palette.dark {
            [0.145, 0.153, 0.169, 1.0]
        } else {
            [0.97, 0.97, 0.97, 1.0]
        }
    }
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| {
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
            .trim_end_matches(['\\', '/'])
            .to_string()
    };
    norm(a) == norm(b)
}
