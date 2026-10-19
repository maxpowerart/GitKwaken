//! Colors and visuals from the selected style (see `gk_settings::style`): Kwak by default,
//! Classic (the original SourceTree-like blue), or a user style file.

use egui::{Color32, Visuals};
use gk_settings::style::{Rgba, Style, Variant};
use parking_lot::RwLock;

/// What the last [`apply_with`] chose, for code that draws without a [`Palette`] at hand
/// (graph lanes, icons).
struct Current {
    lanes: Vec<Color32>,
    icon: Color32,
    node: Color32,
    icon_width: f32,
    nodes: bool,
}

static CURRENT: RwLock<Option<Current>> = RwLock::new(None);

fn color(c: Rgba) -> Color32 {
    let [r, g, b, a] = c.0;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// The built-in default style (always valid).
fn kwak() -> Style {
    gk_settings::style::load_in(std::path::Path::new(""), gk_settings::style::DEFAULT_STYLE).0
}

fn with_current<R>(f: impl FnOnce(&Current) -> R) -> R {
    if let Some(c) = CURRENT.read().as_ref() {
        return f(c);
    }
    let s = kwak();
    f(&current_of(&s, false))
}

fn current_of(style: &Style, light: bool) -> Current {
    let v = if light { &style.light } else { &style.dark };
    Current {
        lanes: v.lanes.iter().copied().map(color).collect(),
        icon: color(v.icon),
        node: color(v.node),
        icon_width: style.icons.width,
        nodes: style.icons.nodes,
    }
}

/// Stroke and node colors of the icons ([`crate::icons`]).
pub fn icon_colors() -> (Color32, Color32) {
    with_current(|c| (c.icon, if c.nodes { c.node } else { c.icon }))
}

/// Icon stroke width on the 24-unit design grid.
pub fn icon_width() -> f32 {
    with_current(|c| c.icon_width)
}

pub fn lane_color(i: u16) -> Color32 {
    with_current(|c| c.lanes[i as usize % c.lanes.len().max(1)])
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
    /// Menu bar and repository tab strip.
    pub chrome: Color32,
    /// Strong accent: selected sidebar entry, active view.
    pub accent: Color32,
    /// Text on [`Palette::accent`].
    pub on_accent: Color32,
}

impl Palette {
    pub fn of(v: &Variant, dark: bool) -> Self {
        Self {
            dark,
            added_bg: color(v.added_bg),
            removed_bg: color(v.removed_bg),
            added_word: color(v.added_word),
            removed_word: color(v.removed_word),
            added_fg: color(v.added_fg),
            removed_fg: color(v.removed_fg),
            hunk_bg: color(v.hunk_bg),
            muted: color(v.muted),
            selection: color(v.selection),
            branch: color(v.branch),
            remote: color(v.remote),
            tag: color(v.tag),
            head: color(v.head),
            stash: color(v.stash),
            modified: color(v.modified),
            untracked: color(v.untracked),
            conflict: color(v.conflict),
            chrome: color(v.chrome),
            accent: color(v.accent),
            on_accent: color(v.on_accent),
        }
    }

    /// Kwak, dark (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn dark() -> Self {
        Self::of(&kwak().dark, true)
    }

    /// Kwak, light.
    #[allow(dead_code)]
    pub fn light() -> Self {
        Self::of(&kwak().light, false)
    }
}

/// Default monospace family (Windows ships it everywhere).
pub const DEFAULT_MONO_FONT: &str = "Consolas";

/// Uses the platform UI fonts when present (Segoe UI / Consolas on Windows)
/// and adds Segoe UI Symbol as a fallback so icons like ⑂ ⇄ ⎘ render; on Linux
/// DejaVu Sans / Noto Sans Symbols play that role ([`SYMBOL_FALLBACKS`]).
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
    // Elsewhere (Linux) there is no Segoe UI Symbol: the common symbol fonts cover the
    // same icons (⑂ ⇄ ⬇ ⬆ ⟳ ✔ ✖ ▣ ⚙ ☁ …). Absent files are skipped.
    for (key, file) in SYMBOL_FALLBACKS {
        add(key, load(system_font(file)), &both, false);
    }
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

/// Symbol fonts used as fallbacks after Segoe UI Symbol, by file name: DejaVu Sans
/// (Arch `ttf-dejavu`, Debian `fonts-dejavu-core`), Noto Sans Symbols 1/2 and Noto Sans
/// Math (`noto-fonts`).
const SYMBOL_FALLBACKS: [(&str, &str); 4] = [
    ("dejavu-sans", "DejaVuSans.ttf"),
    ("noto-symbols2", "NotoSansSymbols2-Regular.ttf"),
    ("noto-symbols", "NotoSansSymbols-Regular.ttf"),
    ("noto-math", "NotoSansMath-Regular.ttf"),
];

/// Font folders: the system one, then the per-user one (fonts installed
/// without admin rights). Linux: the XDG font folders.
fn font_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut v = Vec::new();
    if cfg!(windows) {
        if let Some(w) = std::env::var_os("WINDIR") {
            v.push(PathBuf::from(w).join("Fonts"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(l).join(r"Microsoft\Windows\Fonts"));
        }
        return v;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        v.push(PathBuf::from(d).join("fonts"));
    } else if let Some(h) = &home {
        v.push(h.join(".local/share/fonts"));
    }
    if let Some(h) = &home {
        v.push(h.join(".fonts"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    v.extend(data_dirs.split(':').map(|d| PathBuf::from(d).join("fonts")));
    v
}

/// Font files under [`font_dirs`] by lower-case file name, first found wins. Linux keeps
/// fonts in subfolders (`TTF/`, `truetype/dejavu/`, `noto/`), so the folders are walked
/// once (a few levels deep).
fn font_index() -> &'static std::collections::HashMap<String, std::path::PathBuf> {
    use std::sync::OnceLock;
    static INDEX: OnceLock<std::collections::HashMap<String, std::path::PathBuf>> = OnceLock::new();
    INDEX.get_or_init(|| {
        fn walk(
            dir: &std::path::Path,
            depth: u32,
            out: &mut std::collections::HashMap<String, std::path::PathBuf>,
        ) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if depth < 4 {
                        walk(&p, depth + 1, out);
                    }
                } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    out.entry(n.to_ascii_lowercase()).or_insert(p);
                }
            }
        }
        let mut out = std::collections::HashMap::new();
        for d in font_dirs() {
            walk(&d, 0, &mut out);
        }
        out
    })
}

fn system_font(file: &str) -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        // Flat folders: a direct lookup is cheaper than listing ~1000 files.
        return font_dirs()
            .into_iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file());
    }
    font_index().get(&file.to_ascii_lowercase()).cloned()
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
    style: &Style,
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
    *CURRENT.write() = Some(current_of(style, !dark));
    let v = if dark { &style.dark } else { &style.light };
    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    if let Some(c) = v.panel {
        visuals.panel_fill = color(c);
    }
    if let Some(c) = v.window {
        visuals.window_fill = color(c);
    }
    if let Some(c) = v.extreme {
        visuals.extreme_bg_color = color(c);
    }
    if let Some(c) = v.faint {
        visuals.faint_bg_color = color(c);
    }
    // Selected toggles and text selection.
    if let Some(c) = v.text_selection {
        visuals.selection.bg_fill = color(c);
    }
    if let Some(c) = v.text_selection_fg {
        visuals.selection.stroke.color = color(c);
    }
    // Text fields on light panels need a border.
    if let Some(c) = v.border {
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, color(c));
    }
    if let Some(c) = v.text {
        visuals.override_text_color = Some(color(c));
    }
    visuals.striped = true;
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
    Palette::of(v, dark)
}

/// Body text size relative to the default 13 pt. Chrome drawn with fixed sizes (toolbar
/// buttons, row icons, badges, check boxes) is multiplied by it, so a bigger font never
/// overflows its box; at the default size it is exactly 1.
pub fn text_scale(style: &egui::Style) -> f32 {
    let body = style
        .text_styles
        .get(&egui::TextStyle::Body)
        .map_or(13.0, |f| f.size);
    (body / 13.0).clamp(0.6, 4.0)
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
    /// `scale` is [`text_scale`]: rows grow with a bigger font (unchanged at the default).
    pub fn history_row(self, setting: f32, scale: f32) -> f32 {
        (setting.clamp(14.0, 60.0) * scale.max(1.0)).round() + self.row_extra()
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
        apply_with(&ctx, gk_settings::Theme::Dark, &kwak(), 1.0, 13.0, 12.5);
        let before = ctx.style_of(egui::Theme::Dark).spacing.clone();
        apply_density(&ctx, Density::Compact);
        let after = ctx.style_of(egui::Theme::Dark).spacing.clone();
        assert_eq!(before.item_spacing, after.item_spacing);
        assert_eq!(before.button_padding, after.button_padding);
        assert_eq!(before.interact_size, after.interact_size);
        let c = Density::Compact;
        assert_eq!(c.file_row_pad(), 6.0);
        assert_eq!(c.history_row(22.0, 1.0), 22.0);
        assert_eq!(c.history_row(22.0, 1.5), 33.0);
        assert_eq!(c.sidebar_row(13.0), 17.0);
        // Comfortable is roomier everywhere.
        apply_density(&ctx, Density::Comfortable);
        let roomy = ctx.style_of(egui::Theme::Dark).spacing.clone();
        assert!(roomy.item_spacing.y > after.item_spacing.y);
        assert!(roomy.button_padding.y > after.button_padding.y);
        assert!(roomy.interact_size.y > after.interact_size.y);
        let r = Density::Comfortable;
        assert!(r.file_row_pad() > c.file_row_pad());
        assert!(r.history_row(22.0, 1.0) > c.history_row(22.0, 1.0));
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
        apply_with(&ctx, gk_settings::Theme::Dark, &kwak(), 1.0, 13.0, 15.5);
        let size = ctx.style_of(egui::Theme::Dark).text_styles[&egui::TextStyle::Monospace].size;
        assert_eq!(size, 15.5);
    }
}
