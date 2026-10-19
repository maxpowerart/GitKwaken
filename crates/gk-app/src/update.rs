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
    /// The running version was withdrawn (a bad release): roll back to this one, the newest
    /// good release. The `String` is the reason given when it was withdrawn.
    Withdrawn(Release, String),
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
    /// `Settings::skipped_version`: background checks do not offer it.
    skip: Arc<Mutex<String>>,
    /// Help › Install another version… dialog.
    versions: Arc<Mutex<Versions>>,
}

/// State of the "Install another version" dialog.
#[derive(Default)]
struct Versions {
    open: bool,
    /// `None` while loading.
    list: Option<Result<Vec<gk_update::Listed>, String>>,
    selected: Option<usize>,
    /// Manifest of the selected version (`None` while loading).
    detail: Option<(usize, Result<Release, String>)>,
    /// Going back: do not offer the newer release again.
    skip_newer: bool,
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
            skip: Arc::new(Mutex::new(settings.skipped_version.clone())),
            versions: Arc::default(),
        };
        // Local builds have no built-in channel; release builds get it at build time.
        let enabled =
            settings.check_updates && std::env::var_os("GITKWAKEN_NO_UPDATE_CHECK").is_none();
        if enabled && let Some(url) = u.url.clone() {
            let s = u.state.clone();
            let c = ctx.clone();
            let skip = u.skip.clone();
            std::thread::spawn(move || {
                loop {
                    check(&url, &s, &c, false, &skip);
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
        let skip = self.skip.clone();
        std::thread::spawn(move || check(&url, &s, &c, true, &skip));
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
            State::Withdrawn(rel, _) => (
                format!("Roll back to {}", rel.version),
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
                if matches!(
                    *self.state.lock(),
                    State::Available(_) | State::Withdrawn(..)
                ) {
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
    pub fn dialog(&self, ctx: &egui::Context, settings: &mut gk_settings::Settings) {
        self.versions_dialog(ctx, settings);
        if !*self.asking.lock() {
            return;
        }
        // `Some(reason)`: the running version was withdrawn and `rel` is the way back.
        let (rel, withdrawn) = match &*self.state.lock() {
            State::Available(rel) => (rel.clone(), None),
            State::Withdrawn(rel, reason) => (rel.clone(), Some(reason.clone())),
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
        let mut skip = false;
        let (mut install, mut close) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let modal = egui::Modal::new(egui::Id::new("update-available")).show(ctx, |ui| {
            ui.set_width(520.0);
            let current = env!("CARGO_PKG_VERSION");
            if let Some(reason) = &withdrawn {
                ui.heading(format!("GitKwaken {current} was withdrawn"));
                ui.label(format!(
                    "This release was pulled after it went out{}. Going back to {} is \
                     recommended; settings and repositories are kept.",
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(": {reason}")
                    },
                    rel.version
                ));
            } else {
                ui.heading(format!("GitKwaken {} is available", rel.version));
                let published = rel
                    .published
                    .as_deref()
                    .map(|d| format!("Released {d}. "))
                    .unwrap_or_default();
                ui.label(egui::RichText::new(format!("{published}You have {current}.")).weak());
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(if withdrawn.is_some() {
                    format!("GitKwaken {}", rel.version)
                } else {
                    "What's new".to_string()
                })
                .strong(),
            );
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
                if withdrawn.is_none()
                    && ui
                        .button("Skip this version")
                        .on_hover_text("Don't offer this version again (a newer one will be)")
                        .clicked()
                {
                    skip = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Not now").clicked() {
                        close = true;
                    }
                    let label = if withdrawn.is_some() {
                        format!("Roll back to {}", rel.version)
                    } else {
                        "Install update".to_string()
                    };
                    let b =
                        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
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
        } else if skip {
            *self.asking.lock() = false;
            self.skip_version(&rel.version, settings);
        } else if close {
            *self.asking.lock() = false;
        }
    }

    /// Stops offering `version` (until a newer one is out).
    fn skip_version(&self, version: &str, settings: &mut gk_settings::Settings) {
        settings.skipped_version = version.to_string();
        *self.skip.lock() = version.to_string();
        let mut s = self.state.lock();
        if matches!(&*s, State::Available(r) if r.version == version) {
            *s = State::Idle;
        }
    }

    /// Help › Install another version…: lists the published releases.
    pub fn open_versions(&self, ctx: &egui::Context) {
        let Some(url) = self.url.clone() else {
            *self.state.lock() = State::Failed("This build has no update channel".into());
            return;
        };
        let mut v = self.versions.lock();
        *v = Versions {
            open: true,
            skip_newer: true,
            ..Default::default()
        };
        drop(v);
        let versions = self.versions.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = gk_update::list_releases(&url).map_err(|e| format!("{e:#}"));
            versions.lock().list = Some(r);
            c.request_repaint();
        });
    }

    fn select_version(&self, i: usize, ctx: &egui::Context) {
        let mut v = self.versions.lock();
        let Some(Ok(list)) = &v.list else { return };
        let Some(item) = list.get(i).cloned() else {
            return;
        };
        v.selected = Some(i);
        v.detail = None;
        drop(v);
        let versions = self.versions.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = gk_update::fetch_manifest(&item.manifest_url)
                .and_then(|rel| gk_update::method(&rel).map(|_| rel))
                .map_err(|e| format!("{e:#}"));
            let mut v = versions.lock();
            if v.selected == Some(i) {
                v.detail = Some((i, r));
            }
            c.request_repaint();
        });
    }

    /// The "Install another version" dialog: releases newest first, the selected one's
    /// changelog, Install. Going back to an older version can also skip the newer one.
    fn versions_dialog(&self, ctx: &egui::Context, settings: &mut gk_settings::Settings) {
        if !self.versions.lock().open {
            return;
        }
        let current = env!("CARGO_PKG_VERSION");
        let mut pick: Option<usize> = None;
        let mut install: Option<Release> = None;
        let mut close = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let busy = matches!(
            *self.state.lock(),
            State::Downloading(..) | State::Installing | State::Ready(_)
        );
        let modal = egui::Modal::new(egui::Id::new("update-versions")).show(ctx, |ui| {
            let mut v = self.versions.lock();
            ui.set_width(640.0);
            ui.heading("Install another version");
            ui.label(
                egui::RichText::new(format!(
                    "You have {current}. Pick a release to install, for example to go back \
                     after an update that went wrong."
                ))
                .weak(),
            );
            ui.add_space(6.0);
            let list = match &v.list {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading releases…");
                    });
                    None
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    None
                }
                Some(Ok(list)) => Some(list.clone()),
            };
            if let Some(list) = list {
                let latest = list
                    .iter()
                    .find(|l| !l.prerelease && !l.withdrawn)
                    .map(|l| l.version.clone());
                ui.horizontal_top(|ui| {
                    // Left: versions.
                    ui.vertical(|ui| {
                        ui.set_width(200.0);
                        egui::ScrollArea::vertical()
                            .id_salt("versions-list")
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for (i, l) in list.iter().enumerate() {
                                    let mut text = format!("{}   {}", l.version, l.published);
                                    if l.version == current {
                                        text.push_str("   (installed)");
                                    } else if latest.as_deref() == Some(l.version.as_str()) {
                                        text.push_str("   (latest)");
                                    }
                                    if l.withdrawn {
                                        text.push_str("   (withdrawn)");
                                    } else if l.prerelease && l.version != current {
                                        text.push_str("   (pre-release)");
                                    }
                                    if ui.selectable_label(v.selected == Some(i), text).clicked() {
                                        pick = Some(i);
                                    }
                                }
                            });
                    });
                    ui.add_space(12.0);
                    // Right: the selected release.
                    ui.vertical(|ui| {
                        let Some(i) = v.selected else {
                            ui.label(egui::RichText::new("Select a version.").weak());
                            return;
                        };
                        let l = &list[i];
                        ui.label(egui::RichText::new(format!("GitKwaken {}", l.version)).strong());
                        if l.withdrawn {
                            let why = if l.reason.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", l.reason)
                            };
                            ui.colored_label(ui.visuals().warn_fg_color, format!("Withdrawn{why}"));
                        }
                        let detail = v.detail.clone();
                        match &detail {
                            Some((di, Ok(rel))) if *di == i => {
                                egui::Frame::new()
                                    .fill(ui.visuals().extreme_bg_color)
                                    .corner_radius(4.0)
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        egui::ScrollArea::vertical()
                                            .id_salt("versions-notes")
                                            .max_height(240.0)
                                            .auto_shrink([false, true])
                                            .show(ui, |ui| changelog_ui(ui, &rel.notes));
                                    });
                                let older =
                                    gk_update::is_newer(&rel.version, current).unwrap_or(false);
                                if older {
                                    ui.label(
                                        egui::RichText::new(
                                            "Older than the installed version. Settings and \
                                             repositories are kept.",
                                        )
                                        .weak(),
                                    );
                                    if let Some(newest) = latest.as_deref()
                                        && newest != rel.version
                                    {
                                        ui.checkbox(
                                            &mut v.skip_newer,
                                            format!("Don't offer {newest} again"),
                                        );
                                    }
                                }
                                ui.add_space(4.0);
                                let same = rel.version == current;
                                let label = if same {
                                    "Reinstall".to_string()
                                } else {
                                    format!("Install {}", rel.version)
                                };
                                let b = egui::Button::new(
                                    egui::RichText::new(label).color(egui::Color32::WHITE),
                                )
                                .fill(ui.visuals().selection.bg_fill);
                                if ui
                                    .add_enabled(!busy, b)
                                    .on_disabled_hover_text("An update is already in progress")
                                    .clicked()
                                {
                                    install = Some(rel.clone());
                                }
                            }
                            Some((di, Err(e))) if *di == i => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            _ => {
                                ui.spinner();
                            }
                        }
                    });
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(i) = pick {
            self.select_version(i, ctx);
        }
        if let Some(rel) = install {
            let (skip_newer, newest) = {
                let v = self.versions.lock();
                let newest = match &v.list {
                    Some(Ok(list)) => list
                        .iter()
                        .find(|l| !l.prerelease && !l.withdrawn)
                        .map(|l| l.version.clone()),
                    _ => None,
                };
                (v.skip_newer, newest)
            };
            if skip_newer
                && gk_update::is_newer(&rel.version, current).unwrap_or(false)
                && let Some(newest) = newest
                && newest != rel.version
            {
                self.skip_version(&newest, settings);
            }
            close = true;
            self.install(rel, ctx);
        }
        if close {
            self.versions.lock().open = false;
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
            State::Withdrawn(rel, reason) => {
                let text = egui::RichText::new(format!("⚠ Roll back to {}", rel.version))
                    .color(egui::Color32::BLACK);
                let b = egui::Button::new(text).fill(egui::Color32::from_rgb(0xE8, 0xA7, 0x3D));
                let tip = if reason.is_empty() {
                    format!("GitKwaken {} was withdrawn", env!("CARGO_PKG_VERSION"))
                } else {
                    format!(
                        "GitKwaken {} was withdrawn: {reason}",
                        env!("CARGO_PKG_VERSION")
                    )
                };
                if ui.add(b).on_hover_text(tip).clicked() {
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

/// What a check of the channel found for the running `current` version.
enum Found {
    Newer(Release),
    /// `current` was withdrawn; the release is the newest good one, the `String` the reason.
    Withdrawn(Release, String),
    Nothing,
}

/// The channel's release is newer: offered, unless skipped (background checks only) or not
/// installable here (e.g. no Linux build in it). It is older: then `current` is a release that
/// was withdrawn after it went out (the channel moved back to the previous one) when the
/// release list says so, and the channel's release is the way back.
fn find(url: &str, current: &str, report: bool, skip: &Mutex<String>) -> anyhow::Result<Found> {
    let latest = gk_update::fetch_manifest(url)?;
    if gk_update::is_newer(current, &latest.version)? {
        if !report && *skip.lock() == latest.version {
            return Ok(Found::Nothing);
        }
        gk_update::method(&latest)?;
        return Ok(Found::Newer(latest));
    }
    if gk_update::is_newer(&latest.version, current)? {
        // Only GitHub channels list releases; elsewhere there is nothing to tell.
        if let Ok(list) = gk_update::list_releases(url)
            && let Some(me) = list.iter().find(|l| l.version == current && l.withdrawn)
            && gk_update::method(&latest).is_ok()
        {
            return Ok(Found::Withdrawn(latest, me.reason.clone()));
        }
    }
    Ok(Found::Nothing)
}

/// Checks the channel at `url`. A manual check (`report`) always shows its outcome; a
/// background one only announces a newer release, and only while nothing else is going on.
fn check(url: &str, state: &Mutex<State>, ctx: &egui::Context, report: bool, skip: &Mutex<String>) {
    let r = find(url, env!("CARGO_PKG_VERSION"), report, skip);
    let mut s = state.lock();
    let next = match r {
        Ok(Found::Newer(rel)) if report || s.quiet() => State::Available(rel),
        Ok(Found::Withdrawn(rel, why)) if report || s.quiet() => State::Withdrawn(rel, why),
        Ok(Found::Nothing) if report => State::UpToDate,
        Err(e) if report => State::Failed(format!("{e:#}")),
        _ => return,
    };
    *s = next;
    drop(s);
    ctx.request_repaint();
}
