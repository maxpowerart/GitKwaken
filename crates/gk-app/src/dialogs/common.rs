//! Shared dialog plumbing: the modal frame with OK/Cancel and keyboard
//! handling, background tasks, small widgets and pure helpers (validation,
//! argument splitting, rebase todo manipulation).

use crate::tab::RepoTab;
use egui::{Color32, Context, Id, Key, Modifiers, RichText, Ui};
use gk_core::Snapshot;
use gk_core::ops::TodoAction;
use parking_lot::Mutex;
use std::sync::Arc;

pub const DANGER: Color32 = Color32::from_rgb(0xD0, 0x3B, 0x3B);
pub const WARN: Color32 = Color32::from_rgb(0xE0, 0x9A, 0x2A);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still open.
    None,
    Ok,
    Cancel,
}

/// Per-frame controls of the modal frame, set by the dialog body.
pub struct Ctl {
    pub ok_label: String,
    pub ok_enabled: bool,
    /// Red OK button for destructive actions.
    pub danger: bool,
    pub show_ok: bool,
    pub cancel_label: &'static str,
    /// Enter triggers OK (unless a multi-line editor has focus).
    pub enter_ok: bool,
    pub multiline_focus: bool,
    /// True on the first frame the dialog is shown (to focus a field).
    pub first: bool,
    /// Set by extra buttons in the body: closes the dialog.
    pub close: bool,
    /// Set by extra buttons in the body: acts like OK.
    pub ok: bool,
}

impl Ctl {
    pub fn invalid(&mut self) {
        self.ok_enabled = false;
    }

    /// Call with the response of a multi-line editor so Enter inserts a newline.
    pub fn multiline(&mut self, r: &egui::Response) {
        if r.has_focus() {
            self.multiline_focus = true;
        }
    }

    pub fn focus(&self, r: &egui::Response) {
        if self.first {
            r.request_focus();
        }
    }
}

/// Shows a modal dialog with a title, the body and OK/Cancel buttons.
/// Escape cancels, Enter (Ctrl+Enter in multi-line editors) confirms.
pub fn modal<T>(
    ctx: &Context,
    id: &str,
    title: &str,
    width: f32,
    ok_label: &str,
    body: impl FnOnce(&mut Ui, &mut Ctl) -> T,
) -> (Outcome, T) {
    let id = Id::new(("gk-dialog", id));
    let frame = ctx.cumulative_frame_nr();
    let last = ctx.data_mut(|d| {
        let last = d.get_temp::<u64>(id);
        d.insert_temp(id, frame);
        last
    });
    let mut ctl = Ctl {
        ok_label: ok_label.to_string(),
        ok_enabled: true,
        danger: false,
        show_ok: true,
        cancel_label: "Cancel",
        enter_ok: true,
        multiline_focus: false,
        first: last.is_none_or(|f| f + 1 < frame),
        close: false,
        ok: false,
    };
    let resp = egui::Modal::new(id.with("modal")).show(ctx, |ui| {
        ui.set_width(width);
        ui.heading(title);
        ui.separator();
        let t = body(ui, &mut ctl);
        ui.add_space(6.0);
        ui.separator();
        let mut ok = false;
        let mut cancel = false;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(ctl.cancel_label).clicked() {
                cancel = true;
            }
            if ctl.show_ok {
                let text = RichText::new(&ctl.ok_label).strong();
                let b = if ctl.danger {
                    egui::Button::new(text.color(Color32::WHITE)).fill(DANGER)
                } else {
                    egui::Button::new(text)
                };
                if ui.add_enabled(ctl.ok_enabled, b).clicked() {
                    ok = true;
                }
            }
        });
        (t, ok, cancel)
    });
    let top = resp.is_top_modal && !resp.any_popup_open;
    let (t, ok, cancel) = resp.inner;
    let esc = top && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    let enter = top
        && ctl.show_ok
        && ctl.ok_enabled
        && ctl.enter_ok
        && ctx.input(|i| i.key_pressed(Key::Enter) && (i.modifiers.command || !ctl.multiline_focus));
    let out = if (ok || enter || ctl.ok) && ctl.ok_enabled {
        Outcome::Ok
    } else if cancel || esc || ctl.close {
        Outcome::Cancel
    } else {
        Outcome::None
    };
    (out, t)
}

/// Maps an outcome to `Dialog::show`'s "keep open" result, running `ok` on OK.
pub fn finish(out: Outcome, ok: impl FnOnce()) -> bool {
    match out {
        Outcome::None => true,
        Outcome::Cancel => false,
        Outcome::Ok => {
            ok();
            false
        }
    }
}

pub fn error(ui: &mut Ui, text: impl Into<String>) {
    ui.colored_label(DANGER, format!("✖ {}", text.into()));
}

pub fn warning(ui: &mut Ui, text: impl Into<String>) {
    ui.colored_label(WARN, format!("⚠ {}", text.into()));
}

pub fn hint(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).weak().small());
}

/// Combo box over strings. An empty value shows `none` (selectable when given).
pub fn combo(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut String, options: &[String], none: Option<&str>, width: f32) -> bool {
    let before = value.clone();
    let shown = if value.is_empty() { none.unwrap_or("").to_string() } else { value.clone() };
    egui::ComboBox::from_id_salt(id).width(width).selected_text(shown).height(320.0).show_ui(ui, |ui| {
        if let Some(n) = none {
            ui.selectable_value(value, String::new(), n);
        }
        for o in options {
            ui.selectable_value(value, o.clone(), o);
        }
    });
    *value != before
}

/// Revision picker: grouped local branches, remote branches and tags.
pub fn rev_combo(ui: &mut Ui, id: &str, value: &mut String, snap: &Snapshot, extra: Option<&str>, width: f32) -> bool {
    let before = value.clone();
    let refs = &snap.refs;
    let current = refs.head.branch().unwrap_or_default().to_string();
    egui::ComboBox::from_id_salt(id).width(width).selected_text(value.clone()).height(360.0).show_ui(ui, |ui| {
        if let Some(e) = extra {
            ui.selectable_value(value, e.to_string(), e);
        }
        ui.label(RichText::new("Branches").weak().small());
        for b in refs.branches.iter().filter(|b| b.name != current) {
            ui.selectable_value(value, b.name.clone(), &b.name);
        }
        if !refs.remote_branches.is_empty() {
            ui.label(RichText::new("Remote branches").weak().small());
            for b in &refs.remote_branches {
                let f = b.full();
                ui.selectable_value(value, f.clone(), &f);
            }
        }
        if !refs.tags.is_empty() {
            ui.label(RichText::new("Tags").weak().small());
            for t in &refs.tags {
                ui.selectable_value(value, t.name.clone(), &t.name);
            }
        }
    });
    *value != before
}

pub fn remote_names(snap: &Snapshot) -> Vec<String> {
    snap.refs.remotes.iter().map(|r| r.name.clone()).collect()
}

/// "origin" when present, else the first remote, else "".
pub fn default_remote(remotes: &[String]) -> String {
    if remotes.iter().any(|r| r == "origin") {
        return "origin".into();
    }
    remotes.first().cloned().unwrap_or_default()
}

/// Splits "origin/feature/x" into ("origin", "feature/x") using the known
/// remote names (longest match), falling back to the first slash.
pub fn split_remote_ref(remotes: &[String], full: &str) -> Option<(String, String)> {
    remotes
        .iter()
        .filter(|r| full.len() > r.len() + 1 && full.starts_with(r.as_str()) && full.as_bytes()[r.len()] == b'/')
        .max_by_key(|r| r.len())
        .map(|r| (r.clone(), full[r.len() + 1..].to_string()))
        .or_else(|| full.split_once('/').map(|(a, b)| (a.to_string(), b.to_string())))
}

/// True when tracked files have changes (untracked files do not count).
pub fn worktree_dirty(snap: &Snapshot) -> bool {
    !snap.status.staged.is_empty() || snap.status.unstaged.iter().any(|e| e.kind != gk_status::Kind::Untracked)
}

/// Commit id a rev names, from the snapshot (no git call).
pub fn resolve_rev(snap: &Snapshot, rev: &str) -> Option<String> {
    let refs = &snap.refs;
    if rev == "HEAD" {
        return refs.head_id.clone();
    }
    if let Some(b) = refs.branch(rev) {
        return Some(b.target.clone());
    }
    if let Some(b) = refs.remote_branches.iter().find(|b| b.full() == rev) {
        return Some(b.target.clone());
    }
    if let Some(t) = refs.tags.iter().find(|t| t.name == rev) {
        return Some(t.target.clone());
    }
    let hex = rev.len() >= 4 && rev.bytes().all(|b| b.is_ascii_hexdigit());
    if rev.len() == 40 && hex {
        return Some(rev.to_lowercase());
    }
    if hex {
        let lower = rev.to_lowercase();
        return snap.history.ids.iter().map(|id| id.to_string()).find(|id| id.starts_with(&lower));
    }
    None
}

/// "abcd1234  Summary" for a rev, or "" when unknown. Reads one commit.
pub fn describe_rev(tab: &RepoTab, snap: &Snapshot, rev: &str) -> String {
    let Some(id) = resolve_rev(snap, rev) else { return String::new() };
    let summary = gk_core::ObjectId::from_hex(id.as_bytes())
        .ok()
        .and_then(|oid| tab.session.meta().get_sync(oid))
        .map(|m| m.summary.clone())
        .unwrap_or_default();
    format!("{}  {summary}", crate::util::short_id(&id))
}

/// Cached [`describe_rev`]: recomputed only when the rev changes.
#[derive(Default)]
pub struct RevLabel {
    rev: Option<String>,
    text: String,
}

impl RevLabel {
    pub fn get(&mut self, tab: &RepoTab, snap: &Snapshot, rev: &str) -> &str {
        if self.rev.as_deref() != Some(rev) {
            self.text = describe_rev(tab, snap, rev);
            self.rev = Some(rev.to_string());
        }
        &self.text
    }
}

/// Settings askpass environment for git processes started outside a tab
/// (clone). Set once by the app.
static ASKPASS_ENV: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn set_askpass_env(env: Vec<(String, String)>) {
    *ASKPASS_ENV.lock() = env;
}

pub fn askpass_env() -> Vec<(String, String)> {
    ASKPASS_ENV.lock().clone()
}

// ---------------------------------------------------------------- tasks

/// A value computed on a background thread; repaints when ready.
pub struct Task<T> {
    slot: Arc<Mutex<Option<T>>>,
    done: Option<T>,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let v = f();
            *s2.lock() = Some(v);
            c.request_repaint();
        });
        Self { slot, done: None }
    }

    pub fn poll(&mut self) -> Option<&mut T> {
        if self.done.is_none() {
            self.done = self.slot.lock().take();
        }
        self.done.as_mut()
    }

    pub fn take(&mut self) -> Option<T> {
        self.poll();
        self.done.take()
    }
}

// ---------------------------------------------------------------- validation

/// git's refname rules (`git check-ref-format --allow-onelevel`).
pub fn check_ref_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name is empty".into());
    }
    if name == "@" {
        return Err("\"@\" is not a valid name".into());
    }
    if let Some(c) = name.chars().find(|c| c.is_ascii_control() || " ~^:?*[\\".contains(*c)) {
        let shown = if c == ' ' { "space".to_string() } else if c.is_ascii_control() { "control character".to_string() } else { format!("'{c}'") };
        return Err(format!("Name cannot contain {shown}"));
    }
    if name.contains("..") {
        return Err("Name cannot contain \"..\"".into());
    }
    if name.contains("@{") {
        return Err("Name cannot contain \"@{\"".into());
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err("Name cannot start or end with '/' or contain \"//\"".into());
    }
    if name.ends_with('.') {
        return Err("Name cannot end with '.'".into());
    }
    for comp in name.split('/') {
        if comp.starts_with('.') {
            return Err("A path component cannot start with '.'".into());
        }
        if comp.ends_with(".lock") {
            return Err("A path component cannot end with \".lock\"".into());
        }
    }
    Ok(())
}

/// `git check-ref-format --branch` rules.
pub fn check_branch_name(name: &str) -> Result<(), String> {
    if name.starts_with('-') {
        return Err("Name cannot start with '-'".into());
    }
    if name == "HEAD" {
        return Err("\"HEAD\" is not a valid branch name".into());
    }
    check_ref_name(name)
}

/// Branch name validation including a clash with existing local branches.
pub fn check_new_branch(snap: &Snapshot, name: &str) -> Result<(), String> {
    check_branch_name(name.trim())?;
    if snap.refs.branch(name.trim()).is_some() {
        return Err(format!("A branch named '{}' already exists", name.trim()));
    }
    Ok(())
}

/// Remote names: refname rules, single level, no whitespace.
pub fn check_remote_name(name: &str) -> Result<(), String> {
    if name.contains('/') {
        return Err("Remote name cannot contain '/'".into());
    }
    check_branch_name(name)
}

// ---------------------------------------------------------------- pure helpers

/// Splits a command line into arguments. Whitespace separates; '...' is
/// literal; "..." allows \" and \\ escapes. Backslashes elsewhere are kept
/// (Windows paths). A leading `git` word is dropped.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            '\'' => {
                has = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some('"') | Some('\\') => cur.push(chars.next().unwrap_or('\\')),
                            _ => cur.push('\\'),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("unterminated \" quote".into()),
                    }
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        out.push(cur);
    }
    if out.first().is_some_and(|a| a == "git") {
        out.remove(0);
    }
    Ok(out)
}

/// Folder name git would use for a clone URL.
pub fn folder_from_url(url: &str) -> String {
    let u = url.trim().trim_end_matches(['/', '\\']);
    let last = u.rsplit(['/', '\\', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    name.to_string()
}

/// Moves `v[from]` so it ends up at index `to`.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() || from == to {
        return;
    }
    let item = v.remove(from);
    let to = to.min(v.len());
    v.insert(to, item);
}

/// Parses `git log --format=%H%x00%B%x1e`: (id, full message).
pub fn parse_log_records(out: &str) -> Vec<(String, String)> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches(['\n', '\r']);
            let (id, msg) = rec.split_once('\0')?;
            let id = id.trim();
            (!id.is_empty()).then(|| (id.to_string(), msg.trim_end().to_string()))
        })
        .collect()
}

/// Order of a rebase todo after `--autosquash`: returns the new order as
/// indices into `summaries`, with the action for `fixup!`/`squash!` commits.
pub fn autosquash_plan(ids: &[String], summaries: &[String]) -> Vec<(usize, Option<TodoAction>)> {
    let n = summaries.len();
    let mut root: Vec<Option<usize>> = vec![None; n];
    let mut attached: Vec<Vec<(usize, TodoAction)>> = vec![Vec::new(); n];
    for i in 0..n {
        let s = summaries[i].as_str();
        let (action, rest) = if let Some(r) = s.strip_prefix("fixup! ").or_else(|| s.strip_prefix("amend! ")) {
            (TodoAction::Fixup, r)
        } else if let Some(r) = s.strip_prefix("squash! ") {
            (TodoAction::Squash, r)
        } else {
            continue;
        };
        let mut target = rest;
        while let Some(r) = ["fixup! ", "squash! ", "amend! "].iter().find_map(|p| target.strip_prefix(p)) {
            target = r;
        }
        let found = (0..i).find(|&j| summaries[j] == target || summaries[j].starts_with(target))
            .or_else(|| (target.len() >= 4).then(|| (0..i).find(|&j| ids[j].starts_with(target))).flatten());
        if let Some(j) = found {
            let r = root[j].unwrap_or(j);
            root[i] = Some(r);
            attached[r].push((i, action));
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if root[i].is_some() {
            continue;
        }
        out.push((i, None));
        out.extend(attached[i].iter().map(|&(k, a)| (k, Some(a))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names() {
        for ok in ["main", "feature/x", "fix-1.2", "a/b/c", "üñí", "x@y"] {
            assert!(check_branch_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", "HEAD", "@", "a b", "a..b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "/a", "a/", "a//b",
            "a.", ".a", "a/.b", "a.lock", "a/b.lock/c", "a@{1}", "tab\tx",
        ] {
            assert!(check_branch_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_remote_name("up/stream").is_err());
        assert!(check_remote_name("upstream").is_ok());
    }

    #[test]
    fn argument_splitting() {
        assert_eq!(split_args("git log --oneline -5").unwrap(), ["log", "--oneline", "-5"]);
        assert_eq!(split_args(r#"commit -m "a \"b\" c""#).unwrap(), ["commit", "-m", r#"a "b" c"#]);
        assert_eq!(split_args("commit -m 'it''s'").unwrap(), ["commit", "-m", "its"]);
        assert_eq!(split_args(r"add C:\dir\file.txt").unwrap(), ["add", r"C:\dir\file.txt"]);
        assert_eq!(split_args(r#"x "" y"#).unwrap(), ["x", "", "y"]);
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        assert!(split_args("log \"abc").is_err());
        assert!(split_args("log 'abc").is_err());
    }

    #[test]
    fn clone_folder_names() {
        assert_eq!(folder_from_url("https://github.com/user/repo.git"), "repo");
        assert_eq!(folder_from_url("https://github.com/user/repo/"), "repo");
        assert_eq!(folder_from_url("git@github.com:user/my-repo.git"), "my-repo");
        assert_eq!(folder_from_url("git@host:repo.git"), "repo");
        assert_eq!(folder_from_url(r"C:\src\proj"), "proj");
    }

    #[test]
    fn reorder() {
        let mut v = vec![0, 1, 2, 3];
        move_item(&mut v, 0, 2);
        assert_eq!(v, [1, 2, 0, 3]);
        move_item(&mut v, 3, 0);
        assert_eq!(v, [3, 1, 2, 0]);
        move_item(&mut v, 1, 99);
        assert_eq!(v, [3, 2, 0, 1]);
        move_item(&mut v, 9, 0);
        assert_eq!(v, [3, 2, 0, 1]);
    }

    #[test]
    fn autosquash_ordering() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let ids = s(&["aaaa1111", "bbbb2222", "cccc3333", "dddd4444", "eeee5555"]);
        let sums = s(&["Add A", "Add B", "fixup! Add A", "squash! fixup! Add A", "fixup! bbbb"]);
        let plan = autosquash_plan(&ids, &sums);
        assert_eq!(
            plan,
            [
                (0, None),
                (2, Some(TodoAction::Fixup)),
                (3, Some(TodoAction::Squash)),
                (1, None),
                (4, Some(TodoAction::Fixup)),
            ]
        );
        // Unmatched fixups stay in place.
        let plan = autosquash_plan(&ids[..2], &s(&["fixup! nothing", "Add B"]));
        assert_eq!(plan, [(0, None), (1, None)]);
    }

    #[test]
    fn log_records() {
        let out = "abc\0Subject\n\nBody\n\x1e\ndef\0Other\n\x1e\n";
        assert_eq!(
            parse_log_records(out),
            [("abc".to_string(), "Subject\n\nBody".to_string()), ("def".to_string(), "Other".to_string())]
        );
    }

    #[test]
    fn remote_refs() {
        let r = vec!["origin".to_string(), "origin/mirror".to_string()];
        assert_eq!(split_remote_ref(&r, "origin/feature/x"), Some(("origin".into(), "feature/x".into())));
        assert_eq!(split_remote_ref(&r, "origin/mirror/main"), Some(("origin/mirror".into(), "main".into())));
        assert_eq!(split_remote_ref(&r, "up/main"), Some(("up".into(), "main".into())));
        assert_eq!(split_remote_ref(&r, "plain"), None);
    }
}
