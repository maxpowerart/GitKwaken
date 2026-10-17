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

/// Darker lane colors for the light theme (the dark-theme yellow, cyan and
/// light green are hard to see on white).
pub const LANES_LIGHT: [Color32; 8] = [
    Color32::from_rgb(0x1F, 0x6F, 0xD6),
    Color32::from_rgb(0xD9, 0x5A, 0x1F),
    Color32::from_rgb(0x1E, 0x9A, 0x62),
    Color32::from_rgb(0x9B, 0x4D, 0xCA),
    Color32::from_rgb(0xB0, 0x86, 0x00),
    Color32::from_rgb(0x0E, 0x93, 0xA6),
    Color32::from_rgb(0xD0, 0x3B, 0x6E),
    Color32::from_rgb(0x6A, 0x8A, 0x1F),
];

/// Whether [`apply`] last selected the light theme.
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn lane_color(i: u16) -> Color32 {
    let lanes = if LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
        &LANES_LIGHT
    } else {
        &LANES
    };
    lanes[i as usize % lanes.len()]
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
    /// Menu bar and repository tab strip (SourceTree's blue title area).
    pub chrome: Color32,
    /// Strong accent: selected sidebar entry, active view.
    pub accent: Color32,
    /// Text on [`Palette::accent`].
    pub on_accent: Color32,
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
            chrome: Color32::from_rgb(0x16, 0x33, 0x5E),
            accent: Color32::from_rgb(0x2A, 0x7F, 0xE0),
            on_accent: Color32::WHITE,
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
            chrome: Color32::from_rgb(0xD9, 0xE4, 0xF4),
            accent: Color32::from_rgb(0x2A, 0x7F, 0xE0),
            on_accent: Color32::WHITE,
        }
    }
}

/// Default monospace family (Windows ships it everywhere).
pub const DEFAULT_MONO_FONT: &str = "Consolas";

/// Uses the platform UI fonts when present (Segoe UI / Consolas on Windows)
/// and adds Segoe UI Symbol as a fallback so icons like ⑂ ⇄ ⎘ render.
///
/// The monospace family comes from `GITKWAKEN_MONO_FONT`; use
/// [`install_fonts_with`] to pass `settings.mono_font_family`.
pub fn install_fonts(ctx: &egui::Context) {
    let env = std::env::var("GITKWAKEN_MONO_FONT").ok();
    install_fonts_with(ctx, env.as_deref());
}

/// Like [`install_fonts`] with an explicit monospace font: a family name
/// ("Cascadia Mono", "JetBrains Mono"), a file name in the Windows font
/// folders ("CascadiaMono.ttf") or a full path. Unknown names fall back to
/// Consolas.
///
/// Memory: font files are mapped once per process (see
/// [`gk_dx11::map_file_static`]); only touched pages count. Segoe UI Emoji
/// (13 MB, color glyphs egui cannot draw anyway) is not loaded: egui's
/// built-in Noto Emoji and emoji-icon-font, which sit before it in the
/// fallback chain, already cover the pictographs the UI uses.
/// `GITKWAKEN_EMOJI_FONT=1` brings it back for exotic emoji in commit
/// messages.
pub fn install_fonts_with(ctx: &egui::Context, mono: Option<&str>) {
    ctx.set_fonts(font_definitions(mono));
}

fn font_definitions(mono: Option<&str>) -> egui::FontDefinitions {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;
    let mut fonts = FontDefinitions::default();
    // Mapped, not read: egui would copy owned bytes once more, and a mapped
    // font only costs the pages egui touches.
    let load = |p: Option<std::path::PathBuf>| -> Option<Arc<FontData>> {
        gk_dx11::map_file_static(&p?).map(|b| Arc::new(FontData::from_static(b)))
    };
    let mut add = |key: &str, data: Option<Arc<FontData>>, families: &[FontFamily], primary| {
        let Some(data) = data else { return };
        fonts.font_data.insert(key.to_string(), data);
        for family in families {
            let list = fonts.families.entry(family.clone()).or_default();
            if primary {
                list.insert(0, key.to_string());
            } else {
                list.push(key.to_string());
            }
        }
    };
    let both = [FontFamily::Proportional, FontFamily::Monospace];
    add(
        "segoe-ui",
        load(system_font("segoeui.ttf")),
        &[FontFamily::Proportional],
        true,
    );
    let mono_data = mono
        .filter(|m| !m.trim().is_empty())
        .and_then(|m| load(resolve_font(m)))
        .or_else(|| load(resolve_font(DEFAULT_MONO_FONT)));
    add("mono", mono_data, &[FontFamily::Monospace], true);
    add(
        "segoe-symbol",
        load(system_font("seguisym.ttf")),
        &both,
        false,
    );
    if std::env::var_os("GITKWAKEN_EMOJI_FONT").is_some_and(|v| v == "1") {
        add(
            "segoe-emoji",
            load(system_font("seguiemj.ttf")),
            &both,
            false,
        );
    }
    fonts
}

/// Font folders: the system one, then the per-user one (fonts installed
/// without admin rights).
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(w) = std::env::var_os("WINDIR") {
        v.push(std::path::PathBuf::from(w).join("Fonts"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        v.push(std::path::PathBuf::from(l).join(r"Microsoft\Windows\Fonts"));
    }
    v
}

fn system_font(file: &str) -> Option<std::path::PathBuf> {
    font_dirs()
        .into_iter()
        .map(|d| d.join(file))
        .find(|p| p.is_file())
}

/// Maps a family name, file name or path to a font file.
fn resolve_font(name: &str) -> Option<std::path::PathBuf> {
    let name = name.trim();
    let p = std::path::Path::new(name);
    let lower = name.to_ascii_lowercase();
    if [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e)) {
        return if p.is_absolute() {
            p.is_file().then(|| p.to_path_buf())
        } else {
            system_font(name)
        };
    }
    // Family names whose file names are not derivable from the name.
    const KNOWN: &[(&str, &str)] = &[
        ("consolas", "consola.ttf"),
        ("courier new", "cour.ttf"),
        ("lucida console", "lucon.ttf"),
        ("cascadia mono", "CascadiaMono.ttf"),
        ("cascadia code", "CascadiaCode.ttf"),
        ("segoe ui mono", "SegUIMono.ttf"),
    ];
    if let Some((_, file)) = KNOWN.iter().find(|(n, _)| *n == lower) {
        return system_font(file);
    }
    // "JetBrains Mono" -> JetBrainsMono-Regular.ttf, JetBrainsMono.ttf, ...
    let compact: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    ["-Regular.ttf", ".ttf", "-Regular.otf", ".otf"]
        .iter()
        .find_map(|suffix| system_font(&format!("{compact}{suffix}")))
}

/// Applies theme, zoom and text sizes; monospace text (diffs, ids, console)
/// uses `mono_font_size` (`settings.mono_font_size`).
pub fn apply_with(
    ctx: &egui::Context,
    theme: gk_settings::Theme,
    ui_scale: f32,
    font_size: f32,
    mono_font_size: f32,
) -> Palette {
    let dark = match theme {
        gk_settings::Theme::Dark => true,
        gk_settings::Theme::Light => false,
        gk_settings::Theme::System => ctx
            .system_theme()
            .map(|t| t == egui::Theme::Dark)
            .unwrap_or(true),
    };
    LIGHT.store(!dark, std::sync::atomic::Ordering::Relaxed);
    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    if dark {
        visuals.panel_fill = Color32::from_rgb(0x25, 0x27, 0x2B);
        visuals.window_fill = Color32::from_rgb(0x2B, 0x2D, 0x31);
        visuals.extreme_bg_color = Color32::from_rgb(0x1E, 0x1F, 0x22);
        visuals.faint_bg_color = Color32::from_rgb(0x2A, 0x2C, 0x30);
        // SourceTree-like blue accent for selected toggles and text selection.
        visuals.selection.bg_fill = Color32::from_rgb(0x22, 0x6A, 0xC4);
        visuals.selection.stroke.color = Color32::WHITE;
    }
    visuals.striped = true;
    if !dark {
        // Light text fields otherwise blend into panels.
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_gray(190));
    }
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    ctx.set_zoom_factor(ui_scale);
    ctx.style_mut_of(theme, |s| {
        s.visuals = visuals;
        use egui::{FontId, TextStyle};
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(font_size));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(font_size));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(font_size - 2.0));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(mono_font_size));
        s.text_styles
            .insert(TextStyle::Heading, FontId::proportional(font_size + 4.0));
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 3.0);
    });
    if dark {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// UI density (`settings.compact`). Compact, the default, is the look the
/// app always had; Comfortable adds room to every list and widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Compact,
    Comfortable,
}

impl Density {
    pub fn of(compact: bool) -> Self {
        if compact {
            Density::Compact
        } else {
            Density::Comfortable
        }
    }

    /// Extra height of list rows (sidebar, history, search, reflog).
    pub fn row_extra(self) -> f32 {
        match self {
            Density::Compact => 0.0,
            Density::Comfortable => 4.0,
        }
    }

    /// Padding added to the text height of File Status rows.
    pub fn file_row_pad(self) -> f32 {
        match self {
            Density::Compact => 6.0,
            Density::Comfortable => 10.0,
        }
    }

    /// Sidebar row height for a body text of height `text_h`.
    pub fn sidebar_row(self, text_h: f32) -> f32 {
        (text_h + 4.0).max(16.0) + self.row_extra()
    }

    /// History / search / reflog row height from `settings.history.row_height`.
    pub fn history_row(self, setting: f32) -> f32 {
        setting.clamp(14.0, 60.0) + self.row_extra()
    }

    fn spacing(self, s: &mut egui::style::Spacing) {
        let (item, button, interact_h) = match self {
            Density::Compact => (egui::vec2(6.0, 4.0), egui::vec2(6.0, 3.0), 18.0),
            Density::Comfortable => (egui::vec2(8.0, 6.0), egui::vec2(8.0, 5.0), 22.0),
        };
        s.item_spacing = item;
        s.button_padding = button;
        s.interact_size.y = interact_h;
    }
}

/// Applies the global widget spacing of `density` (both themes).
pub fn apply_density(ctx: &egui::Context, density: Density) {
    ctx.all_styles_mut(|s| density.spacing(&mut s.spacing));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_density_keeps_the_classic_look() {
        let ctx = egui::Context::default();
        apply_with(&ctx, gk_settings::Theme::Dark, 1.0, 13.0, 12.5);
        let before = ctx.style_of(egui::Theme::Dark).spacing.clone();
        apply_density(&ctx, Density::Compact);
        let after = ctx.style_of(egui::Theme::Dark).spacing.clone();
        assert_eq!(before.item_spacing, after.item_spacing);
        assert_eq!(before.button_padding, after.button_padding);
        assert_eq!(before.interact_size, after.interact_size);
        let c = Density::Compact;
        assert_eq!(c.file_row_pad(), 6.0);
        assert_eq!(c.history_row(22.0), 22.0);
        assert_eq!(c.sidebar_row(13.0), 17.0);
        // Comfortable is roomier everywhere.
        apply_density(&ctx, Density::Comfortable);
        let roomy = ctx.style_of(egui::Theme::Dark).spacing.clone();
        assert!(roomy.item_spacing.y > after.item_spacing.y);
        assert!(roomy.button_padding.y > after.button_padding.y);
        assert!(roomy.interact_size.y > after.interact_size.y);
        let r = Density::Comfortable;
        assert!(r.file_row_pad() > c.file_row_pad());
        assert!(r.history_row(22.0) > c.history_row(22.0));
        assert!(r.sidebar_row(13.0) > c.sidebar_row(13.0));
        assert_eq!(Density::of(true), c);
    }

    /// Every icon the UI draws must still render without Segoe UI Emoji.
    #[test]
    fn ui_symbols_render_without_emoji_font() {
        let Some(emoji) = system_font("seguiemj.ttf") else {
            return; // not Windows
        };
        // egui's `has_glyph` reports false for glyphs of the face that also
        // holds the replacement character, so compare against the old font
        // set (with Segoe UI Emoji) instead of expecting no misses.
        let missing = |defs: egui::FontDefinitions| -> String {
            let ctx = egui::Context::default();
            ctx.set_fonts(defs);
            let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
            let icons = "…✖—↑⑂→↓✔◀🔍⚠⟳•⇄🏷⬇▸☰✎⬆▼▲−▾📁▣⚙●○↔⎘⏎←≈▶📝🕘👤☁📦🌲⋯⚑📂×";
            ctx.fonts_mut(|f| {
                icons
                    .chars()
                    .filter(|c| {
                        !f.has_glyph(&egui::FontId::proportional(13.0), *c)
                            || !f.has_glyph(&egui::FontId::monospace(12.0), *c)
                    })
                    .collect()
            })
        };
        let mut with_emoji = font_definitions(None);
        let data = egui::FontData::from_owned(std::fs::read(emoji).unwrap());
        with_emoji
            .font_data
            .insert("segoe-emoji".into(), std::sync::Arc::new(data));
        for list in with_emoji.families.values_mut() {
            list.push("segoe-emoji".into());
        }
        assert_eq!(missing(font_definitions(None)), missing(with_emoji));
        let defs = font_definitions(None);
        assert!(!defs.font_data.contains_key("segoe-emoji"));
        assert!(defs.font_data.contains_key("mono"));
    }

    #[test]
    fn mono_font_names_resolve() {
        if system_font("consola.ttf").is_none() {
            return;
        }
        assert!(resolve_font("Consolas").is_some());
        assert!(resolve_font("consola.ttf").is_some());
        assert!(resolve_font("No Such Font 123").is_none());
        let abs = system_font("consola.ttf").unwrap();
        assert_eq!(resolve_font(abs.to_str().unwrap()), Some(abs));
    }

    #[test]
    fn mono_size_is_applied() {
        let ctx = egui::Context::default();
        apply_with(&ctx, gk_settings::Theme::Dark, 1.0, 13.0, 15.5);
        let size = ctx.style_of(egui::Theme::Dark).text_styles[&egui::TextStyle::Monospace].size;
        assert_eq!(size, 15.5);
    }
}
