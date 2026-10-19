//! Styles: colors, icon look and graph lanes, for the dark and the light variant.
//!
//! A style is a TOML file (see `assets/styles/kwak.toml`, the complete reference). Built-in
//! styles are compiled in; user styles are the `*.toml` files in [`styles_dir`]. Any key a
//! style leaves out comes from the style named in `inherits` (Kwak when not set), so a user
//! style can be as short as `name = "Mine"` plus the colors it changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// Name of the default style.
pub const DEFAULT_STYLE: &str = "Kwak";

/// Built-in styles: (name, TOML source). The first one is the base of every other style.
pub const BUILTIN: [(&str, &str); 2] = [
    ("Kwak", include_str!("../../../assets/styles/kwak.toml")),
    (
        "Classic",
        include_str!("../../../assets/styles/classic.toml"),
    ),
];

/// An RGBA color (unmultiplied alpha), written "#RRGGBB" or "#RRGGBBAA".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub fn parse(s: &str) -> Result<Rgba> {
        let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
        if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid color {s:?} (expected \"#RRGGBB\" or \"#RRGGBBAA\")");
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
        let a = if h.len() == 8 { byte(6) } else { 255 };
        Ok(Rgba([byte(0), byte(2), byte(4), a]))
    }

    pub fn hex(self) -> String {
        let [r, g, b, a] = self.0;
        if a == 255 {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }
}

/// Declares the color keys of a variant: the file form (`VariantDef`, every key optional), the
/// resolved form (`Variant`) and how a definition is laid over a base.
macro_rules! variant {
    (
        required { $($req:ident),* $(,)? }
        optional { $($opt:ident),* $(,)? }
    ) => {
        /// One variant (dark or light) as written in a style file.
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct VariantDef {
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $req: Option<String>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $opt: Option<String>,)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub lanes: Option<Vec<String>>,
        }

        /// A resolved variant. Optional colors left unset keep egui's defaults.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Variant {
            $(pub $req: Rgba,)*
            $(pub $opt: Option<Rgba>,)*
            pub lanes: Vec<Rgba>,
        }

        impl Variant {
            /// `def` over `base`; `ctx` names the variant in errors.
            fn resolve(def: &VariantDef, base: Option<&Variant>, ctx: &str) -> Result<Variant> {
                let color = |v: &Option<String>, key: &str| -> Result<Option<Rgba>> {
                    v.as_deref()
                        .map(|s| Rgba::parse(s).with_context(|| format!("[{ctx}] {key}")))
                        .transpose()
                };
                let lanes = match &def.lanes {
                    Some(v) if !v.is_empty() => v
                        .iter()
                        .map(|s| Rgba::parse(s).with_context(|| format!("[{ctx}] lanes")))
                        .collect::<Result<Vec<_>>>()?,
                    Some(_) => bail!("[{ctx}] lanes: at least one color"),
                    None => base.map(|b| b.lanes.clone()).ok_or_else(|| anyhow!("[{ctx}] lanes missing"))?,
                };
                Ok(Variant {
                    $($req: match (color(&def.$req, stringify!($req))?, base) {
                        (Some(c), _) => c,
                        (None, Some(b)) => b.$req,
                        (None, None) => bail!("[{ctx}] {} missing", stringify!($req)),
                    },)*
                    $($opt: color(&def.$opt, stringify!($opt))?.or(base.and_then(|b| b.$opt)),)*
                    lanes,
                })
            }

            /// Back to the file form, every key written.
            fn to_def(&self) -> VariantDef {
                VariantDef {
                    $($req: Some(self.$req.hex()),)*
                    $($opt: self.$opt.map(Rgba::hex),)*
                    lanes: Some(self.lanes.iter().map(|c| c.hex()).collect()),
                }
            }
        }
    };
}

variant! {
    required {
        chrome, accent, on_accent, icon, node, selection, muted,
        branch, remote, tag, head, stash, modified, untracked, conflict,
        added_fg, removed_fg, added_bg, removed_bg, added_word, removed_word, hunk_bg,
    }
    optional {
        panel, window, extreme, faint, text_selection, text_selection_fg, border, text,
    }
}

/// Icon look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconParams {
    /// Stroke width on the 24-unit design grid.
    pub width: f32,
    /// Commit nodes in the node color (else everything in the stroke color).
    pub nodes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IconsDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<bool>,
}

/// A style file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StyleDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherits: Option<String>,
    pub icons: IconsDef,
    pub dark: VariantDef,
    pub light: VariantDef,
}

/// A resolved style.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub name: String,
    pub icons: IconParams,
    pub dark: Variant,
    pub light: Variant,
}

impl Style {
    /// The style as a complete file (every key written), for "New style from this one".
    pub fn to_toml(&self, name: &str) -> String {
        let def = StyleDef {
            name: Some(name.to_string()),
            inherits: None,
            icons: IconsDef {
                width: Some(self.icons.width),
                nodes: Some(self.icons.nodes),
            },
            dark: self.dark.to_def(),
            light: self.light.to_def(),
        };
        let body = toml::to_string_pretty(&def).unwrap_or_default();
        format!(
            "# GitKwaken style, made from \"{}\". Remove any key to inherit it from Kwak;\n\
             # see assets/styles/kwak.toml in the GitKwaken sources for what each key does.\n\n{body}",
            self.name
        )
    }
}

/// Folder of user styles: `<config>/styles`.
pub fn styles_dir() -> PathBuf {
    crate::Settings::config_dir().join("styles")
}

/// A style offered in Settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub name: String,
    /// `None` for built-in styles.
    pub path: Option<PathBuf>,
    /// Why the file cannot be used (it is listed, but not selectable).
    pub error: Option<String>,
}

fn parse_def(src: &str) -> Result<StyleDef> {
    toml::from_str(src).map_err(|e| anyhow!("{}", e.message()))
}

/// User style files in `dir`: (name, path, definition or error), sorted by name. A file
/// without `name` is named after its file stem.
fn user_defs(dir: &Path) -> Vec<(String, PathBuf, Result<StyleDef>)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
        })
        .map(|p| {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let def = std::fs::read_to_string(&p)
                .context("cannot read the file")
                .and_then(|s| parse_def(&s));
            let name = def
                .as_ref()
                .ok()
                .and_then(|d| d.name.clone())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(stem);
            (name, p, def)
        })
        .collect();
    out.sort_by_key(|a| a.0.to_lowercase());
    out
}

/// Every definition by name: built-ins, then user files in `dir` (a user file may not take a
/// built-in name).
fn all_defs(dir: &Path) -> BTreeMap<String, (Option<PathBuf>, Result<StyleDef>)> {
    let mut map = BTreeMap::new();
    for (name, src) in BUILTIN {
        map.insert(name.to_string(), (None, parse_def(src)));
    }
    for (name, path, def) in user_defs(dir) {
        if BUILTIN.iter().any(|(b, _)| b.eq_ignore_ascii_case(&name)) {
            continue;
        }
        map.entry(name).or_insert((Some(path), def));
    }
    map
}

/// Resolves `name` with its `inherits` chain (Kwak at the root).
fn resolve(
    name: &str,
    defs: &BTreeMap<String, (Option<PathBuf>, Result<StyleDef>)>,
    depth: usize,
) -> Result<Style> {
    if depth > 8 {
        bail!("`inherits` goes in a circle");
    }
    let (_, def) = defs
        .get(name)
        .ok_or_else(|| anyhow!("no style named {name:?}"))?;
    let def = def.as_ref().map_err(|e| anyhow!("{e:#}"))?;
    let base = if name == DEFAULT_STYLE {
        None
    } else {
        let parent = def.inherits.as_deref().unwrap_or(DEFAULT_STYLE);
        Some(resolve(parent, defs, depth + 1).with_context(|| format!("inherits {parent:?}"))?)
    };
    let icons = IconParams {
        width: def
            .icons
            .width
            .or(base.as_ref().map(|b| b.icons.width))
            .unwrap_or(2.0)
            .clamp(0.5, 4.0),
        nodes: def
            .icons
            .nodes
            .or(base.as_ref().map(|b| b.icons.nodes))
            .unwrap_or(true),
    };
    Ok(Style {
        name: name.to_string(),
        icons,
        dark: Variant::resolve(&def.dark, base.as_ref().map(|b| &b.dark), "dark")?,
        light: Variant::resolve(&def.light, base.as_ref().map(|b| &b.light), "light")?,
    })
}

/// Styles for Settings: built-ins first, then user styles from `dir`, each with the reason it
/// cannot be used, if any.
pub fn list_in(dir: &Path) -> Vec<Listed> {
    let defs = all_defs(dir);
    let mut builtin: Vec<Listed> = Vec::new();
    let mut user: Vec<Listed> = Vec::new();
    for (name, (path, _)) in &defs {
        let error = resolve(name, &defs, 0).err().map(|e| format!("{e:#}"));
        let item = Listed {
            name: name.clone(),
            path: path.clone(),
            error,
        };
        if path.is_none() {
            builtin.push(item);
        } else {
            user.push(item);
        }
    }
    // Built-ins in their declared order, user styles by name.
    builtin.sort_by_key(|l| BUILTIN.iter().position(|(n, _)| *n == l.name));
    user.sort_by_key(|l| l.name.to_lowercase());
    builtin.extend(user);
    builtin
}

/// [`list_in`] for [`styles_dir`].
pub fn list() -> Vec<Listed> {
    list_in(&styles_dir())
}

/// The style `name` from `dir`, or Kwak with the reason when it cannot be used.
pub fn load_in(dir: &Path, name: &str) -> (Style, Option<String>) {
    let defs = all_defs(dir);
    match resolve(name, &defs, 0) {
        Ok(s) => (s, None),
        Err(e) => (
            resolve(DEFAULT_STYLE, &defs, 0).expect("the built-in Kwak style is valid"),
            Some(format!("style {name:?}: {e:#}")),
        ),
    }
}

/// [`load_in`] for [`styles_dir`].
pub fn load(name: &str) -> (Style, Option<String>) {
    load_in(&styles_dir(), name)
}

/// Writes `style` as a new, complete user style "<name> copy" in `dir` and returns its path
/// and name.
pub fn create_copy_in(dir: &Path, style: &Style) -> Result<(PathBuf, String)> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let taken = list_in(dir);
    let mut n = 1;
    let name = loop {
        let candidate = if n == 1 {
            format!("{} copy", style.name)
        } else {
            format!("{} copy {n}", style.name)
        };
        let file = dir.join(format!("{}.toml", file_stem(&candidate)));
        if !file.exists()
            && !taken
                .iter()
                .any(|l| l.name.eq_ignore_ascii_case(&candidate))
        {
            break candidate;
        }
        n += 1;
    };
    let path = dir.join(format!("{}.toml", file_stem(&name)));
    std::fs::write(&path, style.to_toml(&name))
        .with_context(|| format!("write {}", path.display()))?;
    Ok((path, name))
}

/// [`create_copy_in`] for [`styles_dir`].
pub fn create_copy(style: &Style) -> Result<(PathBuf, String)> {
    create_copy_in(&styles_dir(), style)
}

fn file_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_styles_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let (kwak, err) = load_in(dir.path(), "Kwak");
        assert_eq!(err, None);
        assert_eq!(kwak.dark.icon, Rgba([0x3C, 0xBA, 0xAF, 255]));
        assert!(kwak.icons.nodes);
        let (classic, err) = load_in(dir.path(), "Classic");
        assert_eq!(err, None);
        assert!(!classic.icons.nodes);
        assert_eq!(classic.dark.chrome, Rgba([0x16, 0x33, 0x5E, 255]));
        // Not set in classic.toml: inherited from Kwak.
        assert_eq!(classic.dark.conflict, kwak.dark.conflict);
        assert_eq!(classic.dark.panel, kwak.dark.panel);
        let names: Vec<String> = list_in(dir.path()).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["Kwak", "Classic"]);
    }

    #[test]
    fn user_styles_inherit_and_report_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mine.toml"),
            "name = \"Mine\"\ninherits = \"Classic\"\n[dark]\naccent = \"#FF0000\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.toml"), "[dark]\naccent = \"red\"\n").unwrap();
        std::fs::write(
            dir.path().join("typo.toml"),
            "[dark]\naccnet = \"#FF0000\"\n",
        )
        .unwrap();
        let list = list_in(dir.path());
        let names: Vec<&str> = list.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Kwak", "Classic", "broken", "Mine", "typo"]);
        assert!(list[2].error.as_deref().unwrap().contains("accent"));
        assert!(list[3].error.is_none());
        assert!(list[4].error.is_some(), "unknown keys are reported");
        let (mine, err) = load_in(dir.path(), "Mine");
        assert_eq!(err, None);
        assert_eq!(mine.dark.accent, Rgba([255, 0, 0, 255]));
        assert_eq!(
            mine.dark.chrome,
            Rgba([0x16, 0x33, 0x5E, 255]),
            "from Classic"
        );
        // A broken style falls back to Kwak, with the reason.
        let (s, err) = load_in(dir.path(), "broken");
        assert_eq!(s.name, "Kwak");
        assert!(err.unwrap().contains("broken"));
    }

    #[test]
    fn a_copy_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (classic, _) = load_in(dir.path(), "Classic");
        let (path, name) = create_copy_in(dir.path(), &classic).unwrap();
        assert_eq!(name, "Classic copy");
        assert!(path.exists());
        let (copy, err) = load_in(dir.path(), "Classic copy");
        assert_eq!(err, None);
        assert_eq!(copy.dark, classic.dark);
        assert_eq!(copy.light, classic.light);
        assert_eq!(copy.icons, classic.icons);
        let (_, second) = create_copy_in(dir.path(), &classic).unwrap();
        assert_eq!(second, "Classic copy 2");
    }

    #[test]
    fn colors_parse() {
        assert_eq!(
            Rgba::parse("#3CBAAF").unwrap(),
            Rgba([0x3C, 0xBA, 0xAF, 255])
        );
        assert_eq!(
            Rgba::parse("#3cbaaf80").unwrap(),
            Rgba([0x3C, 0xBA, 0xAF, 0x80])
        );
        assert!(Rgba::parse("teal").is_err());
        assert!(Rgba::parse("#12345").is_err());
        assert_eq!(Rgba([1, 2, 3, 4]).hex(), "#01020304");
    }
}
