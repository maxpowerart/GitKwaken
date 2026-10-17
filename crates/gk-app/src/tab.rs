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
    /// Built-in 3-way merge editor; state in [`RepoTab::merge`].
    Merge,
    /// Blame of one file; state in [`RepoTab::blame`].
    Blame,
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
    /// Open merge editor (see [`views::merge::open_merge`]).
    pub merge: Option<Box<views::merge::MergeState>>,
    /// Open blame (see [`views::blame::open_blame`]).
    pub blame: Option<Box<views::blame::BlameState>>,
    pub dialog: Option<Dialog>,
    /// Last snapshot generation seen, to detect changes cheaply.
    pub seen_generation: u64,
    last_fetch: Option<std::time::Instant>,
    fetching: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
        opts.status.untracked = if settings.show_untracked {
            gk_status::Untracked::All
        } else {
            gk_status::Untracked::No
        };
        opts.scope = if state.history_scope == "current" {
            gk_core::history::Scope::Current
        } else {
            gk_core::history::Scope::All
        };
        let c = ctx.clone();
        let session = Session::open(repo, opts, move || c.request_repaint());
        let name = session.repo().name();
        let view = if state.last_view == "history" {
            View::History
        } else {
            View::FileStatus
        };
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
            merge: None,
            blame: None,
            dialog: None,
            seen_generation: 0,
            last_fetch: None,
            fetching: Default::default(),
            state,
        };
        tab.status.commit.message = tab.state.commit_message_draft.clone();
        // Debug/screenshot aid: `GITKWAKEN_START_VIEW` = `status`, `history`,
        // `search`, `merge:<path>` or `blame:<path>` opens that view on startup.
        if let Ok(v) = std::env::var("GITKWAKEN_START_VIEW") {
            match (v.as_str(), v.split_once(':')) {
                ("status", _) => tab.view = View::FileStatus,
                ("history", _) => tab.view = View::History,
                ("search", _) => tab.view = View::Search,
                (_, Some(("merge", p))) => views::merge::open_merge(&mut tab, p.to_string()),
                (_, Some(("blame", p))) => views::blame::open_blame(&mut tab, p.to_string(), None),
                _ => {}
            }
        }
        // Debug/screenshot aid: `GITKWAKEN_SELECT=<repo-relative path>` focuses
        // that file in File Status (and in commit details when it is changed).
        if let Ok(p) = std::env::var("GITKWAKEN_SELECT")
            && !p.is_empty()
        {
            tab.status.preselect(p.clone());
            tab.details.preselect(p);
        }
        // Debug/screenshot aid: `GITKWAKEN_SEARCH=<text>` runs a commit search
        // on startup (and shows the Search view).
        if let Ok(q) = std::env::var("GITKWAKEN_SEARCH")
            && !q.is_empty()
        {
            tab.search.start_query(q);
            tab.view = View::Search;
        }
        // Other dev aids: `GITKWAKEN_AUTOSCROLL` (views/history.rs),
        // `GITKWAKEN_PALETTE` (views/palette.rs).
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

    /// Fetches in the background every `auto_fetch_minutes`; failures stay quiet
    /// (offline laptops should not get an error every few minutes).
    fn auto_fetch(&mut self, snap: &gk_core::Snapshot, minutes: u32) {
        if minutes == 0 || snap.refs.remotes.is_empty() || snap.op.is_some() {
            return;
        }
        let due = self
            .last_fetch
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(minutes as u64 * 60));
        if !due || self.fetching.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.last_fetch = Some(std::time::Instant::now());
        self.fetching
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let session = self.session.clone();
        let flag = self.fetching.clone();
        std::thread::spawn(move || {
            let opts = gk_core::ops::FetchOptions {
                prune: true,
                tags: false,
            };
            let ok = session
                .repo()
                .fetch(None, &opts, &gk_cli::CancelToken::new(), &mut |_| {})
                .is_ok();
            flag.store(false, std::sync::atomic::Ordering::SeqCst);
            if ok {
                session.refresh_all();
            }
        });
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let (refresh, v1, v2, v3) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::F5),
                i.consume_key(Modifiers::COMMAND, Key::Num1),
                i.consume_key(Modifiers::COMMAND, Key::Num2),
                i.consume_key(Modifiers::COMMAND, Key::Num3),
            )
        });
        if refresh {
            self.session.refresh_all();
        }
        if v1 {
            self.view = View::FileStatus;
        }
        if v2 {
            self.view = View::History;
        }
        if v3 {
            self.view = View::Search;
        }
    }

    /// Renders the whole tab body (toolbar, sidebar, main view, dialogs).
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &mut Shared) {
        let snap = self.session.snapshot();
        if self.dialog.is_none() {
            self.shortcuts(ui.ctx());
        }
        self.auto_fetch(&snap, shared.settings.auto_fetch_minutes);
        views::toolbar::show(ui, self, &snap, shared);
        views::statusbar::show(ui, self, &snap, shared);
        let side = egui::Panel::left(egui::Id::new(("sidebar", &self.path)))
            .resizable(true)
            // Small windows: keep the default sidebar to a fifth of the width.
            .default_size(
                self.state
                    .layout
                    .sidebar_width
                    .min(ui.available_width() * 0.2)
                    .max(160.0),
            )
            .size_range(140.0..=480.0)
            .show(ui, |ui| views::sidebar::show(ui, self, &snap, shared));
        self.state.layout.sidebar_width = side.response.rect.width();
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::FileStatus => views::file_status::show(ui, self, &snap, shared),
            View::History => views::history::show(ui, self, &snap, shared),
            View::Search => views::search::show(ui, self, &snap, shared),
            View::Merge => views::merge::show(ui, self, &snap, shared),
            View::Blame => views::blame::show(ui, self, &snap, shared),
        });
        if let Some(mut d) = self.dialog.take()
            && d.show(ui.ctx(), self, &snap, shared)
            && self.dialog.is_none()
        {
            self.dialog = Some(d);
        }
        self.seen_generation = snap.generation;
    }
}
