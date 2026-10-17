//! Non-modal banner in a repository tab: a large history (more than
//! [`COMMIT_GRAPH_OFFER_MIN_COMMITS`]) without a commit-graph gets the offer
//! to write one. The UI thread only compares the history length it already
//! has; the on-disk check runs once on a short-lived thread. "Write" queues
//! the write on the session's operation queue; "Don't ask" is remembered per
//! repository (`RepoState::commit_graph_offer_dismissed`).

use crate::tab::RepoTab;
use egui::RichText;
use gk_core::Snapshot;
use gk_core::advanced::{COMMIT_GRAPH_OFFER_MIN_COMMITS, offer_commit_graph};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
pub enum Offer {
    /// Waiting for a (fully loaded) history above the threshold.
    #[default]
    Idle,
    /// Looking for the commit-graph files; `Some(has_graph)` when done.
    Checking(Arc<Mutex<Option<bool>>>),
    Shown,
    /// Written, dismissed for this session, or not needed.
    Done,
}

/// What the user clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Write,
    NotNow,
    Never,
}

/// Advances the offer state; returns true when the banner should show.
fn step(tab: &mut RepoTab, snap: &Snapshot, ctx: &egui::Context) -> bool {
    if tab.state.commit_graph_offer_dismissed {
        return false;
    }
    let commits = snap.history.len();
    let next = match &tab.commit_graph_offer {
        Offer::Done => return false,
        Offer::Shown => return true,
        Offer::Idle => {
            // Cheap: no I/O until the history is known to be large.
            if snap.history_partial || commits <= COMMIT_GRAPH_OFFER_MIN_COMMITS {
                return false;
            }
            let slot: Arc<Mutex<Option<bool>>> = Arc::default();
            let (s2, repo, ctx) = (slot.clone(), tab.session.repo().clone(), ctx.clone());
            let spawned = std::thread::Builder::new()
                .name("gk-commit-graph-check".into())
                .spawn(move || {
                    *s2.lock() = Some(repo.has_commit_graph());
                    ctx.request_repaint();
                });
            if spawned.is_err() {
                Offer::Done
            } else {
                Offer::Checking(slot)
            }
        }
        Offer::Checking(slot) => {
            let Some(has) = *slot.lock() else {
                return false;
            };
            if offer_commit_graph(commits, has) {
                Offer::Shown
            } else {
                Offer::Done
            }
        }
    };
    let show = matches!(next, Offer::Shown);
    tab.commit_graph_offer = next;
    show
}

/// The banner (a top panel under the toolbar), when due.
pub fn show(ui: &mut egui::Ui, tab: &mut RepoTab, snap: &Snapshot) {
    if !step(tab, snap, ui.ctx()) {
        return;
    }
    let mut choice = None;
    let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.22);
    egui::Panel::top(egui::Id::new(("commit-graph-offer", &tab.path)))
        .frame(
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ℹ").strong());
                ui.label(format!(
                    "Large history ({} commits) without a commit-graph — write it now?",
                    snap.history.len()
                ))
                .on_hover_text(
                    "A commit-graph makes the history graph, log, blame and file history much faster. \
                     Runs `git commit-graph write --reachable --changed-paths` in the background.",
                );
                if ui.button(RichText::new("Write").strong()).clicked() {
                    choice = Some(Choice::Write);
                }
                if ui.button("Not now").clicked() {
                    choice = Some(Choice::NotNow);
                }
                if ui.button("Don't ask for this repository").clicked() {
                    choice = Some(Choice::Never);
                }
            });
        });
    match choice {
        Some(Choice::Write) => {
            tab.session.run_op("Write commit-graph", |repo, cancel, p| {
                repo.write_commit_graph(cancel, p).map(|_| String::new())
            });
            tab.commit_graph_offer = Offer::Done;
        }
        Some(Choice::NotNow) => tab.commit_graph_offer = Offer::Done,
        Some(Choice::Never) => {
            tab.commit_graph_offer = Offer::Done;
            tab.state.commit_graph_offer_dismissed = true;
            let _ = tab.state.save(&tab.path);
        }
        None => {}
    }
}
