//! SourceTree-style toolbar: large icons over labels, in SourceTree's groups
//! (Commit Pull Push Fetch | Branch Merge | Stash Discard Tag, and Git Flow,
//! Remote, Terminal, Explorer, Settings on the right). Views switch in the
//! sidebar's WORKSPACE section (and by keyboard), as in SourceTree.

use crate::app::Shared;
use crate::dialogs::{Dialog, GlobalDialog};
use crate::tab::{RepoTab, View};
use egui::{Color32, Rect, Sense, Ui, pos2, vec2};
use gk_core::{RepoState, Snapshot};

/// Below this toolbar width the buttons drop their labels (icon + tooltip).
const COMPACT_BELOW: f32 = 900.0;
/// Size of a labelled toolbar button.
const BUTTON: egui::Vec2 = vec2(56.0, 48.0);
/// Size of an icon-only (compact) toolbar button.
const BUTTON_COMPACT: egui::Vec2 = vec2(34.0, 32.0);
const ICON_SIZE: f32 = 22.0;
const ICON_SIZE_COMPACT: f32 = 18.0;

const BISECT_FILL: Color32 = Color32::from_rgb(0xE0, 0x9A, 0x2A);

struct Tool<'a> {
    icon: &'a str,
    label: &'a str,
    hint: &'a str,
    /// Small count badge on the icon (Commit: pending changes).
    badge: usize,
    fill: Option<Color32>,
}

impl<'a> Tool<'a> {
    fn new(icon: &'a str, label: &'a str, hint: &'a str) -> Self {
        Self {
            icon,
            label,
            hint,
            badge: 0,
            fill: None,
        }
    }
}

/// Paints a toolbar button (icon over label, or icon only when `compact`).
fn paint_tool(ui: &mut Ui, compact: bool, t: &Tool, sense: Sense) -> egui::Response {
    // Everything follows the font size (1.0 at the default 13 pt).
    let k = crate::theme::text_scale(ui.style());
    let size = if compact { BUTTON_COMPACT } else { BUTTON } * k;
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let v = ui.visuals();
    let icon_color = if t.fill.is_some() {
        Color32::BLACK
    } else {
        crate::theme::lane_color(0)
    };
    let text = v.text_color();
    let p = ui.painter();
    if let Some(fill) = t.fill {
        p.rect_filled(rect, 4.0, fill);
    } else if resp.is_pointer_button_down_on() {
        p.rect_filled(rect, 4.0, v.widgets.active.weak_bg_fill);
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
    }
    // Some glyphs come from fallback fonts with smaller or larger designs.
    let scale = match t.icon {
        "⑂" => 1.5,
        "▣" => 0.85,
        _ => 1.0,
    };
    let (icon_size, icon_center) = if compact {
        (ICON_SIZE_COMPACT * k, rect.center())
    } else {
        (ICON_SIZE * k, pos2(rect.center().x, rect.top() + 17.0 * k))
    };
    p.text(
        icon_center,
        egui::Align2::CENTER_CENTER,
        t.icon,
        egui::FontId::proportional(icon_size * scale),
        icon_color,
    );
    if !compact {
        p.text(
            pos2(rect.center().x, rect.bottom() - 9.0 * k),
            egui::Align2::CENTER_CENTER,
            t.label,
            egui::FontId::proportional(12.0 * k),
            if t.fill.is_some() {
                Color32::BLACK
            } else {
                text
            },
        );
    }
    if t.badge > 0 {
        let s = if t.badge > 99 {
            "99+".to_string()
        } else {
            t.badge.to_string()
        };
        let font = egui::FontId::proportional(10.0 * k);
        let g = p.layout_no_wrap(s, font, Color32::WHITE);
        let w = (g.size().x + 6.0 * k).max(14.0 * k);
        let c = pos2(
            icon_center.x + icon_size * 0.5,
            icon_center.y - icon_size * 0.4,
        );
        let r = Rect::from_center_size(c, vec2(w, 14.0 * k));
        p.rect_filled(r, 7.0 * k, crate::theme::lane_color(0));
        p.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
    }
    let hint = if compact {
        format!("{}: {}", t.label, t.hint)
    } else {
        t.hint.to_string()
    };
    resp.on_hover_text(hint)
}

fn tool(ui: &mut Ui, compact: bool, t: Tool) -> bool {
    paint_tool(ui, compact, &t, Sense::click()).clicked()
}

/// A toolbar button that opens a menu; returns the menu's result.
fn tool_menu<R>(
    ui: &mut Ui,
    compact: bool,
    t: Tool,
    content: impl FnOnce(&mut Ui) -> Option<R>,
) -> Option<R> {
    let resp = paint_tool(ui, compact, &t, Sense::click());
    egui::Popup::menu(&resp).show(content).and_then(|r| r.inner)
}

fn group_gap(ui: &mut Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

pub fn show(ui: &mut Ui, tab: &mut RepoTab, snap: &Snapshot, shared: &mut Shared) {
    egui::Panel::top(egui::Id::new(("toolbar", &tab.path)))
        .frame(
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 1,
                bottom: 1,
            }),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let compact =
                    ui.available_width() < COMPACT_BELOW * crate::theme::text_scale(ui.style());
                let changes = snap.status.staged.len() + snap.status.unstaged.len();
                left_groups(ui, compact, changes, tab, snap);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    right_group(ui, compact, tab, snap, shared);
                });
            });
        });
}

fn left_groups(ui: &mut Ui, compact: bool, changes: usize, tab: &mut RepoTab, snap: &Snapshot) {
    let commit = Tool {
        badge: changes,
        ..Tool::new("✔", "Commit", "Commit staged changes")
    };
    if tool(ui, compact, commit) {
        tab.view = View::FileStatus;
        tab.status.commit.request_focus();
    }
    if tool(ui, compact, Tool::new("⬇", "Pull", "Pull from remote")) {
        tab.dialog = Some(Dialog::pull(snap));
    }
    if tool(ui, compact, Tool::new("⬆", "Push", "Push to remote")) {
        tab.dialog = Some(Dialog::push(snap));
    }
    if tool(
        ui,
        compact,
        Tool::new("⟳", "Fetch", "Fetch from all remotes"),
    ) {
        tab.dialog = Some(Dialog::fetch(snap));
    }
    group_gap(ui);
    if tool(
        ui,
        compact,
        Tool::new("⑂", "Branch", "Create or delete branches"),
    ) {
        tab.dialog = Some(Dialog::branch(snap, None));
    }
    if tool(
        ui,
        compact,
        Tool::new("⇄", "Merge", "Merge into the current branch"),
    ) {
        tab.dialog = Some(Dialog::merge(snap, None));
    }
    group_gap(ui);
    if tool(
        ui,
        compact,
        Tool::new("▣", "Stash", "Stash working copy changes"),
    ) {
        tab.dialog = Some(Dialog::stash());
    }
    if tool(
        ui,
        compact,
        Tool::new("✖", "Discard", "Discard working copy changes"),
    ) && changes > 0
    {
        tab.dialog = Some(Dialog::discard_all());
    }
    if tool(ui, compact, Tool::new("🏷", "Tag", "Create a tag")) {
        tab.dialog = Some(Dialog::tag(None));
    }
    if snap.refs.state == RepoState::Bisecting {
        group_gap(ui);
        let t = Tool {
            fill: Some(BISECT_FILL),
            ..Tool::new(
                "🔍",
                "Bisect",
                "A bisect is in progress: mark commits good or bad",
            )
        };
        if tool(ui, compact, t) {
            tab.dialog = Some(Dialog::bisect(snap));
        }
    }
}

/// Right-aligned group (laid out right to left): Settings, Explorer,
/// Terminal, Remote, Git Flow.
fn right_group(
    ui: &mut Ui,
    compact: bool,
    tab: &mut RepoTab,
    snap: &Snapshot,
    shared: &mut Shared,
) {
    let settings = tool_menu(
        ui,
        compact,
        Tool::new("⚙", "Settings", "Repository settings"),
        |ui| {
            let mut d = None;
            if ui.button("Edit repository config…").clicked() {
                d = Some(Ok(Dialog::edit_repo_config()));
            }
            if ui.button("Edit .gitignore…").clicked() {
                d = Some(Ok(Dialog::edit_gitignore()));
            }
            if ui.button("Authentication…").clicked() {
                d = Some(Ok(Dialog::authentication(snap)));
            }
            ui.separator();
            if ui.button("Options…").clicked() {
                d = Some(Err(GlobalDialog::settings()));
            }
            d
        },
    );
    match settings {
        Some(Ok(d)) => tab.dialog = Some(d),
        Some(Err(g)) => shared.actions.push(crate::app::Action::Global(g)),
        None => {}
    }
    if tool(ui, compact, Tool::new("📂", "Explorer", "Show in Explorer")) {
        let _ = open::that(&tab.path);
    }
    if tool(
        ui,
        compact,
        Tool::new(">_", "Terminal", "Open a terminal in the repository"),
    ) {
        open_terminal(&tab.path, shared);
    }
    let remote = tool_menu(
        ui,
        compact,
        Tool::new("☁", "Remote", "Add or edit remotes"),
        |ui| {
            let mut d = None;
            for r in &snap.refs.remotes {
                if ui.button(format!("Edit {}…", r.name)).clicked() {
                    d = Some(Dialog::edit_remote(r.name.clone()));
                }
            }
            if !snap.refs.remotes.is_empty() {
                ui.separator();
            }
            if ui.button("Add remote…").clicked() {
                d = Some(Dialog::add_remote());
            }
            d
        },
    );
    if let Some(d) = remote {
        tab.dialog = Some(d);
    }
    if tool(
        ui,
        compact,
        Tool::new(
            "⚑",
            "Git Flow",
            "Git Flow: start or finish features, releases and hotfixes",
        ),
    ) {
        tab.dialog = Some(Dialog::git_flow(snap));
    }
}

pub fn open_terminal(path: &std::path::Path, shared: &Shared) {
    if let Some(t) = &shared.settings.terminal {
        let args = t.args.replace("$PWD", &path.display().to_string());
        let _ = std::process::Command::new(&t.command)
            .args(args.split_whitespace())
            .current_dir(path)
            .spawn();
        return;
    }
    #[cfg(windows)]
    {
        // Prefer Git Bash when available, else Windows Terminal / cmd.
        let git = gk_cli::find_git();
        let bash = git
            .parent()
            .and_then(|d| d.parent())
            .map(|d| d.join("git-bash.exe"));
        if let Some(b) = bash.filter(|b| b.is_file()) {
            let _ = std::process::Command::new(b)
                .arg(format!("--cd={}", path.display()))
                .spawn();
        } else {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "cmd"])
                .current_dir(path)
                .spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-a", "Terminal"])
            .arg(path)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // `x-terminal-emulator` exists only on Debian-likes; elsewhere (Arch/Manjaro, Fedora)
        // take $TERMINAL or the first common terminal that starts. All open in the cwd.
        let candidates = std::env::var("TERMINAL").ok().into_iter().chain(
            [
                "x-terminal-emulator",
                "kgx",
                "gnome-terminal",
                "konsole",
                "xfce4-terminal",
                "mate-terminal",
                "lxterminal",
                "alacritty",
                "kitty",
                "foot",
                "xterm",
            ]
            .map(String::from),
        );
        for c in candidates {
            if std::process::Command::new(&c)
                .current_dir(path)
                .spawn()
                .is_ok()
            {
                break;
            }
        }
    }
}
