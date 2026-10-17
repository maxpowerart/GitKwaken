//! Keyboard access to lists (history, search, reflog, sidebar): Shift+F10
//! opens the context menu of the selected row, Enter checks it out, Ctrl+B
//! creates a branch and Ctrl+T a tag there, Delete deletes (sidebar).
//!
//! The Menu (Apps) key is not delivered by egui-winit (no `egui::Key` for
//! it), so Shift+F10 is the keyboard binding that works everywhere.

use egui::{Id, Key, Modifiers, Pos2, Response, Ui};
use egui::{Popup, PopupAnchor, SetOpenCommand};

/// Shortcut texts shown in menus and palette labels.
pub const CHECKOUT_KEY: &str = "Enter";
pub const BRANCH_KEY: &str = "Ctrl+B";
pub const TAG_KEY: &str = "Ctrl+T";
pub const DELETE_KEY: &str = "Delete";

/// List shortcuts pressed this frame (consumed only when `focused`).
#[derive(Debug, Default, Clone, Copy)]
pub struct ListKeys {
    pub menu: bool,
    pub enter: bool,
    pub branch: bool,
    pub tag: bool,
    pub delete: bool,
}

impl ListKeys {
    pub fn read(ui: &Ui, focused: bool) -> Self {
        if !focused {
            return Self::default();
        }
        ui.input_mut(|i| Self {
            menu: take(i, Modifiers::SHIFT, Key::F10),
            enter: take(i, Modifiers::NONE, Key::Enter),
            branch: take(i, Modifiers::COMMAND, Key::B),
            tag: take(i, Modifiers::COMMAND, Key::T),
            delete: take(i, Modifiers::NONE, Key::Delete),
        })
    }
}

/// Consumes a press of `key` with exactly the modifiers `m` (so Ctrl+B does
/// not also fire on Ctrl+Shift+B).
fn take(i: &mut egui::InputState, m: Modifiers, key: Key) -> bool {
    let mut hit = false;
    i.events.retain(|e| match e {
        egui::Event::Key {
            key: k,
            pressed: true,
            modifiers,
            ..
        } if *k == key && modifiers.matches_exact(m) => {
            hit = true;
            false
        }
        _ => true,
    });
    hit
}

/// A context menu button showing its shortcut on the right; closes the menu
/// when clicked.
pub fn menu_item(ui: &mut Ui, enabled: bool, text: &str, shortcut: &str) -> bool {
    let mut b = egui::Button::new(text);
    if !shortcut.is_empty() {
        b = b.shortcut_text(egui::RichText::new(shortcut).weak());
    }
    let clicked = ui.add_enabled(enabled, b).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// A context menu opened from the keyboard: anchored at `pos` (the selected
/// row) instead of the pointer. `open` opens it this frame; afterwards it
/// stays open until an item calls `ui.close()` or the user clicks elsewhere.
pub fn keyboard_menu(
    resp: &Response,
    id: Id,
    open: bool,
    pos: Pos2,
    add: impl FnOnce(&mut Ui),
) -> bool {
    Popup::menu(resp)
        .id(id)
        .anchor(PopupAnchor::Position(pos))
        .open_memory(open.then_some(SetOpenCommand::Bool(true)))
        .show(add)
        .is_some()
}
