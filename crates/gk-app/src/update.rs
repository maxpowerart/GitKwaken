//! Background update check and the tab-bar button that drives it.
//!
//! Every release build knows its channel (`update.json` of the latest GitHub release) and checks
//! it at startup and every [`CHECK_EVERY`] while running. A newer release is offered when this
//! installation can take it ([`gk_update::method`]): Windows and Linux tarball installs download
//! the new executable and swap it in on exit; a pacman install (Arch / Manjaro) downloads the
//! release's package and installs it with `pkexec pacman -U` right away.

use gk_update::Release;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
enum State {
    Idle,
    /// Manual check running (Help › Check for updates).
    Checking,
    /// Manual check found nothing newer.
    UpToDate,
    Available(Release),
    Downloading(u64, u64),
    /// `pkexec pacman -U` running (polkit asks for the password).
    Installing,
    Ready(String),
    Failed(String),
}

impl State {
    /// Nothing in progress or waiting for the user: a background check may report a release.
    fn quiet(&self) -> bool {
        matches!(self, State::Idle | State::UpToDate | State::Failed(_))
    }
}

/// Shared with `main`, which swaps the executable after the window closes.
pub type Staged = Arc<Mutex<Option<PathBuf>>>;

/// Interval of the background re-check while GitKwaken stays open.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

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
    /// The "update available" dialog (changelog, install or not) is open.
    asking: Mutex<bool>,
}

impl Updater {
    /// Channel URL: `GITKWAKEN_UPDATE_URL`, else Settings → Update channel,
    /// else the built-in release channel.
    pub fn new(ctx: &egui::Context, staged: Staged, settings: &gk_settings::Settings) -> Self {
        let url = std::env::var("GITKWAKEN_UPDATE_URL")
            .ok()
            .or_else(|| (!settings.update_url.is_empty()).then(|| settings.update_url.clone()))
            .or_else(|| gk_update::DEFAULT_CHANNEL_URL.map(String::from));
        let u = Self {
            state: Arc::new(Mutex::new(State::Idle)),
            staged,
            url,
            asking: Mutex::new(false),
        };
        // Local builds have no built-in channel; release builds get it at build time.
        let enabled =
            settings.check_updates && std::env::var_os("GITKWAKEN_NO_UPDATE_CHECK").is_none();
        if enabled && let Some(url) = u.url.clone() {
            let s = u.state.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                loop {
                    check(&url, &s, &c, false);
                    std::thread::sleep(CHECK_EVERY);
                }
            });
        }
        u
    }

    /// Help › Check for updates: checks now and reports "up to date" too.
    pub fn check_now(&self, ctx: &egui::Context) {
        let busy = matches!(
            *self.state.lock(),
            State::Checking | State::Downloading(..) | State::Installing | State::Ready(_)
        );
        if busy {
            return;
        }
        let Some(url) = self.url.clone() else {
            *self.state.lock() = State::Failed("This build has no update channel".into());
            return;
        };
        *self.state.lock() = State::Checking;
        let s = self.state.clone();
        let c = ctx.clone();
        std::thread::spawn(move || check(&url, &s, &c, true));
    }

    /// Downloads `rel`, then stages the new executable or installs the package.
    fn install(&self, rel: Release, ctx: &egui::Context) {
        let method = match gk_update::method(&rel) {
            Ok(m) => m,
            Err(e) => {
                *self.state.lock() = State::Failed(format!("{e:#}"));
                return;
            }
        };
        *self.state.lock() = State::Downloading(0, 0);
        let s = self.state.clone();
        let staged = self.staged.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let s2 = s.clone();
            let c2 = c.clone();
            let result = gk_update::stage(&rel, &method, move |done, total| {
                *s2.lock() = State::Downloading(done, total);
                c2.request_repaint();
            })
            .and_then(|st| match st {
                gk_update::Staged::Exe(path) => {
                    *staged.lock() = Some(path);
                    Ok(())
                }
                gk_update::Staged::Package(pkg) => {
                    *s.lock() = State::Installing;
                    c.request_repaint();
                    gk_update::install_package(&pkg)
                }
            });
            *s.lock() = match result {
                Ok(()) => State::Ready(rel.version.clone()),
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
            State::Installing => ("Installing update…".into(), false, UpdateAction::Check),
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
                if matches!(*self.state.lock(), State::Available(_)) {
                    *self.asking.lock() = true;
                    ctx.request_repaint();
                }
            }
            UpdateAction::Restart => {
                if matches!(*self.state.lock(), State::Ready(_)) {
                    RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// The "update available" dialog: version, changelog, and Install / Not now. "Not now"
    /// (also Escape or a click outside) only closes it; the update button stays. Enter installs.
    pub fn dialog(&self, ctx: &egui::Context) {
        if !*self.asking.lock() {
            return;
        }
        let rel = match &*self.state.lock() {
            State::Available(rel) => rel.clone(),
            _ => {
                *self.asking.lock() = false;
                return;
            }
        };
        let how = match gk_update::method(&rel) {
            Ok(gk_update::Method::Pacman(_)) => {
                "The package is installed with pacman; the system asks for the administrator \
                 password. Restart GitKwaken afterwards."
            }
            Ok(_) => {
                "The download runs in the background; the update installs when GitKwaken \
                      restarts."
            }
            Err(_) => "",
        };
        // Taken before anything else reads the keyboard this frame.
        let (mut install, mut close) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let modal = egui::Modal::new(egui::Id::new("update-available")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(format!("GitKwaken {} is available", rel.version));
            let published = rel
                .published
                .as_deref()
                .map(|d| format!("Released {d}. "))
                .unwrap_or_default();
            ui.label(
                egui::RichText::new(format!(
                    "{published}You have {}.",
                    env!("CARGO_PKG_VERSION")
                ))
                .weak(),
            );
            ui.add_space(6.0);
            ui.label(egui::RichText::new("What's new").strong());
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(4.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| changelog_ui(ui, &rel.notes));
                });
            if !how.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(how).weak());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Not now").clicked() {
                        close = true;
                    }
                    let b = egui::Button::new(
                        egui::RichText::new("Install update").color(egui::Color32::WHITE),
                    )
                    .fill(ui.visuals().selection.bg_fill);
                    if ui.add(b).clicked() {
                        install = true;
                    }
                });
            });
        });
        if modal.should_close() {
            close = true;
        }
        if install {
            *self.asking.lock() = false;
            self.install(rel, ctx);
        } else if close {
            *self.asking.lock() = false;
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
            State::Available(rel) => {
                let b = ui
                    .button(format!("⬆ Update to {}", rel.version))
                    .on_hover_text("See what's new and install");
                if b.clicked() {
                    *self.asking.lock() = true;
                }
            }
            State::Installing => {
                ui.spinner();
                ui.label("Installing update…");
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
                if self.staged.lock().is_some() {
                    ui.label(format!("Update {v} installs when GitKwaken closes"));
                } else {
                    ui.label(format!("Update {v} installed"));
                }
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

/// Release notes as written in CHANGELOG.md: `#` headings bold, `-` / `*` items as bullets,
/// links clickable; anything else as plain text.
fn changelog_ui(ui: &mut egui::Ui, notes: &str) {
    let notes = notes.trim();
    if notes.is_empty() {
        ui.label(egui::RichText::new("No release notes.").weak());
        return;
    }
    ui.spacing_mut().item_spacing.y = 3.0;
    // Indented lines that are not list items continue the previous line (wrapped source).
    let mut lines: Vec<String> = Vec::new();
    for line in notes.lines() {
        let t = line.trim_end();
        let rest = t.trim_start();
        let continues = t.len() != rest.len()
            && !rest.is_empty()
            && !rest.starts_with("- ")
            && !rest.starts_with("* ");
        match lines.last_mut() {
            Some(prev) if continues && !prev.trim().is_empty() => {
                prev.push(' ');
                prev.push_str(rest);
            }
            _ => lines.push(t.to_string()),
        }
    }
    for t in &lines {
        let t = t.as_str();
        if t.trim().is_empty() {
            ui.add_space(4.0);
        } else if let Some(h) = t.strip_prefix('#') {
            ui.add_space(2.0);
            ui.label(egui::RichText::new(h.trim_start_matches('#').trim()).strong());
        } else if let Some(item) = t
            .trim_start()
            .strip_prefix("- ")
            .or_else(|| t.trim_start().strip_prefix("* "))
        {
            let indent = (t.len() - t.trim_start().len()) as f32 * 4.0;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(indent);
                ui.label("•");
                rich_line(ui, item);
            });
        } else {
            ui.horizontal_wrapped(|ui| rich_line(ui, t));
        }
    }
}

/// One changelog line: `**bold**` markers dropped, http(s) links as hyperlinks.
fn rich_line(ui: &mut egui::Ui, text: &str) {
    let text = text.replace("**", "").replace('`', "");
    if !text.contains("http") {
        ui.label(text);
        return;
    }
    ui.spacing_mut().item_spacing.x = 0.0;
    let mut first = true;
    for word in text.split(' ') {
        if !first {
            ui.label(" ");
        }
        first = false;
        let url = word.trim_end_matches(['.', ',', ')', ';']);
        if url.starts_with("https://") || url.starts_with("http://") {
            ui.hyperlink(url);
            ui.label(&word[url.len()..]);
        } else {
            ui.label(word);
        }
    }
}

/// Checks the channel at `url`. A manual check (`report`) always shows its outcome; a
/// background one only announces a newer release, and only while nothing else is going on.
fn check(url: &str, state: &Mutex<State>, ctx: &egui::Context, report: bool) {
    // A release this installation cannot take (e.g. no Linux build in it) is not offered.
    let r = gk_update::check(env!("CARGO_PKG_VERSION"), url).and_then(|rel| match rel {
        Some(rel) => gk_update::method(&rel).map(|_| Some(rel)),
        None => Ok(None),
    });
    let mut s = state.lock();
    let next = match r {
        Ok(Some(rel)) if report || s.quiet() => State::Available(rel),
        Ok(None) if report => State::UpToDate,
        Err(e) if report => State::Failed(format!("{e:#}")),
        _ => return,
    };
    *s = next;
    drop(s);
    ctx.request_repaint();
}
