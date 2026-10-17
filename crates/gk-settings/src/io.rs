//! File helpers shared by settings and repo state: tolerant load, atomic save.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Loads `path` as TOML. Missing file -> defaults. Unreadable/corrupt file ->
/// backed up to `<name>.bak-<unix-ts>` and defaults returned. Never fails.
pub(crate) fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            eprintln!(
                "gk-settings: cannot read {}: {e}; using defaults",
                path.display()
            );
            backup_broken(path);
            return T::default();
        }
    };
    match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "gk-settings: cannot parse {}: {e}; using defaults",
                path.display()
            );
            backup_broken(path);
            T::default()
        }
    }
}

fn backup_broken(path: &Path) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".bak-{ts}"));
    let bak = path.with_file_name(name);
    match fs::rename(path, &bak) {
        Ok(()) => eprintln!("gk-settings: broken file backed up to {}", bak.display()),
        Err(e) => eprintln!("gk-settings: backup of {} failed: {e}", path.display()),
    }
}

/// Serializes `value` to TOML and writes it atomically (temp file + rename).
pub(crate) fn save_atomic<T: Serialize>(value: &T, path: &Path) -> Result<()> {
    let text = toml::to_string_pretty(value).context("serialize settings")?;
    write_atomic(path, text.as_bytes())
}

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
    let tmp = tmp_path(path);
    let res = (|| -> Result<()> {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        // std::fs::rename replaces the destination on Windows too.
        fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// FNV-1a 64: stable across Rust versions (unlike `DefaultHasher`).
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Path key for equality checks: separators unified, trailing slash dropped,
/// case-folded on Windows.
pub(crate) fn path_key(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}
