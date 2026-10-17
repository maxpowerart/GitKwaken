//! Optimize › file system monitor daemon: shows whether git's builtin
//! fsmonitor daemon serves this repository and starts / stops it.

use super::common::{Task, error, hint};
use egui::{Color32, Context, RichText, Ui};
use gk_status::FsMonitor;

const RUNNING: Color32 = Color32::from_rgb(0x3F, 0xA3, 0x4D);

/// State of the daemon row; lives in the Optimize dialog.
#[derive(Default)]
pub struct DaemonPanel {
    state: Option<FsMonitor>,
    query: Option<Task<FsMonitor>>,
    action: Option<Task<Result<FsMonitor, String>>>,
    error: Option<String>,
    /// `core.fsmonitor` as the dialog last reported it; a change re-queries.
    configured: Option<bool>,
}

/// (label, hint) for a daemon state.
pub fn describe(s: FsMonitor) -> (&'static str, &'static str) {
    match s {
        FsMonitor::Off => (
            "not used",
            "core.fsmonitor is off for this repository; \"Optimize now\" turns it on.",
        ),
        FsMonitor::Configured => (
            "stopped",
            "core.fsmonitor is on; the next full status starts the daemon, or start it now.",
        ),
        FsMonitor::Running => (
            "running",
            "Full status scans ask the daemon which files changed instead of checking every file.",
        ),
    }
}

impl DaemonPanel {
    /// One row: state, Start / Stop / Refresh. `configured` is the dialog's
    /// view of `core.fsmonitor`; when it changes (after "Optimize now") the
    /// daemon state is queried again.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &Context, git: &gk_cli::Git, configured: bool) {
        if cfg!(not(any(windows, target_os = "macos"))) {
            return;
        }
        let refresh = self.configured.replace(configured) != Some(configured);
        if let Some(t) = &mut self.query
            && let Some(s) = t.take()
        {
            self.query = None;
            self.state = Some(s);
        }
        if let Some(t) = &mut self.action
            && let Some(r) = t.take()
        {
            self.action = None;
            match r {
                Ok(s) => self.state = Some(s),
                Err(e) => self.error = Some(e),
            }
        }
        let busy = self.query.is_some() || self.action.is_some();
        if (self.state.is_none() || refresh) && self.query.is_none() {
            let g = git.clone();
            self.query = Some(Task::spawn(ctx, move || gk_status::fsmonitor_state(&g)));
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("File system monitor daemon:").strong());
            match self.state {
                None => {
                    ui.spinner();
                }
                Some(s) => {
                    let (label, _) = describe(s);
                    let color = if s == FsMonitor::Running {
                        RUNNING
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.label(RichText::new(format!("● {label}")).color(color));
                }
            }
            if busy {
                ui.spinner();
            }
            let state = self.state;
            // A daemon is only useful when core.fsmonitor tells git to ask it.
            let start = state == Some(FsMonitor::Configured);
            let stop = state == Some(FsMonitor::Running);
            let mut run = None;
            if ui
                .add_enabled(!busy && start, egui::Button::new("Start"))
                .on_hover_text("git fsmonitor--daemon start")
                .clicked()
            {
                run = Some(true);
            }
            if ui
                .add_enabled(!busy && stop, egui::Button::new("Stop"))
                .on_hover_text("git fsmonitor--daemon stop")
                .clicked()
            {
                run = Some(false);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("⟳"))
                .on_hover_text("Check again")
                .clicked()
            {
                self.state = None;
            }
            if let Some(start) = run {
                self.error = None;
                let g = git.clone();
                self.action = Some(Task::spawn(ctx, move || {
                    gk_status::fsmonitor_daemon(&g, start)
                        .map(|()| gk_status::fsmonitor_state(&g))
                        .map_err(|e| format!("{e:#}"))
                }));
            }
        });
        if let Some(s) = self.state {
            hint(ui, describe(s).1);
        }
        if let Some(e) = &self.error {
            error(ui, e.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_described() {
        assert_eq!(describe(FsMonitor::Running).0, "running");
        assert_eq!(describe(FsMonitor::Configured).0, "stopped");
        assert_eq!(describe(FsMonitor::Off).0, "not used");
    }
}
