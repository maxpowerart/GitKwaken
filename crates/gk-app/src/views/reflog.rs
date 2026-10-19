//! Reflog view: where HEAD has been (`git reflog`), with checkout, branch and
//! reset from any entry. The details of the selected entry's commit show
//! below the list.

use crate::app::Shared;
use crate::dialogs::Dialog;
use crate::tab::{RepoTab, View};
use crate::views::history::keys::{self, ListKeys};
use crate::views::history::task::{Task, TaskState};
use crate::views::history::{in_rect, selection, split, table};
use egui::{Id, Rect, RichText, Ui};
use gk_core::{ObjectId, Snapshot};
use std::sync::Arc;

/// Entries loaded at most (`git reflog -n`).
const MAX_ENTRIES: usize = 5000;

/// One `git reflog` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: ObjectId,
    /// `HEAD@{n}`.
    pub selector: String,
    /// What happened, e.g. `commit: Fix x` or `checkout: moving from a to b`.
    pub subject: String,
    /// When HEAD moved (seconds since the epoch).
    pub time: i64,
    /// Local time zone offset at that time (seconds).
    pub offset: i32,
}

#[derive(Default)]
pub struct ReflogState {
    task: Task<(usize, u64), Vec<Entry>>,
    /// Bumped by "Refresh".
    run: u64,
    /// Last loaded list, shown while a reload runs.
    last: Option<Arc<Vec<Entry>>>,
    /// Selected row.
    pub sel: Option<usize>,
    scroll_to: Option<usize>,
    menu_row: Option<usize>,
    page_rows: usize,
}

impl ReflogState {
    /// Commit of the selected entry.
    pub fn selected_id(&self) -> Option<ObjectId> {
        let v = self.last.as_ref()?;
        v.get(self.sel?).map(|e| e.id)
    }
}

/// Parses `git reflog --date=iso-local --format=%H%x00%gd%x00%gs`: with a
/// date mode, `%gd` reads `HEAD@{2026-10-17 10:19:55 +0200}`; the entry
/// index is the line number.
pub fn parse(out: &str) -> Vec<Entry> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\0');
            let id = ObjectId::from_hex(it.next()?.trim().as_bytes()).ok()?;
            let gd = it.next()?;
            let (time, offset) = gd
                .split_once('{')
                .and_then(|(_, t)| parse_iso(t.trim_end_matches('}')))
                .unwrap_or((0, 0));
            let subject = it.next().unwrap_or("").to_string();
            Some((id, subject, time, offset))
        })
        .enumerate()
        .map(|(i, (id, subject, time, offset))| Entry {
            id,
            selector: format!("HEAD@{{{i}}}"),
            subject,
            time,
            offset,
        })
        .collect()
}

/// `2026-10-17 10:19:55 +0200` -> (unix seconds, offset seconds).
pub fn parse_iso(s: &str) -> Option<(i64, i32)> {
    let mut it = s.split_whitespace();
    let (date, time, tz) = (it.next()?, it.next()?, it.next()?);
    let n = |s: &str| s.parse::<i64>().ok();
    let mut d = date.split('-');
    let (y, m, day) = (n(d.next()?)?, n(d.next()?)?, n(d.next()?)?);
    let mut t = time.split(':');
    let (h, mi, se) = (n(t.next()?)?, n(t.next()?)?, n(t.next()?)?);
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let tz = tz.trim_start_matches(['+', '-']);
    let off = sign * (n(tz.get(..2)?)? * 3600 + n(tz.get(2..4)?)? * 60);
    // Howard Hinnant's days-from-civil.
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + se - off, off as i32))
}

/// Splits a reflog subject into its action (`commit (amend)`) and message.
pub fn split_subject(s: &str) -> (&str, &str) {
    match s.split_once(": ") {
        Some((a, m)) => (a, m),
        None => (s, ""),
    }
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    // Reload whenever the refs change (every HEAD move refreshes them).
    let key = (Arc::as_ptr(&snap.refs) as usize, tab.reflog.run);
    let git = tab.session.repo().git().clone();
    let state = tab.reflog.task.get(&key, ui.ctx(), move || {
        let n = format!("-n{MAX_ENTRIES}");
        let o = git.run_raw(
            &[
                "reflog",
                "show",
                n.as_str(),
                "--date=iso-local",
                "--format=%H%x00%gd%x00%gs",
                "HEAD",
                "--",
            ],
            None,
        )?;
        // No reflog yet (fresh clone, unborn branch): an empty list.
        if o.code != Some(0) {
            return Ok(Vec::new());
        }
        Ok(parse(&o.stdout_str()))
    });
    let mut error = None;
    match state {
        TaskState::Done(v) => tab.reflog.last = Some(v),
        TaskState::Failed(e) => error = Some(e),
        _ => {}
    }
    let loading = matches!(tab.reflog.task.state(), TaskState::Running);
    let entries = tab.reflog.last.clone().unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Reflog").strong());
        ui.label(RichText::new("where HEAD has been").weak());
        if loading {
            ui.spinner();
        } else {
            ui.label(RichText::new(format!("{} entries", entries.len())).weak());
        }
        if ui
            .add(crate::icons::IconButton::new(crate::icons::Icon::Refresh).small())
            .on_hover_text("Refresh")
            .clicked()
        {
            tab.reflog.run += 1;
        }
        ui.separator();
        let sel = tab.reflog.sel.and_then(|i| entries.get(i)).cloned();
        let has = sel.is_some();
        if ui
            .add_enabled(has, egui::Button::new("Checkout…"))
            .on_hover_text(format!(
                "Checkout this commit (detached) ({})",
                keys::CHECKOUT_KEY
            ))
            .clicked()
            && let Some(e) = &sel
        {
            tab.dialog = Some(Dialog::checkout(e.id.to_string()));
        }
        if ui
            .add_enabled(has, egui::Button::new("Create branch…"))
            .on_hover_text(format!("New branch at this commit ({})", keys::BRANCH_KEY))
            .clicked()
            && let Some(e) = &sel
        {
            tab.dialog = Some(Dialog::branch(snap, Some(e.id.to_string())));
        }
        if ui
            .add_enabled(has, egui::Button::new("Reset to here…"))
            .on_hover_text("Reset the current branch to this commit")
            .clicked()
            && let Some(e) = &sel
        {
            tab.dialog = Some(Dialog::reset(e.id.to_string()));
        }
        ui.separator();
        if ui
            .button("Discarded changes…")
            .on_hover_text("Restore changes saved before a discard")
            .clicked()
        {
            tab.dialog = Some(Dialog::discarded());
        }
    });
    if let Some(e) = error {
        ui.colored_label(shared.palette.removed_fg, e);
    }
    ui.add_space(2.0);
    if entries.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            if loading {
                ui.spinner();
            } else {
                ui.label(RichText::new("The reflog is empty.").weak());
            }
        });
        return;
    }
    if tab.reflog.sel.is_none_or(|i| i >= entries.len()) {
        tab.reflog.sel = Some(0);
    }

    let mut frac = tab.state.layout.list_split;
    let (top, bottom) = split(
        ui,
        Id::new(("reflog-split", &tab.path)),
        &mut frac,
        true,
        80.0,
    );
    tab.state.layout.list_split = frac;
    in_rect(ui, top, "reflog-table", |ui| {
        list(ui, tab, snap, shared, &entries)
    });
    in_rect(ui, bottom, "reflog-details", |ui| {
        crate::views::details::show(ui, tab, snap, shared)
    });
}

fn list(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared, entries: &[Entry]) {
    let id = Id::new(("reflog-list", &tab.path));
    let n = entries.len();
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        let st = &mut tab.reflog;
        for (key, _) in table::nav_keys(ui) {
            if let Some(t) = selection::nav_target(key, st.sel, n, st.page_rows.saturating_sub(1)) {
                st.sel = Some(t);
                st.scroll_to = Some(t);
            }
        }
    }
    let k = ListKeys::read(ui, focused);
    let row_h = crate::theme::Density::of(shared.settings.compact).history_row(
        shared.settings.history.row_height,
        crate::theme::text_scale(ui.style()),
    );
    let scroll = tab.reflog.scroll_to.take().map(table::ScrollTo::Reveal);
    let st = crate::views::history::cell_style(ui, shared);
    let visuals = ui.visuals().clone();
    let sel = tab.reflog.sel;
    let relative = shared.settings.history.relative_dates;
    let head = crate::views::history::head_oid(snap);
    let out = table::virtual_list(ui, id, n, row_h, scroll, |f, range| {
        let w = f.x.max - f.x.min;
        let x0 = f.x.min;
        let sel_w = 90.0;
        let act_w = (w * 0.18).clamp(90.0, 180.0);
        let id_w = 80.0;
        let date_w = 130.0;
        for r in range {
            let e = &entries[r];
            let rect = f.row_rect(r);
            table::paint_row_bg(f, r, rect, sel == Some(r), focused, st.palette, &visuals);
            let cell = |a: f32, b: f32| Rect::from_x_y_ranges(a..=b, rect.y_range());
            let (action, msg) = split_subject(&e.subject);
            let mut x = x0;
            table::cell_text(
                f.painter,
                cell(x, x + sel_w),
                e.selector.clone(),
                &st.mono,
                st.muted,
            );
            x += sel_w;
            let color = action_color(action, st.palette);
            table::cell_text(
                f.painter,
                cell(x, x + act_w),
                action.to_string(),
                &st.font,
                color,
            );
            x += act_w;
            let right = f.x.max - id_w - date_w;
            let text_color = if head == Some(e.id) {
                st.palette.head
            } else {
                st.text
            };
            table::cell_text(
                f.painter,
                cell(x, right),
                msg.to_string(),
                &st.font,
                text_color,
            );
            let short = crate::util::short_id(&e.id.to_string()).to_string();
            table::cell_text(
                f.painter,
                cell(right, right + id_w),
                short,
                &st.mono,
                st.muted,
            );
            let date = crate::util::format_time(e.time, e.offset, relative);
            table::cell_text(
                f.painter,
                cell(right + id_w, f.x.max),
                date,
                &st.font,
                st.muted,
            );
        }
    });
    tab.reflog.page_rows = out.visible.len().max(1);
    if let Some((r, _)) = out.clicked {
        tab.reflog.sel = Some(r);
    }
    if let Some(r) = out.double_clicked {
        tab.dialog = Some(Dialog::checkout(entries[r].id.to_string()));
    }
    if let Some(r) = out.secondary {
        tab.reflog.sel = Some(r);
        tab.reflog.menu_row = Some(r);
    }
    let cur = tab.reflog.sel.filter(|&r| r < n);
    if let Some(r) = cur {
        let hex = entries[r].id.to_string();
        if k.enter {
            tab.dialog = Some(Dialog::checkout(hex.clone()));
        }
        if k.branch {
            tab.dialog = Some(Dialog::branch(snap, Some(hex.clone())));
        }
        if k.tag {
            tab.dialog = Some(Dialog::tag(Some(hex.clone())));
        }
        if k.menu {
            tab.reflog.menu_row = Some(r);
        }
    }
    if let Some(r) = tab.reflog.menu_row.filter(|&r| r < n) {
        let e = entries[r].clone();
        let mut pick = None;
        out.response
            .context_menu(|ui| pick = entry_menu(ui, snap, &e));
        keys::keyboard_menu(
            &out.response,
            id.with("kb-menu"),
            k.menu,
            out.row_pos(r),
            |ui| pick = entry_menu(ui, snap, &e),
        );
        match pick {
            Some(Pick::Dialog(d)) => tab.dialog = Some(*d),
            Some(Pick::Copy(s)) => ui.ctx().copy_text(s),
            Some(Pick::Show(id)) => {
                tab.view = View::History;
                tab.history.select_commit(id);
            }
            None => {}
        }
    }
}

enum Pick {
    Dialog(Box<Dialog>),
    Copy(String),
    Show(ObjectId),
}

fn entry_menu(ui: &mut Ui, snap: &Snapshot, e: &Entry) -> Option<Pick> {
    let hex = e.id.to_string();
    let mut pick = None;
    if keys::menu_item(ui, true, "Checkout (detached)…", keys::CHECKOUT_KEY) {
        pick = Some(Pick::Dialog(Box::new(Dialog::checkout(hex.clone()))));
    }
    if keys::menu_item(ui, true, "Create branch here…", keys::BRANCH_KEY) {
        pick = Some(Pick::Dialog(Box::new(Dialog::branch(
            snap,
            Some(hex.clone()),
        ))));
    }
    if keys::menu_item(ui, true, "Create tag here…", keys::TAG_KEY) {
        pick = Some(Pick::Dialog(Box::new(Dialog::tag(Some(hex.clone())))));
    }
    if keys::menu_item(ui, true, "Reset current branch to here…", "") {
        pick = Some(Pick::Dialog(Box::new(Dialog::reset(hex.clone()))));
    }
    ui.separator();
    if keys::menu_item(ui, true, "Copy SHA", "") {
        pick = Some(Pick::Copy(hex.clone()));
    }
    if keys::menu_item(ui, snap.history.row(&e.id).is_some(), "Show in history", "") {
        pick = Some(Pick::Show(e.id));
    }
    pick
}

fn action_color(action: &str, p: &crate::theme::Palette) -> egui::Color32 {
    let a = action.split(' ').next().unwrap_or(action);
    match a {
        "commit" | "cherry-pick" | "revert" => p.added_fg,
        "reset" | "rebase" => p.removed_fg,
        "merge" | "pull" => p.branch,
        "checkout" | "switch" => p.remote,
        _ => p.muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "4ca56ea8275359cbf761c0ca70e915ac2d28f067";

    #[test]
    fn parses_reflog_lines() {
        let out = format!(
            "{A}\0HEAD@{{2023-11-14 23:13:20 +0100}}\0commit (amend): Fix: x\n{A}\0HEAD@{{2020-09-13 12:26:40 +0000}}\0checkout: moving from a to b\nbad\n"
        );
        let v = parse(&out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].selector, "HEAD@{0}");
        assert_eq!(v[1].selector, "HEAD@{1}");
        assert_eq!((v[0].time, v[0].offset), (1_700_000_000, 3600));
        assert_eq!(v[1].time, 1_600_000_000);
        assert_eq!(
            crate::util::format_time(v[0].time, v[0].offset, false),
            "2023-11-14 23:13"
        );
        assert_eq!(
            parse_iso("2026-01-02 03:04:05 -0130").map(|x| x.1),
            Some(-5400)
        );
        assert_eq!(split_subject(&v[0].subject), ("commit (amend)", "Fix: x"));
        assert_eq!(split_subject("initial"), ("initial", ""));
    }
}
