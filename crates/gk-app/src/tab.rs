//! One open repository: its session plus all per-view UI state.

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::views;
use gk_core::{Repo, Session, SessionOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    FileStatus,
    History,
    Search,
}

pub struct RepoTab {
    pub session: Session,
    pub path: PathBuf,
    pub name: String,
    pub view: View,
    /// Persisted per-repository UI state (draft message, layout...).
    pub state: gk_settings::RepoState,
    pub history: views::history::HistoryState,
    pub status: views::file_status::StatusState,
    pub details: views::details::DetailsState,
    pub sidebar: views::sidebar::SidebarState,
    pub search: views::search::SearchState,
    pub dialog: Option<Dialog>,
    /// Last snapshot generation seen, to detect changes cheaply.
    pub seen_generation: u64,
}

impl RepoTab {
    pub fn open(
        path: &Path,
        ctx: &egui::Context,
        log: gk_cli::CommandLog,
        env: Vec<(String, String)>,
        settings: &gk_settings::Settings,
    ) -> anyhow::Result<Self> {
        let repo = Repo::open(path, log)?.with_env(env);
        let workdir = repo.workdir().to_path_buf();
        let state = gk_settings::RepoState::load(&workdir);
        let mut opts = SessionOptions::default();
        opts.status.engine = match settings.status_engine {
            gk_settings::StatusEngine::Gix => gk_status::Engine::Gix,
            gk_settings::StatusEngine::Cli => gk_status::Engine::Cli,
        };
        opts.status.untracked =
            if settings.show_untracked { gk_status::Untracked::All } else { gk_status::Untracked::No };
        opts.scope = if state.history_scope == "current" {
            gk_core::history::Scope::Current
        } else {
            gk_core::history::Scope::All
        };
        let c = ctx.clone();
        let session = Session::open(repo, opts, move || c.request_repaint());
        let name = session.repo().name();
        let view = if state.last_view == "history" { View::History } else { View::FileStatus };
        let mut tab = Self {
            session,
            path: workdir,
            name,
            view,
            history: Default::default(),
            status: Default::default(),
            details: Default::default(),
            sidebar: Default::default(),
            search: Default::default(),
            dialog: None,
            seen_generation: 0,
            state,
        };
        tab.status.commit.message = tab.state.commit_message_draft.clone();
        Ok(tab)
    }

    pub fn save_state(&mut self) {
        self.state.commit_message_draft = self.status.commit.message.clone();
        self.state.last_view = match self.view {
            View::History => "history",
            _ => "status",
        }
        .into();
        let _ = self.state.save(&self.path);
    }

    /// Renders the whole tab body (toolbar, sidebar, main view, dialogs).
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &mut Shared) {
        let snap = self.session.snapshot();
        views::toolbar::show(ui, self, &snap, shared);
        views::statusbar::show(ui, self, &snap, shared);
        egui::Panel::left(egui::Id::new(("sidebar", &self.path)))
            .resizable(true)
            .default_size(self.state.layout.sidebar_width.max(160.0))
            .size_range(140.0..=480.0)
            .show(ui, |ui| views::sidebar::show(ui, self, &snap, shared));
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::FileStatus => views::file_status::show(ui, self, &snap, shared),
            View::History => views::history::show(ui, self, &snap, shared),
            View::Search => views::search::show(ui, self, &snap, shared),
        });
        if let Some(mut d) = self.dialog.take() {
            if d.show(ui.ctx(), self, &snap, shared) {
                if self.dialog.is_none() {
                    self.dialog = Some(d);
                }
            }
        }
        self.seen_generation = snap.generation;
    }
}
