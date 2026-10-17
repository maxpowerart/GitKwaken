//! Self-update for GitKwaken.
//!
//! Flow:
//! 1. At startup call [`cleanup_old`] (removes the `.old` exe left by a previous update).
//! 2. On a background thread call [`check`] with the running version and a channel URL
//!    (usually [`DEFAULT_CHANNEL_URL`]). `Some(release)` means a newer version exists.
//! 3. If the user accepts, [`download_and_stage`] downloads the release zip, verifies its
//!    SHA-256 and extracts `gitkwaken.exe` into a staging directory.
//! 4. When the app exits, [`apply_on_exit`] swaps the staged exe into place (the running exe is
//!    renamed to `gitkwaken.exe.old`, which Windows allows; it cannot be deleted while running).
//!
//! # Channel manifest (`update.json`)
//!
//! ```json
//! {
//!   "version": "1.0.1",
//!   "notes": "Changelog text (plain / markdown)",
//!   "url": "https://github.com/OWNER/gitkwaken/releases/download/v1.0.1/GitKwaken-1.0.1-x64.zip",
//!   "sha256": "<64 hex chars of the zip>",
//!   "published": "2026-10-17"
//! }
//! ```
//! `notes` and `published` are optional. `version` may carry a leading `v`.

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Placeholder channel; `OWNER` must be replaced with the real GitHub org/user before release.
pub const DEFAULT_CHANNEL_URL: &str =
    "https://github.com/OWNER/gitkwaken/releases/latest/download/update.json";

/// File name of the executable inside the release zip (matched case-insensitively, any folder).
pub const EXE_NAME: &str = "gitkwaken.exe";

/// Max size accepted for the channel manifest.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// An available release, as described by the channel manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

impl Release {
    /// Parsed semantic version (leading `v` tolerated).
    pub fn semver(&self) -> Result<semver::Version> {
        parse_version(&self.version)
    }
}

/// Parses and validates a channel manifest.
pub fn parse_manifest(json: &str) -> Result<Release> {
    let mut r: Release = serde_json::from_str(json).context("invalid update manifest JSON")?;
    r.semver()?;
    ensure!(
        r.sha256.len() == 64 && r.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "update manifest: sha256 must be 64 hex chars"
    );
    r.sha256.make_ascii_lowercase();
    ensure!(
        r.url.starts_with("https://") || r.url.starts_with("http://"),
        "update manifest: url must be http(s)"
    );
    Ok(r)
}

/// Parses `1.2.3`, `v1.2.3`, `1.2.3-beta.1`.
pub fn parse_version(v: &str) -> Result<semver::Version> {
    let t = v.trim();
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    semver::Version::parse(t).with_context(|| format!("invalid version {v:?}"))
}

/// True if `candidate` is strictly newer than `current` (semver precedence; build metadata ignored).
pub fn is_newer(current: &str, candidate: &str) -> Result<bool> {
    let cur = parse_version(current)?;
    let cand = parse_version(candidate)?;
    Ok(cand.cmp_precedence(&cur).is_gt())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("GitKwaken-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Fetches the channel manifest and returns it if it is newer than `current`.
///
/// Blocking (network): call from a background thread.
pub fn check(current: &str, channel_url: &str) -> Result<Option<Release>> {
    let mut resp = agent(Duration::from_secs(20))
        .get(channel_url)
        .header("Accept", "application/json")
        .call()
        .with_context(|| format!("fetch {channel_url}"))?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(MANIFEST_LIMIT)
        .read_to_string()
        .context("read update manifest")?;
    let release = parse_manifest(&body)?;
    Ok(is_newer(current, &release.version)?.then_some(release))
}

/// Directory used for downloads and the staged exe.
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("gitkwaken-update")
}

/// Downloads the release zip into [`staging_dir`], verifies SHA-256, extracts the exe and returns
/// its path. `progress(downloaded, total)` is called as bytes arrive (`total` is 0 if unknown).
///
/// Blocking (network): call from a background thread.
pub fn download_and_stage(release: &Release, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    download_and_stage_in(release, &staging_dir(), progress)
}

/// [`download_and_stage`] with an explicit staging directory.
pub fn download_and_stage_in(
    release: &Release,
    dir: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    let version = release.semver()?;
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let zip_path = dir.join(format!("GitKwaken-{version}.zip.part"));

    let result = (|| -> Result<PathBuf> {
        let mut resp = agent(Duration::from_secs(30 * 60))
            .get(&release.url)
            .call()
            .with_context(|| format!("download {}", release.url))?;
        let total = resp.body().content_length().unwrap_or(0);
        let mut reader = resp.body_mut().as_reader();
        let mut file = fs::File::create(&zip_path)
            .with_context(|| format!("create {}", zip_path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = reader.read(&mut buf).context("download interrupted")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        file.sync_all()?;
        drop(file);
        ensure!(
            total == 0 || done == total,
            "download truncated: {done} of {total} bytes"
        );

        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(&release.sha256) {
            bail!(
                "SHA-256 mismatch for {}: expected {}, got {actual}",
                release.url,
                release.sha256
            );
        }

        let exe = dir.join(format!("gitkwaken-{version}.exe"));
        extract_exe(&zip_path, &exe)?;
        Ok(exe)
    })();
    let _ = fs::remove_file(&zip_path);
    result
}

/// Extracts the first entry named [`EXE_NAME`] (any directory, case-insensitive) to `dest`.
pub fn extract_exe(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("invalid release zip")?;
    let index = (0..archive.len())
        .find(|&i| {
            matches!(archive.name_for_index(i), Some(Ok(name)) if {
                let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
                base.eq_ignore_ascii_case(EXE_NAME)
            })
        })
        .ok_or_else(|| anyhow!("{EXE_NAME} not found in release zip"))?;
    let mut entry = archive.by_index(index)?;
    let tmp = with_suffix(dest, ".tmp");
    {
        let mut out =
            fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        std::io::copy(&mut entry, &mut out).context("extract exe")?;
        out.sync_all()?;
    }
    replace_file(&tmp, dest)
}

/// Swaps `staged_exe` in place of the currently running executable. Call when the app is exiting
/// (after the UI has closed); the new version is used on the next launch.
pub fn apply_on_exit(staged_exe: &Path) -> Result<()> {
    let current = std::env::current_exe().context("locate current exe")?;
    let current = fs::canonicalize(&current).unwrap_or(current);
    replace_exe(&current, staged_exe)
}

/// Self-replace on an explicit path (testable core of [`apply_on_exit`]).
///
/// 1. copy `staged` next to `exe` as `<exe>.new` (same volume, so the final step is a rename);
/// 2. rename `exe` -> `<exe>.old` (allowed on Windows even while the image is running);
/// 3. rename `<exe>.new` -> `exe`; on failure roll back step 2.
///
/// `<exe>.old` is deleted by [`cleanup_old`] on the next start.
pub fn replace_exe(exe: &Path, staged: &Path) -> Result<()> {
    ensure!(staged.is_file(), "staged exe {} missing", staged.display());
    let new = with_suffix(exe, ".new");
    let old = with_suffix(exe, ".old");

    fs::copy(staged, &new)
        .with_context(|| format!("copy {} -> {}", staged.display(), new.display()))?;

    // A leftover .old from an earlier update may still exist (e.g. its process was running when
    // cleanup ran). Try to remove it; if that fails, park it under a unique name.
    if old.exists() && fs::remove_file(&old).is_err() {
        let parked = with_suffix(exe, &format!(".old.{}", std::process::id()));
        fs::rename(&old, &parked).ok();
    }

    if let Err(e) = fs::rename(exe, &old) {
        let _ = fs::remove_file(&new);
        return Err(e).with_context(|| format!("rename {} -> {}", exe.display(), old.display()));
    }
    if let Err(e) = fs::rename(&new, exe) {
        // Roll back so the app still starts.
        let _ = fs::rename(&old, exe);
        let _ = fs::remove_file(&new);
        return Err(e).with_context(|| format!("install {}", exe.display()));
    }
    let _ = fs::remove_file(staged);
    Ok(())
}

/// Removes leftovers of a previous update next to the running exe (`.old`, `.old.<pid>`, `.new`)
/// and the staging directory. Best-effort; call once at startup.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let exe = fs::canonicalize(&exe).unwrap_or(exe);
        cleanup_old_at(&exe);
    }
    let _ = fs::remove_dir_all(staging_dir());
}

/// [`cleanup_old`] for an explicit exe path. Returns the number of files removed.
pub fn cleanup_old_at(exe: &Path) -> usize {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let old_prefix = format!("{name}.old");
    let new_name = format!("{name}.new");
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&old_prefix) || n == new_name)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        fs::remove_file(to).with_context(|| format!("remove {}", to.display()))?;
    }
    fs::rename(from, to).with_context(|| format!("rename {} -> {}", from.display(), to.display()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// SHA-256 of a file as lowercase hex (used by packaging / tests).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex(&h.finalize()))
}

#[cfg(test)]
mod tests;
