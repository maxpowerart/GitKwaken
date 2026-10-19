//! Self-update for GitKwaken.
//!
//! Flow:
//! 1. At startup call [`cleanup_old`] (removes the `.old` exe left by a previous update).
//! 2. On a background thread call [`check`] with the running version and a channel URL
//!    (usually [`DEFAULT_CHANNEL_URL`]). `Some(release)` means a newer version exists.
//! 3. [`method`] picks how this installation takes it: the Windows zip, the Linux tarball, or
//!    (when pacman owns the executable) the Arch package.
//! 4. If the user accepts, [`stage`] downloads that file and verifies its SHA-256. The zip and
//!    the tarball yield the new executable in a staging directory; the package is installed
//!    right away with [`install_package`] (`pkexec pacman -U`).
//! 5. When the app exits, [`apply_on_exit`] swaps a staged executable into place (the running
//!    one is renamed to `<exe>.old`, which Windows allows while it runs and Linux always does).
//!
//! # Channel manifest (`update.json`)
//!
//! ```json
//! {
//!   "version": "1.0.1",
//!   "notes": "Changelog text (plain / markdown)",
//!   "url": "https://github.com/<owner>/gitkwaken/releases/download/v1.0.1/GitKwaken-1.0.1-x64.zip",
//!   "sha256": "<64 hex chars of the zip>",
//!   "published": "2026-10-17",
//!   "linux": {
//!     "tarball": { "url": "…/GitKwaken-1.0.1-x86_64-linux.tar.gz", "sha256": "…" },
//!     "pacman": { "url": "…/gitkwaken-bin-1.0.1-1-x86_64.pkg.tar.zst", "sha256": "…" }
//!   }
//! }
//! ```
//! `notes`, `published` and `linux` (and each of its entries) are optional. `version` may carry
//! a leading `v`. `url` / `sha256` are the Windows zip (older clients read only those).

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Release channel baked in at build time from `GITKWAKEN_UPDATE_CHANNEL`
/// (the release workflow sets it to the repository's `latest/download/update.json`).
/// `None` for local builds, which then only use a channel configured at runtime.
pub const DEFAULT_CHANNEL_URL: Option<&str> = option_env!("GITKWAKEN_UPDATE_CHANNEL");

/// File name of the executable inside the release zip (matched case-insensitively, any folder).
pub const EXE_NAME: &str = "gitkwaken.exe";

/// File name of the executable inside the Linux tarball (any folder).
pub const LINUX_EXE_NAME: &str = "gitkwaken";

/// Max size accepted for the channel manifest.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// An available release, as described by the channel manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    /// The Windows zip.
    pub url: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<LinuxAssets>,
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

/// The Linux x86_64 files of a release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxAssets {
    /// `GitKwaken-<ver>-x86_64-linux.tar.gz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tarball: Option<Asset>,
    /// `gitkwaken-bin-<ver>-<rel>-x86_64.pkg.tar.zst`, built from the release's PKGBUILD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pacman: Option<Asset>,
}

impl Release {
    /// Parsed semantic version (leading `v` tolerated).
    pub fn semver(&self) -> Result<semver::Version> {
        parse_version(&self.version)
    }

    fn assets_mut(&mut self) -> impl Iterator<Item = (&mut String, &mut String)> {
        let linux = self.linux.iter_mut().flat_map(|l| {
            [l.tarball.as_mut(), l.pacman.as_mut()]
                .into_iter()
                .flatten()
                .map(|a| (&mut a.url, &mut a.sha256))
        });
        std::iter::once((&mut self.url, &mut self.sha256)).chain(linux)
    }
}

/// Parses and validates a channel manifest.
pub fn parse_manifest(json: &str) -> Result<Release> {
    let mut r: Release = serde_json::from_str(json).context("invalid update manifest JSON")?;
    r.semver()?;
    for (url, sha256) in r.assets_mut() {
        ensure!(
            sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "update manifest: sha256 must be 64 hex chars"
        );
        sha256.make_ascii_lowercase();
        // http(s) or relative to the manifest (local channels use relative URLs).
        // Absolute file:// or other schemes are rejected so a remote manifest can
        // never point at local files.
        ensure!(
            url.starts_with("https://") || url.starts_with("http://") || !url.contains(':'),
            "update manifest: url must be http(s) or relative to the manifest"
        );
    }
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
    let release = fetch_manifest(channel_url)?;
    Ok(is_newer(current, &release.version)?.then_some(release))
}

/// The manifest at `url` (any version), its download URLs resolved.
///
/// Blocking (network): call from a background thread.
pub fn fetch_manifest(url: &str) -> Result<Release> {
    let (reader, _) = open_source(url, Duration::from_secs(20))?;
    let mut body = String::new();
    reader
        .take(MANIFEST_LIMIT)
        .read_to_string(&mut body)
        .context("read update manifest")?;
    let mut release = parse_manifest(&body)?;
    for (u, _) in release.assets_mut() {
        *u = resolve_url(url, u);
    }
    Ok(release)
}

/// A published release that can be installed (it carries an `update.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub version: String,
    /// `YYYY-MM-DD`.
    pub published: String,
    pub prerelease: bool,
    /// Withdrawn after release (`.github/workflows/withdraw.yml`): its title carries
    /// "(withdrawn)". Never offered; installs of it are offered a way back.
    pub withdrawn: bool,
    /// Why it was withdrawn (the "Withdrawn: …" line of its notes), if given.
    pub reason: String,
    /// Its own `update.json`, for [`fetch_manifest`].
    pub manifest_url: String,
}

/// Marker the withdraw workflow puts in a release title.
pub const WITHDRAWN_MARK: &str = "(withdrawn)";

/// The "Withdrawn: <reason>" line the withdraw workflow puts at the top of the notes.
fn withdrawn_reason(body: &str) -> String {
    body.lines()
        .map(|l| l.trim().trim_start_matches("**").replace("**", ""))
        .find_map(|l| l.strip_prefix("Withdrawn:").map(|r| r.trim().to_string()))
        .unwrap_or_default()
}

/// GitHub API URL listing the releases of the repository a GitHub channel
/// (`https://github.com/<owner>/<repo>/releases/latest/download/update.json`) belongs to.
pub fn releases_api_url(channel_url: &str) -> Option<String> {
    let rest = channel_url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || parts.next() != Some("releases") {
        return None;
    }
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases?per_page=50"
    ))
}

/// Releases of the channel's repository, newest first: drafts and releases without an
/// `update.json` (nothing to install from) are left out.
///
/// Blocking (network): call from a background thread.
pub fn list_releases(channel_url: &str) -> Result<Vec<Listed>> {
    let api = releases_api_url(channel_url)
        .ok_or_else(|| anyhow!("only GitHub release channels list older versions"))?;
    let (reader, _) = open_source(&api, Duration::from_secs(20))?;
    let mut body = String::new();
    reader
        .take(16 * MANIFEST_LIMIT)
        .read_to_string(&mut body)
        .context("read release list")?;
    parse_release_list(&body)
}

/// [`list_releases`] on a GitHub API response body (testable core).
pub fn parse_release_list(json: &str) -> Result<Vec<Listed>> {
    let v: serde_json::Value = serde_json::from_str(json).context("invalid release list")?;
    let items = v
        .as_array()
        .ok_or_else(|| anyhow!("invalid release list"))?;
    let mut out: Vec<Listed> = items
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            parse_version(tag).ok()?;
            let manifest_url = r["assets"]
                .as_array()?
                .iter()
                .find(|a| a["name"].as_str() == Some("update.json"))?["browser_download_url"]
                .as_str()?
                .to_string();
            Some(Listed {
                version: tag.trim_start_matches(['v', 'V']).to_string(),
                published: r["published_at"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(10)
                    .collect(),
                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                withdrawn: r["name"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase().contains(WITHDRAWN_MARK)),
                reason: withdrawn_reason(r["body"].as_str().unwrap_or("")),
                manifest_url,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let (x, y) = (parse_version(&a.version), parse_version(&b.version));
        match (x, y) {
            (Ok(x), Ok(y)) => y.cmp_precedence(&x),
            _ => std::cmp::Ordering::Equal,
        }
    });
    Ok(out)
}

/// Resolves a download URL that is relative to the manifest's location
/// (e.g. `GitKwaken-1.0.1-x64.zip` next to `update.json`).
pub fn resolve_url(manifest_url: &str, url: &str) -> String {
    if url.contains("://") {
        return url.to_string();
    }
    let base = manifest_url
        .rsplit_once('/')
        .map_or(manifest_url, |(b, _)| b);
    format!("{base}/{}", url.trim_start_matches("./"))
}

/// How the running installation takes an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// The Windows zip; its `gitkwaken.exe` is swapped in on exit.
    WindowsZip(Asset),
    /// The Linux tarball; its `gitkwaken` is swapped in on exit (an install the user can write).
    Tarball(Asset),
    /// The Arch package, installed with `pkexec pacman -U` (pacman owns the executable).
    Pacman(Asset),
}

/// A downloaded, verified update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    /// New executable for [`apply_on_exit`].
    Exe(PathBuf),
    /// Package for [`install_package`].
    Package(PathBuf),
}

/// How this installation updates to `release`, or why it cannot.
pub fn method(release: &Release) -> Result<Method> {
    if cfg!(windows) {
        return Ok(Method::WindowsZip(Asset {
            url: release.url.clone(),
            sha256: release.sha256.clone(),
        }));
    }
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let exe = std::env::current_exe().context("locate current exe")?;
        let exe = fs::canonicalize(&exe).unwrap_or(exe);
        return linux_method(release, &exe, pacman_owns(&exe));
    }
    bail!("GitKwaken does not update itself on this platform")
}

/// [`method`] on Linux for the executable at `exe` (testable core).
pub fn linux_method(release: &Release, exe: &Path, owned_by_pacman: bool) -> Result<Method> {
    let linux = release.linux.clone().unwrap_or_default();
    if owned_by_pacman {
        return linux
            .pacman
            .map(Method::Pacman)
            .ok_or_else(|| anyhow!("release {} has no Arch package", release.version));
    }
    let tarball = linux
        .tarball
        .ok_or_else(|| anyhow!("release {} has no Linux build", release.version))?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    ensure!(
        writable(dir),
        "no write access to {}: update GitKwaken with the tool that installed it",
        dir.display()
    );
    Ok(Method::Tarball(tarball))
}

/// Whether the installed package `pacman -Qo` reports owns `exe`.
fn pacman_owns(exe: &Path) -> bool {
    std::process::Command::new("pacman")
        .arg("-Qqo")
        .arg(exe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether a file can be created in `dir` (permissions alone miss read-only mounts).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".gitkwaken-update-{}", std::process::id()));
    let ok = fs::File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// Directory used for downloads and the staged exe.
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("gitkwaken-update")
}

/// Downloads the file `method` names into [`staging_dir`] and verifies its SHA-256; the zip and
/// the tarball are unpacked to the new executable. `progress(downloaded, total)` is called as
/// bytes arrive (`total` is 0 if unknown).
///
/// Blocking (network): call from a background thread.
pub fn stage(release: &Release, method: &Method, progress: impl FnMut(u64, u64)) -> Result<Staged> {
    stage_in(release, method, &staging_dir(), progress)
}

/// [`stage`] with an explicit staging directory.
pub fn stage_in(
    release: &Release,
    method: &Method,
    dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<Staged> {
    let version = release.semver()?;
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    match method {
        Method::WindowsZip(asset) => {
            let part = dir.join(format!("GitKwaken-{version}.zip.part"));
            let exe = dir.join(format!("gitkwaken-{version}.exe"));
            let r = download(asset, &part, progress).and_then(|()| extract_exe(&part, &exe));
            let _ = fs::remove_file(&part);
            r.map(|()| Staged::Exe(exe))
        }
        Method::Tarball(asset) => {
            let part = dir.join(format!("GitKwaken-{version}.tar.gz.part"));
            let exe = dir.join(format!("gitkwaken-{version}"));
            let r = download(asset, &part, progress).and_then(|()| extract_tar_exe(&part, &exe));
            let _ = fs::remove_file(&part);
            r.map(|()| Staged::Exe(exe))
        }
        Method::Pacman(asset) => {
            // pacman reads the compression from the extension.
            let name = asset
                .url
                .rsplit('/')
                .next()
                .filter(|n| n.ends_with(".pkg.tar.zst") && !n.contains(['\\', '\0']))
                .map_or_else(
                    || format!("gitkwaken-bin-{version}.pkg.tar.zst"),
                    String::from,
                );
            let pkg = dir.join(name);
            let part = with_suffix(&pkg, ".part");
            let r = download(asset, &part, progress).and_then(|()| replace_file(&part, &pkg));
            if r.is_err() {
                let _ = fs::remove_file(&part);
            }
            r.map(|()| Staged::Package(pkg))
        }
    }
}

/// The Windows zip update ([`stage`] with [`Method::WindowsZip`]); returns the staged exe.
pub fn download_and_stage(release: &Release, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    download_and_stage_in(release, &staging_dir(), progress)
}

/// [`download_and_stage`] with an explicit staging directory.
pub fn download_and_stage_in(
    release: &Release,
    dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    let zip = Method::WindowsZip(Asset {
        url: release.url.clone(),
        sha256: release.sha256.clone(),
    });
    match stage_in(release, &zip, dir, progress)? {
        Staged::Exe(p) | Staged::Package(p) => Ok(p),
    }
}

/// Downloads `asset` to `dest` and checks its size and SHA-256.
fn download(asset: &Asset, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let (mut reader, total) = open_source(&asset.url, Duration::from_secs(30 * 60))?;
    let mut file = fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
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
    if !actual.eq_ignore_ascii_case(&asset.sha256) {
        bail!(
            "SHA-256 mismatch for {}: expected {}, got {actual}",
            asset.url,
            asset.sha256
        );
    }
    Ok(())
}

/// Extracts the first regular file named [`LINUX_EXE_NAME`] (any directory) of a `.tar.gz` to
/// `dest`, executable.
pub fn extract_tar_exe(tgz: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(tgz).with_context(|| format!("open {}", tgz.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries().context("invalid release tarball")? {
        let mut entry = entry.context("invalid release tarball")?;
        let is_exe = entry.header().entry_type().is_file()
            && entry
                .path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n == LINUX_EXE_NAME))
                .unwrap_or(false);
        if !is_exe {
            continue;
        }
        let tmp = with_suffix(dest, ".tmp");
        {
            let mut out =
                fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            std::io::copy(&mut entry, &mut out).context("extract executable")?;
            out.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        }
        return replace_file(&tmp, dest);
    }
    bail!("{LINUX_EXE_NAME} not found in release tarball")
}

/// Installs a staged Arch package with `pkexec pacman -U` (polkit asks for the password).
///
/// Blocking: waits for the authentication dialog and pacman.
pub fn install_package(pkg: &Path) -> Result<()> {
    let out = std::process::Command::new("pkexec")
        .args(["pacman", "-U", "--noconfirm"])
        .arg(pkg)
        .stdin(std::process::Stdio::null())
        .output()
        .context("start pkexec (polkit) to run pacman")?;
    let _ = fs::remove_file(pkg);
    match out.status.code() {
        Some(0) => Ok(()),
        // pkexec: dialog dismissed / not authorized.
        Some(126 | 127) => bail!("administrator authorization was cancelled or denied"),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            bail!(
                "pacman -U failed{}",
                if err.is_empty() {
                    String::new()
                } else {
                    format!(": {err}")
                }
            )
        }
    }
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

/// Local channels: `file:///D:/releases/update.json`, `file://server/share/...`
/// or a plain path. Lets a team (or a single machine) publish releases to a
/// folder or network share without any web server.
pub fn local_path(url: &str) -> Option<PathBuf> {
    let rest = if let Some(r) = url.strip_prefix("file:///") {
        // file:///C:/x on Windows, file:///home/x elsewhere
        if cfg!(windows) {
            r.to_string()
        } else {
            format!("/{r}")
        }
    } else if let Some(r) = url.strip_prefix("file://") {
        format!("//{r}") // UNC share
    } else if !url.contains("://") {
        url.to_string()
    } else {
        return None;
    };
    Some(PathBuf::from(rest.replace("%20", " ")))
}

/// Opens a manifest or package from HTTP(S) or a local channel; returns the
/// reader and the total size when known.
fn open_source(url: &str, timeout: Duration) -> Result<(Box<dyn Read + Send>, u64)> {
    if let Some(path) = local_path(url) {
        let file = fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        return Ok((Box::new(file), len));
    }
    let resp = agent(timeout)
        .get(url)
        .call()
        .with_context(|| format!("fetch {url}"))?;
    let total = resp.body().content_length().unwrap_or(0);
    Ok((Box::new(resp.into_body().into_reader()), total))
}
