//! Top-level application: tabs, global dialogs, askpass prompts, settings.

use crate::tab::RepoTab;
use crate::theme::{self, Palette};
use crate::{dialogs, views};
use gk_cli::CommandLog;
use gk_settings::{Autosave, Settings};
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
}

impl GkApp {
    pub fn new(ctx: &egui::Context, settings: Settings, initial: Vec<PathBuf>, staged: crate::update::Staged) -> Self {
        theme::install_fonts(ctx);
        let palette = theme::apply(ctx, settings.theme, settings.ui_scale, settings.font_size);
        let c = ctx.clone();
        let askpass = gk_askpass::Server::start(Box::new(move || c.request_repaint())).ok().map(Arc::new);
        let mut app = Self {
            autosave: Autosave::new(&settings),
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
            updater: crate::update::Updater::new(ctx, staged),
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
        match RepoTab::open(&path, ctx, self.log.clone(), self.askpass_env(), &self.settings) {
            Ok(tab) => {
                self.settings.add_recent(tab.path.clone());
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
            Err(e) => self.errors.push(format!("Cannot open {}: {e:#}", path.display())),
        }
    }

    fn handle_actions(&mut self, ctx: &egui::Context) {
        for a in std::mem::take(&mut self.actions) {
            match a {
                Action::OpenRepo(p) => self.open_repo(ctx, p),
                Action::OpenRepoDialog => {
                    if let Some(p) = rfd::FileDialog::new().set_title("Open repository").pick_folder() {
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
                Action::Global(d) => self.global = Some(d),
                Action::ApplySettings => {
                    self.palette =
                        theme::apply(ctx, self.settings.theme, self.settings.ui_scale, self.settings.font_size);
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
                let r = ui.selectable_label(selected, &t.name).on_hover_text(t.path.display().to_string());
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
            if ui.button("+").on_hover_text("Open repository (Ctrl+O)").clicked() {
                self.actions.push(Action::OpenRepoDialog);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.updater.ui(ui);
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.actions.push(Action::Global(dialogs::GlobalDialog::settings()));
                }
                if ui.selectable_label(self.show_log, "Output").clicked() {
                    self.show_log = !self.show_log;
                }
                if ui.button("Clone…").clicked() {
                    self.actions.push(Action::Global(dialogs::GlobalDialog::clone_repo()));
                }
            });
        });
    }

    fn askpass_prompt(&mut self, ctx: &egui::Context) {
        let Some(server) = self.askpass.clone() else { return };
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

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let (open, close, next, palette) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::W),
                i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab),
                i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::P),
            )
        });
        // Never open over a dialog: its unsaved input would be lost.
        let dialog_open = self.tabs.get(self.active).is_some_and(|t| t.dialog.is_some());
        if palette && !dialog_open {
            self.command_palette.toggle();
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
    }
}

impl gk_dx11::App for GkApp {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
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
        if self.palette.dark { [0.145, 0.153, 0.169, 1.0] } else { [0.97, 0.97, 0.97, 1.0] }
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
