//! Background update check and the tab-bar button that drives it.
//!
//! Only Windows builds update themselves: the release channel carries the Windows zip and the
//! swap replaces `gitkwaken.exe`. Elsewhere GitKwaken comes from a package manager or the Linux
//! tarball, so the updater stays off and Help › Check for updates says where to get new versions.

use gk_update::Release;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
enum State {
    Idle,
    /// Manual check running (Help › Check for updates).
    Checking,
    /// Manual check found nothing newer.
    UpToDate,
    /// Manual check on a platform without self-update.
    Unsupported,
    Available(Release),
    Downloading(u64, u64),
    Ready(String),
    Failed(String),
}

/// Shared with `main`, which swaps the executable after the window closes.
pub type Staged = Arc<Mutex<Option<PathBuf>>>;

/// Whether this platform updates itself (see the module docs).
pub const SELF_UPDATE: bool = cfg!(windows);

/// Help › Check for updates where [`SELF_UPDATE`] is off.
const UNSUPPORTED: &str = "GitKwaken does not update itself on this platform. Update it with \
    your package manager, or download the new release from \
    https://github.com/maxpowerart/GitKwaken/releases";

/// Set by "Restart now": `main` starts the updated executable after swapping it.
pub static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a Help-menu update entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    Check,
    Install,
    Restart,
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    staged: Staged,
    url: Option<String>,
}

impl Updater {
    /// Channel URL: `GITKWAKEN_UPDATE_URL`, else Settings → Update channel,
    /// else the built-in release channel. None where [`SELF_UPDATE`] is off.
    pub fn new(ctx: &egui::Context, staged: Staged, settings: &gk_settings::Settings) -> Self {
        let url = std::env::var("GITKWAKEN_UPDATE_URL")
            .ok()
            .or_else(|| (!settings.update_url.is_empty()).then(|| settings.update_url.clone()))
            .or_else(|| gk_update::DEFAULT_CHANNEL_URL.map(String::from))
            .filter(|_| SELF_UPDATE);
        let u = Self {
            state: Arc::new(Mutex::new(State::Idle)),
            staged,
            url,
        };
        // Local builds have no built-in channel; release builds get it at build time.
        let enabled =
            settings.check_updates && std::env::var_os("GITKWAKEN_NO_UPDATE_CHECK").is_none();
        if enabled {
            u.spawn_check(ctx, false);
        }
        u
    }

    /// Help › Check for updates: checks now and reports "up to date" too.
    pub fn check_now(&self, ctx: &egui::Context) {
        let busy = matches!(
            *self.state.lock(),
            State::Checking | State::Downloading(..) | State::Ready(_)
        );
        if busy {
            return;
        }
        if !SELF_UPDATE {
            *self.state.lock() = State::Unsupported;
            return;
        }
        if self.url.is_none() {
            *self.state.lock() = State::Failed("This build has no update channel".into());
            return;
        }
        *self.state.lock() = State::Checking;
        self.spawn_check(ctx, true);
    }

    fn spawn_check(&self, ctx: &egui::Context, report: bool) {
        let Some(url) = self.url.clone() else { return };
        let s = self.state.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = gk_update::check(env!("CARGO_PKG_VERSION"), &url);
            let next = match r {
                Ok(Some(rel)) => Some(State::Available(rel)),
                Ok(None) if report => Some(State::UpToDate),
                Err(e) if report => Some(State::Failed(format!("{e:#}"))),
                _ if report => Some(State::Idle),
                _ => None,
            };
            if let Some(n) = next {
                *s.lock() = n;
                c.request_repaint();
            }
        });
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

    /// The Help-menu update entry: check, install an available
    /// update, or restart to finish a downloaded one.
    pub fn menu_entry(&self) -> (String, bool, UpdateAction) {
        // One slot whatever the state, so the menu layout never shifts.
        match &*self.state.lock() {
            State::Available(rel) => (
                format!("Install update {}", rel.version),
                true,
                UpdateAction::Install,
            ),
            State::Ready(v) => (
                format!("Restart to finish update {v}"),
                true,
                UpdateAction::Restart,
            ),
            State::Checking => ("Checking for updates…".into(), false, UpdateAction::Check),
            State::Downloading(done, total) => (
                match (done * 100).checked_div(*total) {
                    Some(pct) => format!("Downloading update {pct}%"),
                    None => "Downloading update…".into(),
                },
                false,
                UpdateAction::Check,
            ),
            State::Idle | State::UpToDate | State::Failed(_) => {
                ("Check for updates".into(), true, UpdateAction::Check)
            }
        }
    }

    /// Runs a Help-menu entry (same as the tab-bar buttons).
    pub fn act(&self, action: UpdateAction, ctx: &egui::Context) {
        match action {
            UpdateAction::Check => self.check_now(ctx),
            UpdateAction::Install => {
                let rel = match &*self.state.lock() {
                    State::Available(rel) => rel.clone(),
                    _ => return,
                };
                self.download(rel, ctx);
            }
            UpdateAction::Restart => {
                if matches!(*self.state.lock(), State::Ready(_)) {
                    RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// Small control for the tab bar; nothing is shown while there is no update.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let state = self.state.lock().clone();
        match state {
            State::Idle => {}
            State::Checking => {
                ui.spinner();
                ui.label("Checking for updates…");
            }
            State::UpToDate => {
                if ui.small_button("✖").on_hover_text("Dismiss").clicked() {
                    *self.state.lock() = State::Idle;
                }
                ui.label(format!(
                    "GitKwaken {} is up to date",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            State::Unsupported => {
                if ui.small_button("✖").on_hover_text("Dismiss").clicked() {
                    *self.state.lock() = State::Idle;
                }
                ui.label("Update via your package manager or re-download")
                    .on_hover_text(UNSUPPORTED);
            }
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
