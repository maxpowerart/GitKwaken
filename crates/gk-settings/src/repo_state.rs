//! Per-repository UI state stored in `<config>/repos/<hash>.toml`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::Settings;
use crate::io::{fnv1a64, load_or_default, path_key, save_atomic};

pub const MAX_RECENT_MESSAGES: usize = 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoState {
    pub commit_message_draft: String,
    /// Newest first, deduplicated, capped at [`MAX_RECENT_MESSAGES`].
    pub recent_messages: Vec<String>,
    pub sidebar_collapsed: BTreeSet<String>,
    /// "all" | "current"
    pub history_scope: String,
    pub layout: Layout,
    pub last_view: String,
    pub file_view_tree: bool,
    /// "Don't ask for this repository" on the large-history commit-graph offer.
    pub commit_graph_offer_dismissed: bool,
}

impl Default for RepoState {
    fn default() -> Self {
        Self {
            commit_message_draft: String::new(),
            recent_messages: Vec::new(),
            sidebar_collapsed: BTreeSet::new(),
            history_scope: "all".into(),
            layout: Layout::default(),
            last_view: "history".into(),
            file_view_tree: false,
            commit_graph_offer_dismissed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_width: f32,
    /// Fraction of height for the commit list.
    pub list_split: f32,
    /// Fraction of width for the file list in the detail pane.
    pub detail_split: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: 240.0,
            list_split: 0.55,
            detail_split: 0.35,
        }
    }
}

impl RepoState {
    /// Never fails; missing/corrupt files yield defaults.
    pub fn load(workdir: &Path) -> RepoState {
        let mut s: RepoState = load_or_default(&Self::path_for(workdir));
        s.normalize();
        s
    }

    pub fn save(&self, workdir: &Path) -> Result<()> {
        save_atomic(self, &Self::path_for(workdir))
    }

    /// `<config>/repos/<16-hex>.toml`, keyed by the canonical workdir.
    pub fn path_for(workdir: &Path) -> PathBuf {
        Settings::config_dir()
            .join("repos")
            .join(format!("{}.toml", Self::key_for(workdir)))
    }

    /// 16-hex stable hash of the canonicalized workdir.
    pub fn key_for(workdir: &Path) -> String {
        let canon = dunce_canonicalize(workdir);
        format!("{:016x}", fnv1a64(path_key(&canon).as_bytes()))
    }

    /// Adds a message to the front of the history (trimmed, deduped, capped).
    pub fn push_recent_message(&mut self, msg: impl AsRef<str>) {
        let msg = msg.as_ref().trim();
        if msg.is_empty() {
            return;
        }
        self.recent_messages.retain(|m| m != msg);
        self.recent_messages.insert(0, msg.to_owned());
        self.recent_messages.truncate(MAX_RECENT_MESSAGES);
    }

    fn normalize(&mut self) {
        self.recent_messages.truncate(MAX_RECENT_MESSAGES);
        if self.history_scope != "all" && self.history_scope != "current" {
            self.history_scope = "all".into();
        }
    }
}

/// Canonicalize, stripping the Windows `\\?\` verbatim prefix; falls back to
/// the input path if it doesn't exist.
fn dunce_canonicalize(p: &Path) -> PathBuf {
    let Ok(c) = std::fs::canonicalize(p) else {
        return p.to_path_buf();
    };
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        c
    }
}
