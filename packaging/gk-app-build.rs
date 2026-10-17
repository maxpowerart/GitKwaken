//! Build script for `gk-app` (install as `crates/gk-app/build.rs`).
//!
//! On Windows targets it embeds a VERSIONINFO block, the app icon (`assets/icon.ico`) and an
//! application manifest (`packaging/gitkwaken.manifest`: per-monitor-v2 DPI awareness, long
//! paths, Common Controls v6, asInvoker) into `gitkwaken.exe`. On other targets it is a no-op.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.join("../..");
    let icon = repo_root.join("assets/icon.ico");
    let app_manifest = repo_root.join("packaging/gitkwaken.manifest");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Check the *target* OS (build scripts run on the host, so cfg!(windows) would be wrong when
    // cross-compiling).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let packed = pack_version(&version);

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("utf-8 path"))
        .set_manifest_file(app_manifest.to_str().expect("utf-8 path"))
        .set("ProductName", "GitKwaken")
        .set("FileDescription", "GitKwaken - fast git client")
        .set("InternalName", "gitkwaken")
        .set("OriginalFilename", "gitkwaken.exe")
        .set("CompanyName", "GitKwaken contributors")
        .set("LegalCopyright", "Copyright (c) GitKwaken contributors. MIT OR Apache-2.0")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    if let Err(e) = res.compile() {
        // Missing rc.exe / windres should not make the whole build unusable; warn loudly instead.
        println!("cargo:warning=failed to embed Windows resources: {e}");
    }
}

/// "1.2.3-beta.1" -> 0x0001_0002_0003_0000 (major.minor.patch.0).
fn pack_version(v: &str) -> u64 {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0) & 0xFFFF);
    let (ma, mi, pa) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    (ma << 48) | (mi << 32) | (pa << 16)
}
