//! Persistent application settings and per-repository UI state (TOML).
//!
//! Location: `%APPDATA%\GitKwaken\settings.toml` on Windows (platform config
//! dir elsewhere), overridable with `GITKWAKEN_CONFIG_DIR`.

mod autosave;
mod io;
mod repo_state;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use autosave::{Autosave, DEFAULT_AUTOSAVE_INTERVAL};
pub use repo_state::{Layout, MAX_RECENT_MESSAGES, RepoState};

use io::{load_or_default, same_path, save_atomic};

pub const CONFIG_DIR_ENV: &str = "GITKWAKEN_CONFIG_DIR";
pub const SETTINGS_FILE: &str = "settings.toml";
pub const MAX_RECENT_REPOS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub bookmarks: Vec<Bookmark>,
    pub groups: Vec<String>,
    pub open_tabs: Vec<PathBuf>,
    pub active_tab: usize,
    pub window: WindowState,
    pub theme: Theme,
    pub ui_scale: f32,
    pub font_size: f32,
    pub mono_font_size: f32,
    pub diff: DiffSettings,
    pub status_engine: StatusEngine,
    pub show_untracked: bool,
    /// 0 = off.
    pub auto_fetch_minutes: u32,
    pub git_path: Option<PathBuf>,
    pub external_diff_tool: Option<ExternalTool>,
    pub external_merge_tool: Option<ExternalTool>,
    pub terminal: Option<ExternalTool>,
    pub commit: CommitSettings,
    pub history: HistorySettings,
    /// Most recent first, max [`MAX_RECENT_REPOS`].
    pub recent_repos: Vec<PathBuf>,
    pub confirmations: Confirmations,
    /// action -> shortcut text.
    pub keymap: BTreeMap<String, String>,
    pub language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bookmarks: Vec::new(),
            groups: Vec::new(),
            open_tabs: Vec::new(),
            active_tab: 0,
            window: WindowState::default(),
            theme: Theme::default(),
            ui_scale: 1.0,
            font_size: 13.0,
            mono_font_size: 12.5,
            diff: DiffSettings::default(),
            status_engine: StatusEngine::default(),
            show_untracked: true,
            auto_fetch_minutes: 10,
            git_path: None,
            external_diff_tool: None,
            external_merge_tool: None,
            terminal: None,
            commit: CommitSettings::default(),
            history: HistorySettings::default(),
            recent_repos: Vec::new(),
            confirmations: Confirmations::default(),
            keymap: BTreeMap::new(),
            language: "en".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
    pub group: Option<String>,
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: PathBuf::new(),
            group: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub position: Option<[f32; 2]>,
    pub size: [f32; 2],
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            position: None,
            size: [1400.0, 900.0],
            maximized: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusEngine {
    #[default]
    Gix,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffSettings {
    pub context_lines: u32,
    pub ignore_whitespace: bool,
    pub word_diff: bool,
    pub side_by_side: bool,
    pub tab_width: u32,
    pub syntax_highlight: bool,
    /// Files larger than this are not diffed inline.
    pub max_file_kb: u64,
}

impl Default for DiffSettings {
    fn default() -> Self {
        Self {
            context_lines: 3,
            ignore_whitespace: false,
            word_diff: true,
            side_by_side: false,
            tab_width: 4,
            syntax_highlight: true,
            max_file_kb: 20480,
        }
    }
}

/// External program. `args` is a template with `$LOCAL $REMOTE $BASE $MERGED`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalTool {
    pub name: String,
    pub command: String,
    pub args: String,
}

impl ExternalTool {
    /// Expands `$LOCAL $REMOTE $BASE $MERGED` in `args` (paths are quoted).
    pub fn expand_args(&self, local: &str, remote: &str, base: &str, merged: &str) -> String {
        let q = |s: &str| format!("\"{s}\"");
        self.args
            .replace("$LOCAL", &q(local))
            .replace("$REMOTE", &q(remote))
            .replace("$BASE", &q(base))
            .replace("$MERGED", &q(merged))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommitSettings {
    pub signoff: bool,
    /// None = follow git config `commit.gpgsign`.
    pub sign: Option<bool>,
    pub subject_guide_column: u32,
    pub body_wrap_column: u32,
    pub push_after_commit: bool,
}

impl Default for CommitSettings {
    fn default() -> Self {
        Self {
            signoff: false,
            sign: None,
            subject_guide_column: 50,
            body_wrap_column: 72,
            push_after_commit: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub show_all_branches: bool,
    /// strftime-style.
    pub date_format: String,
    pub relative_dates: bool,
    pub show_remote_branches: bool,
    pub show_tags: bool,
    pub show_stashes: bool,
    pub graph_lane_width: f32,
    pub row_height: f32,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            show_all_branches: true,
            date_format: "%Y-%m-%d %H:%M".into(),
            relative_dates: false,
            show_remote_branches: true,
            show_tags: true,
            show_stashes: true,
            graph_lane_width: 14.0,
            row_height: 22.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Confirmations {
    pub discard: bool,
    pub hard_reset: bool,
    pub force_push: bool,
    pub delete_branch: bool,
}

impl Default for Confirmations {
    fn default() -> Self {
        Self {
            discard: true,
            hard_reset: true,
            force_push: true,
            delete_branch: true,
        }
    }
}

impl Settings {
    /// Config dir: `$GITKWAKEN_CONFIG_DIR`, else `%APPDATA%\GitKwaken` on
    /// Windows / platform config dir elsewhere, else `./.gitkwaken`.
    pub fn config_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV).filter(|v| !v.is_empty()) {
            return PathBuf::from(dir);
        }
        if cfg!(windows) {
            // ProjectDirs on Windows appends `\config`; we want the bare app dir.
            if let Some(b) = directories::BaseDirs::new() {
                return b.config_dir().join("GitKwaken");
            }
        } else if let Some(p) = directories::ProjectDirs::from("", "", "GitKwaken") {
            return p.config_dir().to_path_buf();
        }
        PathBuf::from(".gitkwaken")
    }

    pub fn path() -> PathBuf {
        Self::config_dir().join(SETTINGS_FILE)
    }

    /// Never fails: corrupt files are backed up and defaults returned.
    pub fn load() -> Settings {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Settings {
        let mut s: Settings = load_or_default(path);
        s.normalize();
        s
    }

    /// Atomic write (temp file + rename), creating directories as needed.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        save_atomic(self, path)
    }

    /// Clamps out-of-range values from hand-edited files.
    fn normalize(&mut self) {
        self.recent_repos.truncate(MAX_RECENT_REPOS);
        if self.active_tab >= self.open_tabs.len() {
            self.active_tab = self.open_tabs.len().saturating_sub(1);
        }
        let d = Settings::default();
        let fix = |v: &mut f32, lo: f32, hi: f32, def: f32| {
            if !v.is_finite() || *v < lo || *v > hi {
                *v = def;
            }
        };
        fix(&mut self.ui_scale, 0.5, 4.0, d.ui_scale);
        fix(&mut self.font_size, 6.0, 72.0, d.font_size);
        fix(&mut self.mono_font_size, 6.0, 72.0, d.mono_font_size);
        fix(
            &mut self.history.row_height,
            10.0,
            100.0,
            d.history.row_height,
        );
        fix(
            &mut self.history.graph_lane_width,
            4.0,
            100.0,
            d.history.graph_lane_width,
        );
        if self.diff.tab_width == 0 {
            self.diff.tab_width = d.diff.tab_width;
        }
    }

    /// Moves/inserts `path` to the front of the recent list (dedup, max 20).
    pub fn add_recent(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.recent_repos.retain(|p| !same_path(p, &path));
        self.recent_repos.insert(0, path);
        self.recent_repos.truncate(MAX_RECENT_REPOS);
    }

    pub fn remove_recent(&mut self, path: &Path) {
        self.recent_repos.retain(|p| !same_path(p, path));
    }

    pub fn find_bookmark(&self, path: &Path) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| same_path(&b.path, path))
    }

    fn find_bookmark_mut(&mut self, path: &Path) -> Option<&mut Bookmark> {
        self.bookmarks.iter_mut().find(|b| same_path(&b.path, path))
    }

    /// Adds a bookmark; returns false if `path` is already bookmarked.
    /// An empty `name` defaults to the folder name. Unknown groups are created.
    pub fn add_bookmark(
        &mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        group: Option<String>,
    ) -> bool {
        let path = path.into();
        if self.find_bookmark(&path).is_some() {
            return false;
        }
        let mut name = name.into();
        if name.trim().is_empty() {
            name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
        }
        if let Some(g) = &group {
            self.add_group(g.clone());
        }
        self.bookmarks.push(Bookmark { name, path, group });
        true
    }

    pub fn remove_bookmark(&mut self, path: &Path) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|b| !same_path(&b.path, path));
        self.bookmarks.len() != before
    }

    pub fn rename_bookmark(&mut self, path: &Path, new_name: impl Into<String>) -> bool {
        let new_name = new_name.into();
        if new_name.trim().is_empty() {
            return false;
        }
        match self.find_bookmark_mut(path) {
            Some(b) => {
                b.name = new_name;
                true
            }
            None => false,
        }
    }

    /// `None` moves to the ungrouped section. Unknown groups are created.
    pub fn move_bookmark_to_group(&mut self, path: &Path, group: Option<String>) -> bool {
        if self.find_bookmark(path).is_none() {
            return false;
        }
        if let Some(g) = &group {
            self.add_group(g.clone());
        }
        if let Some(b) = self.find_bookmark_mut(path) {
            b.group = group;
        }
        true
    }

    /// Returns false if the group exists or the name is blank.
    pub fn add_group(&mut self, name: impl Into<String>) -> bool {
        let name = name.into();
        if name.trim().is_empty() || self.groups.contains(&name) {
            return false;
        }
        self.groups.push(name);
        true
    }

    /// Removes a group; its bookmarks become ungrouped.
    pub fn remove_group(&mut self, name: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g != name);
        for b in &mut self.bookmarks {
            if b.group.as_deref() == Some(name) {
                b.group = None;
            }
        }
        self.groups.len() != before
    }

    pub fn rename_group(&mut self, old: &str, new: impl Into<String>) -> bool {
        let new = new.into();
        if new.trim().is_empty() || self.groups.contains(&new) {
            return false;
        }
        let Some(g) = self.groups.iter_mut().find(|g| *g == old) else {
            return false;
        };
        *g = new.clone();
        for b in &mut self.bookmarks {
            if b.group.as_deref() == Some(old) {
                b.group = Some(new.clone());
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
