//! Background update check and the tab-bar button that drives it.

use gk_update::Release;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
enum State {
    Idle,
    Available(Release),
    Downloading(u64, u64),
    Ready(String),
    Failed(String),
}

/// Shared with `main`, which swaps the executable after the window closes.
pub type Staged = Arc<Mutex<Option<PathBuf>>>;

/// Set by "Restart now": `main` starts the updated executable after swapping it.
pub static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct Updater {
    state: Arc<Mutex<State>>,
    staged: Staged,
}

impl Updater {
    /// Channel URL: `GITKWAKEN_UPDATE_URL`, else Settings → Update channel,
    /// else the built-in release channel.
    pub fn new(ctx: &egui::Context, staged: Staged, settings: &gk_settings::Settings) -> Self {
        let state = Arc::new(Mutex::new(State::Idle));
        let url = std::env::var("GITKWAKEN_UPDATE_URL")
            .ok()
            .or_else(|| (!settings.update_url.is_empty()).then(|| settings.update_url.clone()))
            .or_else(|| gk_update::DEFAULT_CHANNEL_URL.map(String::from));
        // Local builds have no built-in channel; release builds get it at build time.
        let enabled =
            settings.check_updates && std::env::var_os("GITKWAKEN_NO_UPDATE_CHECK").is_none();
        if let (true, Some(url)) = (enabled, url) {
            let s = state.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                if let Ok(Some(rel)) = gk_update::check(env!("CARGO_PKG_VERSION"), &url) {
                    *s.lock() = State::Available(rel);
                    c.request_repaint();
                }
            });
        }
        Self { state, staged }
    }

    fn download(&self, rel: Release, ctx: &egui::Context) {
        *self.state.lock() = State::Downloading(0, 0);
        let s = self.state.clone();
        let staged = self.staged.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let s2 = s.clone();
            let c2 = c.clone();
            let result = gk_update::download_and_stage(&rel, move |done, total| {
                *s2.lock() = State::Downloading(done, total);
                c2.request_repaint();
            });
            *s.lock() = match result {
                Ok(path) => {
                    *staged.lock() = Some(path);
                    State::Ready(rel.version.clone())
                }
                Err(e) => State::Failed(format!("{e:#}")),
            };
            c.request_repaint();
        });
    }

    /// Small control for the tab bar; nothing is shown while there is no update.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let state = self.state.lock().clone();
        match state {
            State::Idle => {}
            State::Available(rel) => {
                let b = ui
                    .button(format!("⬆ Update to {}", rel.version))
                    .on_hover_text(if rel.notes.is_empty() {
                        "A new version is available".to_string()
                    } else {
                        rel.notes.clone()
                    });
                if b.clicked() {
                    self.download(rel, ui.ctx());
                }
            }
            State::Downloading(done, total) => {
                ui.spinner();
                if let Some(pct) = (done * 100).checked_div(total) {
                    ui.label(format!("Downloading update {pct}%"));
                } else {
                    ui.label("Downloading update…");
                }
            }
            State::Ready(v) => {
                ui.label(format!("Update {v} installs when GitKwaken closes"));
                if ui.button("Restart now").clicked() {
                    RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            State::Failed(e) => {
                ui.label("Update failed").on_hover_text(e);
            }
        }
    }
}
