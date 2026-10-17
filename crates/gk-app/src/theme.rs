//! Colors and visuals. Dark is the default, tuned to resemble SourceTree.

use egui::{Color32, Visuals};

/// Lane colors of the history graph.
pub const LANES: [Color32; 8] = [
    Color32::from_rgb(0x4F, 0x9D, 0xF7),
    Color32::from_rgb(0xF0, 0x7A, 0x4A),
    Color32::from_rgb(0x3D, 0xC2, 0x8B),
    Color32::from_rgb(0xC3, 0x7B, 0xE8),
    Color32::from_rgb(0xE8, 0xC1, 0x3D),
    Color32::from_rgb(0x2E, 0xC4, 0xD6),
    Color32::from_rgb(0xE8, 0x5D, 0x8A),
    Color32::from_rgb(0x9A, 0xB8, 0x4E),
];

pub fn lane_color(i: u16) -> Color32 {
    LANES[i as usize % LANES.len()]
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub added_bg: Color32,
    pub removed_bg: Color32,
    pub added_word: Color32,
    pub removed_word: Color32,
    pub added_fg: Color32,
    pub removed_fg: Color32,
    pub hunk_bg: Color32,
    pub muted: Color32,
    pub selection: Color32,
    pub branch: Color32,
    pub remote: Color32,
    pub tag: Color32,
    pub head: Color32,
    pub stash: Color32,
    pub modified: Color32,
    pub untracked: Color32,
    pub conflict: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            added_bg: Color32::from_rgba_unmultiplied(0x2E, 0xA0, 0x43, 40),
            removed_bg: Color32::from_rgba_unmultiplied(0xF8, 0x51, 0x49, 40),
            added_word: Color32::from_rgba_unmultiplied(0x2E, 0xA0, 0x43, 110),
            removed_word: Color32::from_rgba_unmultiplied(0xF8, 0x51, 0x49, 110),
            added_fg: Color32::from_rgb(0x56, 0xD3, 0x64),
            removed_fg: Color32::from_rgb(0xF8, 0x51, 0x49),
            hunk_bg: Color32::from_rgba_unmultiplied(0x58, 0x8B, 0xE8, 30),
            muted: Color32::from_gray(140),
            selection: Color32::from_rgba_unmultiplied(0x4F, 0x9D, 0xF7, 60),
            branch: Color32::from_rgb(0x3D, 0x8B, 0xD8),
            remote: Color32::from_rgb(0x8A, 0x63, 0xD2),
            tag: Color32::from_rgb(0xC9, 0x9A, 0x2E),
            head: Color32::from_rgb(0x3D, 0xA8, 0x5A),
            stash: Color32::from_rgb(0x80, 0x80, 0x80),
            modified: Color32::from_rgb(0xE8, 0xA7, 0x3D),
            untracked: Color32::from_rgb(0x8A, 0x8A, 0x8A),
            conflict: Color32::from_rgb(0xF8, 0x51, 0x49),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            added_bg: Color32::from_rgb(0xE6, 0xFF, 0xEC),
            removed_bg: Color32::from_rgb(0xFF, 0xEB, 0xE9),
            added_word: Color32::from_rgb(0xAB, 0xF2, 0xBC),
            removed_word: Color32::from_rgb(0xFF, 0xC0, 0xBA),
            added_fg: Color32::from_rgb(0x1A, 0x7F, 0x37),
            removed_fg: Color32::from_rgb(0xCF, 0x22, 0x2E),
            hunk_bg: Color32::from_rgb(0xDD, 0xF4, 0xFF),
            muted: Color32::from_gray(110),
            selection: Color32::from_rgba_unmultiplied(0x2F, 0x6F, 0xD8, 50),
            branch: Color32::from_rgb(0x1F, 0x6F, 0xC5),
            remote: Color32::from_rgb(0x6A, 0x43, 0xB2),
            tag: Color32::from_rgb(0x9A, 0x6A, 0x00),
            head: Color32::from_rgb(0x1A, 0x7F, 0x37),
            stash: Color32::from_rgb(0x60, 0x60, 0x60),
            modified: Color32::from_rgb(0xB0, 0x6F, 0x00),
            untracked: Color32::from_rgb(0x70, 0x70, 0x70),
            conflict: Color32::from_rgb(0xCF, 0x22, 0x2E),
        }
    }
}

/// Uses the platform UI fonts when present (Segoe UI / Consolas on Windows)
/// and adds symbol and emoji fonts as fallbacks so icons like ⑂ ⇄ 🏷 render.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"));
    let load = |name: &str| -> Option<FontData> {
        let p = dir.as_ref()?.join(name);
        std::fs::read(p).ok().map(FontData::from_owned)
    };
    let mut add = |key: &str, file: &str, family: FontFamily, primary: bool| {
        if let Some(data) = load(file) {
            fonts.font_data.insert(key.to_string(), std::sync::Arc::new(data));
            let list = fonts.families.entry(family).or_default();
            if primary {
                list.insert(0, key.to_string());
            } else {
                list.push(key.to_string());
            }
        }
    };
    add("segoe-ui", "segoeui.ttf", FontFamily::Proportional, true);
    add("consolas", "consola.ttf", FontFamily::Monospace, true);
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        add("segoe-symbol", "seguisym.ttf", family.clone(), false);
        add("segoe-emoji", "seguiemj.ttf", family, false);
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, theme: gk_settings::Theme, ui_scale: f32, font_size: f32) -> Palette {
    let dark = match theme {
        gk_settings::Theme::Dark => true,
        gk_settings::Theme::Light => false,
        gk_settings::Theme::System => ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true),
    };
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    if dark {
        visuals.panel_fill = Color32::from_rgb(0x25, 0x27, 0x2B);
        visuals.window_fill = Color32::from_rgb(0x2B, 0x2D, 0x31);
        visuals.extreme_bg_color = Color32::from_rgb(0x1E, 0x1F, 0x22);
        visuals.faint_bg_color = Color32::from_rgb(0x2A, 0x2C, 0x30);
    }
    visuals.striped = true;
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    ctx.set_zoom_factor(ui_scale);
    ctx.style_mut_of(theme, |s| {
        s.visuals = visuals;
        use egui::{FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::proportional(font_size));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(font_size));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(font_size - 2.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(font_size - 0.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(font_size + 4.0));
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 3.0);
    });
    if dark { Palette::dark() } else { Palette::light() }
}
