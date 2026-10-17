//! Configurable keyboard shortcuts: named actions, their default bindings,
//! user overrides from `settings.keymap` ("Ctrl+Shift+P", "" = unbound) and
//! conflict detection. `app.rs` publishes the active keymap in the egui
//! context every frame; `app.rs` and `tab.rs` consume shortcuts through it.

use egui::{Context, Id, InputState, Key, Modifiers};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A named action that can be bound to a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Act {
    OpenRepo,
    CloseTab,
    NextTab,
    PrevTab,
    Palette,
    Settings,
    Refresh,
    ViewStatus,
    ViewHistory,
    ViewSearch,
    Commit,
    StageAll,
    UnstageAll,
    Fetch,
    Pull,
    Push,
    NewBranch,
    Stash,
    // File Status lists (while a list has keyboard focus).
    FileToggle,
    FileDiscard,
    FileHistory,
    FileBlame,
    FileExternalDiff,
    FileIgnore,
    // Diff view (while the diff body has keyboard focus).
    DiffNextHunk,
    DiffPrevHunk,
    DiffStage,
    DiffUnstage,
    DiffDiscard,
    DiffSelectDown,
    DiffSelectUp,
}

/// Where an action's shortcut is active. Scoped shortcuts only fire while
/// that widget has keyboard focus, so they may be bare keys ("S", "Delete")
/// and may repeat across scopes; global ones apply everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    FileList,
    Diff,
}

pub fn scope_of(a: Act) -> Scope {
    use Act::*;
    match a {
        FileToggle | FileDiscard | FileHistory | FileBlame | FileExternalDiff | FileIgnore => {
            Scope::FileList
        }
        DiffNextHunk | DiffPrevHunk | DiffStage | DiffUnstage | DiffDiscard | DiffSelectDown
        | DiffSelectUp => Scope::Diff,
        _ => Scope::Global,
    }
}

/// (action, settings id, label, default shortcut). New actions go at the
/// end: Settings › Keyboard lists them in this order.
pub const ACTIONS: &[(Act, &str, &str, &str)] = &[
    (Act::OpenRepo, "open_repo", "Open repository", "Ctrl+O"),
    (Act::CloseTab, "close_tab", "Close tab", "Ctrl+W"),
    (Act::NextTab, "next_tab", "Next tab", "Ctrl+Tab"),
    (Act::PrevTab, "prev_tab", "Previous tab", "Ctrl+Shift+Tab"),
    (Act::Palette, "palette", "Command palette", "Ctrl+Shift+P"),
    (Act::Settings, "settings", "Settings", "Ctrl+Comma"),
    (Act::Refresh, "refresh", "Refresh", "F5"),
    (Act::ViewStatus, "view_status", "File Status view", "Ctrl+1"),
    (Act::ViewHistory, "view_history", "History view", "Ctrl+2"),
    (Act::ViewSearch, "view_search", "Search view", "Ctrl+3"),
    (
        Act::Commit,
        "commit",
        "Commit (focus message)",
        "Ctrl+Shift+C",
    ),
    (Act::StageAll, "stage_all", "Stage all", "Ctrl+Shift+A"),
    (
        Act::UnstageAll,
        "unstage_all",
        "Unstage all",
        "Ctrl+Shift+U",
    ),
    (Act::Fetch, "fetch", "Fetch…", "Ctrl+Shift+F"),
    (Act::Pull, "pull", "Pull…", "Ctrl+Shift+L"),
    (Act::Push, "push", "Push…", "Ctrl+Shift+K"),
    (Act::NewBranch, "new_branch", "New branch…", "Ctrl+Shift+B"),
    (Act::Stash, "stash", "Stash…", "Ctrl+Shift+S"),
    (
        Act::FileToggle,
        "file_toggle",
        "File list: Stage / unstage selected",
        "Space",
    ),
    (
        Act::FileDiscard,
        "file_discard",
        "File list: Discard selected…",
        "Delete",
    ),
    (
        Act::FileHistory,
        "file_history",
        "File list: File history",
        "H",
    ),
    (Act::FileBlame, "file_blame", "File list: Blame", "B"),
    (
        Act::FileExternalDiff,
        "file_external_diff",
        "File list: Open in external diff tool",
        "E",
    ),
    (Act::FileIgnore, "file_ignore", "File list: Ignore…", "I"),
    (Act::DiffNextHunk, "diff_next_hunk", "Diff: Next hunk", "N"),
    (
        Act::DiffPrevHunk,
        "diff_prev_hunk",
        "Diff: Previous hunk",
        "P",
    ),
    (
        Act::DiffStage,
        "diff_stage",
        "Diff: Stage hunk / selected lines",
        "S",
    ),
    (
        Act::DiffUnstage,
        "diff_unstage",
        "Diff: Unstage hunk / selected lines",
        "U",
    ),
    (
        Act::DiffDiscard,
        "diff_discard",
        "Diff: Discard hunk / selected lines…",
        "Delete",
    ),
    (
        Act::DiffSelectDown,
        "diff_select_down",
        "Diff: Extend line selection down",
        "Shift+Down",
    ),
    (
        Act::DiffSelectUp,
        "diff_select_up",
        "Diff: Extend line selection up",
        "Shift+Up",
    ),
];

/// A key with exact modifiers. `ctrl` means Ctrl (Cmd on macOS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

impl Shortcut {
    /// Parses "Ctrl+Shift+P", "F5", "Ctrl+Comma", "alt+left" (case-insensitive modifiers).
    pub fn parse(text: &str) -> Result<Shortcut, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty shortcut".into());
        }
        let (mut ctrl, mut shift, mut alt) = (false, false, false);
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        // "Ctrl++" style is not supported; a trailing empty part is an error.
        let (key_part, mods) = parts.split_last().ok_or("empty shortcut")?;
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                "" => return Err(format!("invalid shortcut \"{text}\"")),
                other => return Err(format!("unknown modifier \"{other}\"")),
            }
        }
        let key = parse_key(key_part).ok_or_else(|| format!("unknown key \"{key_part}\""))?;
        Ok(Shortcut {
            ctrl,
            shift,
            alt,
            key,
        })
    }

    pub fn from_event(key: Key, m: Modifiers) -> Shortcut {
        Shortcut {
            ctrl: m.ctrl || m.command,
            shift: m.shift,
            alt: m.alt,
            key,
        }
    }

    fn matches(&self, key: Key, m: Modifiers) -> bool {
        self.key == key && *self == Shortcut::from_event(key, m)
    }

    /// Bare keys (no Ctrl/Alt) that would break typing in text fields.
    pub fn is_typing_key(&self) -> bool {
        if self.ctrl || self.alt {
            return false;
        }
        let name = self.key.name();
        // Function keys and navigation keys are fine on their own.
        !(name.starts_with('F') && name.len() > 1 && name[1..].chars().all(|c| c.is_ascii_digit()))
            && !matches!(
                self.key,
                Key::Escape
                    | Key::Insert
                    | Key::Delete
                    | Key::Home
                    | Key::End
                    | Key::PageUp
                    | Key::PageDown
            )
    }
}

impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        f.write_str(self.key.name())
    }
}

fn parse_key(s: &str) -> Option<Key> {
    if let Some(k) = Key::from_name(s) {
        return Some(k);
    }
    // Case-insensitive fallback ("p", "f5", "comma", "enter").
    let mut c = s.chars();
    let first = c.next()?;
    let cap: String = first
        .to_uppercase()
        .chain(c.flat_map(char::to_lowercase))
        .collect();
    Key::from_name(&cap).or_else(|| Key::from_name(&s.to_uppercase()))
}

/// The resolved keymap: one optional shortcut per action.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    bindings: Vec<(Act, Option<Shortcut>)>,
    /// Override texts that did not parse: (settings id, error).
    pub errors: Vec<(String, String)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_overrides(&BTreeMap::new())
    }
}

pub fn id_of(a: Act) -> &'static str {
    ACTIONS.iter().find(|x| x.0 == a).map(|x| x.1).unwrap_or("")
}

pub fn label_of(a: Act) -> &'static str {
    ACTIONS.iter().find(|x| x.0 == a).map(|x| x.2).unwrap_or("")
}

pub fn default_of(a: Act) -> Option<Shortcut> {
    ACTIONS
        .iter()
        .find(|x| x.0 == a)
        .and_then(|x| Shortcut::parse(x.3).ok())
}

impl Keymap {
    /// Defaults with `overrides` (settings id -> shortcut text; empty or
    /// "none" unbinds). Invalid texts keep the default and are reported.
    pub fn from_overrides(overrides: &BTreeMap<String, String>) -> Keymap {
        let mut errors = Vec::new();
        let bindings = ACTIONS
            .iter()
            .map(|&(act, id, _, def)| {
                let sc = match overrides.get(id).map(|s| s.trim()) {
                    None => Shortcut::parse(def).ok(),
                    Some("") => None,
                    Some(t) if t.eq_ignore_ascii_case("none") => None,
                    Some(t) => match Shortcut::parse(t) {
                        Ok(s) => Some(s),
                        Err(e) => {
                            errors.push((id.to_string(), e));
                            Shortcut::parse(def).ok()
                        }
                    },
                };
                (act, sc)
            })
            .collect();
        for k in overrides.keys() {
            if !ACTIONS.iter().any(|a| a.1 == k) {
                errors.push((k.clone(), "unknown action".into()));
            }
        }
        Keymap { bindings, errors }
    }

    pub fn get(&self, a: Act) -> Option<Shortcut> {
        self.bindings.iter().find(|b| b.0 == a).and_then(|b| b.1)
    }

    /// Text for menus and tooltips ("" when unbound).
    pub fn text(&self, a: Act) -> String {
        self.get(a).map(|s| s.to_string()).unwrap_or_default()
    }

    /// Pairs of actions sharing a shortcut where both can fire: same scope,
    /// or one of them global (global shortcuts are taken first).
    pub fn conflicts(&self) -> Vec<(Act, Act, Shortcut)> {
        let mut out = Vec::new();
        for (i, (a, sa)) in self.bindings.iter().enumerate() {
            let Some(sa) = sa else { continue };
            for (b, sb) in &self.bindings[i + 1..] {
                let (x, y) = (scope_of(*a), scope_of(*b));
                let overlap = x == y || x == Scope::Global || y == Scope::Global;
                if overlap && Some(*sa) == *sb {
                    out.push((*a, *b, *sa));
                }
            }
        }
        out
    }

    /// Consumes the key press bound to `a` (exact modifiers). Returns true when pressed.
    pub fn consume(&self, i: &mut InputState, a: Act) -> bool {
        let Some(sc) = self.get(a) else {
            return false;
        };
        let mut hit = false;
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if !hit && sc.matches(*key, *modifiers) => {
                hit = true;
                false
            }
            _ => true,
        });
        hit
    }

    /// Consumes the presses of every action in `acts` (in that order);
    /// returns the ones pressed this frame.
    pub fn consume_all(&self, i: &mut InputState, acts: &[Act]) -> Vec<Act> {
        acts.iter()
            .copied()
            .filter(|a| self.consume(i, *a))
            .collect()
    }
}

/// Scoped shortcuts pressed this frame (consumed), or nothing while
/// shortcuts are suspended (a dialog or the palette is open).
pub fn pressed(ctx: &Context, acts: &[Act]) -> Vec<Act> {
    match active(ctx) {
        Some(km) => ctx.input_mut(|i| km.consume_all(i, acts)),
        None => Vec::new(),
    }
}

/// The published keymap, also while shortcuts are suspended (for labels).
pub fn current(ctx: &Context) -> Arc<Keymap> {
    ctx.data(|d| d.get_temp::<(Arc<Keymap>, bool)>(data_id()))
        .map(|(km, _)| km)
        .unwrap_or_default()
}

/// Shortcut text of `a` in the published keymap ("" when unbound), for menus.
pub fn text(ctx: &Context, a: Act) -> String {
    current(ctx).text(a)
}

/// Builds the settings-map form of `bindings` (only entries differing from the defaults).
pub fn to_overrides(bindings: &[(Act, Option<Shortcut>)]) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (a, sc) in bindings {
        if *sc != default_of(*a) {
            m.insert(
                id_of(*a).to_string(),
                sc.map(|s| s.to_string()).unwrap_or_default(),
            );
        }
    }
    m
}

impl Keymap {
    pub fn bindings(&self) -> &[(Act, Option<Shortcut>)] {
        &self.bindings
    }
}

fn data_id() -> Id {
    Id::new("gk-keymap")
}

/// Publishes the active keymap (and whether shortcuts are suspended, e.g. a
/// global dialog or the palette is open) for this frame.
pub fn publish(ctx: &Context, map: Arc<Keymap>, suspended: bool) {
    ctx.data_mut(|d| d.insert_temp(data_id(), (map, suspended)));
}

/// The published keymap; `None` while shortcuts are suspended.
pub fn active(ctx: &Context) -> Option<Arc<Keymap>> {
    let (map, suspended) = ctx
        .data(|d| d.get_temp::<(Arc<Keymap>, bool)>(data_id()))
        .unwrap_or_else(|| (Arc::new(Keymap::default()), false));
    (!suspended).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_roundtrip() {
        for t in [
            "Ctrl+Shift+P",
            "F5",
            "Ctrl+Comma",
            "Alt+Left",
            "Ctrl+1",
            "Ctrl+Enter",
        ] {
            let s = Shortcut::parse(t).unwrap();
            assert_eq!(Shortcut::parse(&s.to_string()).unwrap(), s, "{t}");
        }
        let s = Shortcut::parse("ctrl+shift+p").unwrap();
        assert!(s.ctrl && s.shift && !s.alt && s.key == Key::P);
        assert_eq!(s.to_string(), "Ctrl+Shift+P");
        assert!(Shortcut::parse("Hyper+P").is_err());
        assert!(Shortcut::parse("Ctrl+").is_err());
        assert!(Shortcut::parse("").is_err());
        assert!(Shortcut::parse("Ctrl+NoSuchKey").is_err());
        assert!(Shortcut::parse("P").unwrap().is_typing_key());
        assert!(!Shortcut::parse("F5").unwrap().is_typing_key());
        assert!(!Shortcut::parse("Ctrl+P").unwrap().is_typing_key());
    }

    #[test]
    fn defaults_have_no_conflicts() {
        let k = Keymap::default();
        assert!(k.conflicts().is_empty(), "{:?}", k.conflicts());
        assert!(k.errors.is_empty());
        assert_eq!(k.text(Act::Palette), "Ctrl+Shift+P");
        for (a, ..) in ACTIONS {
            assert!(default_of(*a).is_some(), "{a:?} default parses");
        }
    }

    #[test]
    fn scoped_shortcuts_conflict_only_where_both_fire() {
        assert_eq!(scope_of(Act::Fetch), Scope::Global);
        assert_eq!(scope_of(Act::FileDiscard), Scope::FileList);
        assert_eq!(scope_of(Act::DiffDiscard), Scope::Diff);
        // Delete in the file list and in the diff: different widgets.
        assert_eq!(
            Keymap::default().get(Act::FileDiscard),
            Keymap::default().get(Act::DiffDiscard)
        );
        let mut o = BTreeMap::new();
        o.insert("diff_stage".to_string(), "H".to_string());
        assert!(
            Keymap::from_overrides(&o).conflicts().is_empty(),
            "file list H vs diff H"
        );
        o.insert("diff_unstage".to_string(), "S".to_string());
        let c = Keymap::from_overrides(&o).conflicts();
        assert_eq!(c.len(), 0, "S is free in the diff once Stage moved to H");
        o.insert("file_blame".to_string(), "Ctrl+Shift+K".to_string());
        let c = Keymap::from_overrides(&o).conflicts();
        assert_eq!((c[0].0, c[0].1), (Act::Push, Act::FileBlame), "global wins");
        o.insert("diff_next_hunk".to_string(), "S".to_string());
        assert_eq!(Keymap::from_overrides(&o).conflicts().len(), 2);
    }

    #[test]
    fn overrides_unbind_report_and_conflict() {
        let mut o = BTreeMap::new();
        o.insert("fetch".to_string(), "Ctrl+Shift+P".to_string());
        o.insert("refresh".to_string(), String::new());
        o.insert("push".to_string(), "Ctrl+Bogus".to_string());
        o.insert("nope".to_string(), "F1".to_string());
        let k = Keymap::from_overrides(&o);
        assert_eq!(k.get(Act::Refresh), None);
        assert_eq!(k.text(Act::Push), "Ctrl+Shift+K", "invalid keeps default");
        assert_eq!(k.errors.len(), 2);
        let c = k.conflicts();
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].0, c[0].1), (Act::Palette, Act::Fetch));
        // Back to the overrides form: only non-defaults.
        let back = to_overrides(k.bindings());
        assert_eq!(back.get("fetch").map(String::as_str), Some("Ctrl+Shift+P"));
        assert_eq!(back.get("refresh").map(String::as_str), Some(""));
        assert!(!back.contains_key("push"));
    }

    #[test]
    fn consume_needs_exact_modifiers() {
        let k = Keymap::default();
        let press = |key, m: Modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        let mut i = InputState::default();
        i.events
            .push(press(Key::P, Modifiers::COMMAND | Modifiers::CTRL));
        assert!(
            !k.consume(&mut i, Act::Palette),
            "Ctrl+P is not Ctrl+Shift+P"
        );
        i.events.push(press(
            Key::P,
            Modifiers::COMMAND | Modifiers::CTRL | Modifiers::SHIFT,
        ));
        assert!(k.consume(&mut i, Act::Palette));
        assert_eq!(i.events.len(), 1, "only the matching press is consumed");
        i.events.push(press(Key::F5, Modifiers::NONE));
        assert!(k.consume(&mut i, Act::Refresh));
    }
}
