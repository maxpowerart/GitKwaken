use super::*;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::Arc;

/// Minimal HTTP/1.1 server on 127.0.0.1 serving fixed bodies by path. Runs until the test exits.
fn serve(routes: HashMap<&'static str, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = routes.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                // Drain headers.
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let (status, body) = match routes.get(path) {
                    Some(b) => ("200 OK", b.clone()),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            });
        }
    });
    format!("http://{addr}")
}

fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn sha_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

const SHA0: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn manifest_json(version: &str, url: &str, sha: &str) -> String {
    format!(r#"{{"version":"{version}","notes":"fixes","url":"{url}","sha256":"{sha}"}}"#)
}

// ---------------------------------------------------------------- versions

#[test]
fn version_comparison() {
    assert!(is_newer("0.1.0", "0.1.1").unwrap());
    assert!(is_newer("0.9.9", "1.0.0").unwrap());
    assert!(is_newer("1.0.0", "v1.0.1").unwrap());
    assert!(is_newer("1.0.0-beta.2", "1.0.0").unwrap());
    assert!(is_newer("1.0.0-beta.2", "1.0.0-beta.10").unwrap());
    assert!(!is_newer("1.0.0", "1.0.0").unwrap());
    assert!(!is_newer("1.0.0", "1.0.0+build.5").unwrap());
    assert!(!is_newer("1.0.1", "1.0.0").unwrap());
    assert!(!is_newer("1.0.0", "1.0.0-rc.1").unwrap());
    assert!(!is_newer("2.0.0", "1.99.99").unwrap());
    assert!(is_newer("1.0", "1.0.1").is_err());
    assert!(is_newer("1.0.0", "latest").is_err());
}

// ---------------------------------------------------------------- manifest

#[test]
fn manifest_parsing() {
    let sha = "AB".repeat(32);
    let r = parse_manifest(&manifest_json("v1.2.3", "https://x/y.zip", &sha)).unwrap();
    assert_eq!(r.version, "v1.2.3");
    assert_eq!(r.semver().unwrap(), semver::Version::new(1, 2, 3));
    assert_eq!(r.notes, "fixes");
    assert_eq!(r.sha256, "ab".repeat(32), "sha is normalised to lowercase");
    assert_eq!(r.published, None);

    // Optional fields may be missing; unknown fields ignored.
    let r = parse_manifest(&format!(
        r#"{{"version":"1.0.0","url":"https://a/b.zip","sha256":"{SHA0}","extra":1,"published":"2026-10-17"}}"#
    ))
    .unwrap();
    assert_eq!(r.notes, "");
    assert_eq!(r.published.as_deref(), Some("2026-10-17"));

    // Round-trips through serde (packaging writes the same shape).
    let back = parse_manifest(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn manifest_rejects_bad_input() {
    assert!(parse_manifest("not json").is_err());
    assert!(parse_manifest(r#"{"version":"1.0.0","url":"https://a"}"#).is_err());
    assert!(parse_manifest(&manifest_json("one", "https://a/b.zip", SHA0)).is_err());
    assert!(parse_manifest(&manifest_json("1.0.0", "https://a/b.zip", "abc")).is_err());
    assert!(parse_manifest(&manifest_json("1.0.0", "https://a/b.zip", &"zz".repeat(32))).is_err());
    assert!(parse_manifest(&manifest_json("1.0.0", "file:///c:/evil.zip", SHA0)).is_err());
}

// ---------------------------------------------------------------- network (local server)

#[test]
fn check_against_local_server() {
    let base = serve(HashMap::from([
        (
            "/update.json",
            manifest_json("1.0.1", "https://x/y.zip", SHA0).into_bytes(),
        ),
        ("/bad.json", b"{oops".to_vec()),
    ]));
    let url = format!("{base}/update.json");
    let r = check("1.0.0", &url).unwrap().expect("newer release");
    assert_eq!(r.version, "1.0.1");
    assert!(check("1.0.1", &url).unwrap().is_none());
    assert!(check("2.0.0", &url).unwrap().is_none());
    assert!(check("1.0.0", &format!("{base}/bad.json")).is_err());
    assert!(
        check("1.0.0", &format!("{base}/missing.json")).is_err(),
        "404 is an error"
    );
}

#[test]
fn download_verifies_and_extracts() {
    let exe_bytes: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let zip = make_zip(&[
        ("GitKwaken-1.0.1-x64/LICENSE-MIT", b"mit"),
        ("GitKwaken-1.0.1-x64/GitKwaken.exe", &exe_bytes),
    ]);
    let sha = sha_hex(&zip);
    let base = serve(HashMap::from([("/r.zip", zip.clone())]));
    let tmp = tempfile::tempdir().unwrap();
    let release = parse_manifest(&manifest_json("1.0.1", &format!("{base}/r.zip"), &sha)).unwrap();

    let mut last = (0, 0);
    let mut calls = 0;
    let staged = download_and_stage_in(&release, tmp.path(), |d, t| {
        assert!(d >= last.0, "progress is monotonic");
        last = (d, t);
        calls += 1;
    })
    .unwrap();
    assert_eq!(last, (zip.len() as u64, zip.len() as u64));
    assert!(calls >= 2);
    assert_eq!(fs::read(&staged).unwrap(), exe_bytes);
    assert_eq!(staged.file_name().unwrap(), "gitkwaken-1.0.1.exe");
    // The zip itself is not kept.
    let left: Vec<_> = fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "{left:?}");
}

#[test]
fn download_rejects_sha_mismatch() {
    let zip = make_zip(&[("gitkwaken.exe", b"payload")]);
    let base = serve(HashMap::from([("/r.zip", zip.clone())]));
    let tmp = tempfile::tempdir().unwrap();
    let mut wrong = sha_hex(&zip);
    wrong.replace_range(0..1, if wrong.starts_with('0') { "1" } else { "0" });
    let release =
        parse_manifest(&manifest_json("1.0.1", &format!("{base}/r.zip"), &wrong)).unwrap();
    let err = download_and_stage_in(&release, tmp.path(), |_, _| {}).unwrap_err();
    assert!(format!("{err:#}").contains("SHA-256 mismatch"), "{err:#}");
    // Nothing is left behind (no partial zip, no exe).
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn download_rejects_zip_without_exe() {
    let zip = make_zip(&[("readme.txt", b"hi")]);
    let base = serve(HashMap::from([("/r.zip", zip.clone())]));
    let tmp = tempfile::tempdir().unwrap();
    let release = parse_manifest(&manifest_json(
        "1.0.1",
        &format!("{base}/r.zip"),
        &sha_hex(&zip),
    ))
    .unwrap();
    let err = download_and_stage_in(&release, tmp.path(), |_, _| {}).unwrap_err();
    assert!(format!("{err:#}").contains("not found"), "{err:#}");
}

// ---------------------------------------------------------------- self-replace

#[test]
fn self_replace_and_cleanup() {
    let app = tempfile::tempdir().unwrap();
    let stage = tempfile::tempdir().unwrap();
    let exe = app.path().join("gitkwaken.exe");
    fs::write(&exe, b"old build").unwrap();
    let staged = stage.path().join("gitkwaken-1.0.1.exe");
    fs::write(&staged, b"new build").unwrap();

    replace_exe(&exe, &staged).unwrap();
    assert_eq!(fs::read(&exe).unwrap(), b"new build");
    assert_eq!(
        fs::read(app.path().join("gitkwaken.exe.old")).unwrap(),
        b"old build"
    );
    assert!(!app.path().join("gitkwaken.exe.new").exists());
    assert!(!staged.exists(), "staged file consumed");

    // Second update while a previous .old is still around: it gets replaced.
    fs::write(&staged, b"newer build").unwrap();
    replace_exe(&exe, &staged).unwrap();
    assert_eq!(fs::read(&exe).unwrap(), b"newer build");
    assert_eq!(
        fs::read(app.path().join("gitkwaken.exe.old")).unwrap(),
        b"new build"
    );

    // Next start: leftovers go, unrelated files stay.
    fs::write(app.path().join("gitkwaken.exe.old.1234"), b"x").unwrap();
    fs::write(app.path().join("gitkwaken.exe.new"), b"x").unwrap();
    fs::write(app.path().join("settings.toml"), b"keep").unwrap();
    assert_eq!(cleanup_old_at(&exe), 3);
    let mut names: Vec<_> = fs::read_dir(app.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["gitkwaken.exe", "settings.toml"]);
    assert_eq!(cleanup_old_at(&exe), 0);
}

#[test]
fn self_replace_missing_staged_leaves_exe_untouched() {
    let app = tempfile::tempdir().unwrap();
    let exe = app.path().join("gitkwaken.exe");
    fs::write(&exe, b"old build").unwrap();
    assert!(replace_exe(&exe, &app.path().join("nope.exe")).is_err());
    assert_eq!(fs::read(&exe).unwrap(), b"old build");
    assert_eq!(fs::read_dir(app.path()).unwrap().count(), 1);
}

/// Real Windows semantics: a running image can be renamed but not overwritten.
#[cfg(windows)]
#[test]
fn self_replace_while_exe_is_running() {
    let app = tempfile::tempdir().unwrap();
    let exe = app.path().join("gitkwaken.exe");
    // Use a copy of PING.EXE as the "running" app: `ping -n 30 127.0.0.1` runs long enough.
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap();
    fs::copy(system.join("System32").join("PING.EXE"), &exe).unwrap();
    let mut child = std::process::Command::new(&exe)
        .args(["-n", "30", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(fs::write(&exe, b"x").is_err(), "running exe must be locked");

    let staged = app.path().join("staged.exe");
    fs::write(&staged, b"new build").unwrap();
    replace_exe(&exe, &staged).unwrap();
    assert_eq!(fs::read(&exe).unwrap(), b"new build");

    // .old is still in use: cleanup must fail gracefully, then succeed once the process exits.
    assert_eq!(cleanup_old_at(&exe), 0);
    child.kill().unwrap();
    child.wait().unwrap();
    let mut removed = 0;
    for _ in 0..50 {
        removed += cleanup_old_at(&exe);
        if removed > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(removed, 1);
}

#[test]
fn relative_download_urls_resolve_against_manifest() {
    use crate::resolve_url;
    assert_eq!(
        resolve_url("http://h/r/update.json", "./GitKwaken-1.0.1-x64.zip"),
        "http://h/r/GitKwaken-1.0.1-x64.zip"
    );
    assert_eq!(
        resolve_url("http://h/r/update.json", "https://x/y.zip"),
        "https://x/y.zip"
    );
}

// ---------------------------------------------------------------- Linux

fn make_tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    for (name, data) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, *data).unwrap();
    }
    b.into_inner().unwrap().finish().unwrap()
}

fn linux_manifest(tarball: Option<(&str, &str)>, pacman: Option<(&str, &str)>) -> String {
    let asset = |a: Option<(&str, &str)>| {
        a.map_or("null".to_string(), |(u, s)| {
            format!(r#"{{"url":"{u}","sha256":"{s}"}}"#)
        })
    };
    format!(
        r#"{{"version":"1.0.2","url":"https://x/w.zip","sha256":"{SHA0}","linux":{{"tarball":{},"pacman":{}}}}}"#,
        asset(tarball),
        asset(pacman)
    )
}

#[test]
fn manifest_with_linux_assets() {
    let sha = "CD".repeat(32);
    let r = parse_manifest(&linux_manifest(
        Some(("https://x/t.tar.gz", &sha)),
        Some(("https://x/p.pkg.tar.zst", SHA0)),
    ))
    .unwrap();
    let linux = r.linux.clone().unwrap();
    assert_eq!(linux.tarball.unwrap().sha256, "cd".repeat(32));
    assert_eq!(linux.pacman.unwrap().url, "https://x/p.pkg.tar.zst");
    assert_eq!(
        parse_manifest(&serde_json::to_string(&r).unwrap()).unwrap(),
        r
    );
    // Linux entries are validated like the Windows one.
    assert!(parse_manifest(&linux_manifest(Some(("https://x/t", "abc")), None)).is_err());
    assert!(parse_manifest(&linux_manifest(None, Some(("file:///etc/p", SHA0)))).is_err());
    // Relative Linux URLs resolve against the manifest too.
    let base = serve(HashMap::from([(
        "/c/update.json",
        linux_manifest(Some(("t.tar.gz", SHA0)), None).into_bytes(),
    )]));
    let r = check("1.0.1", &format!("{base}/c/update.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        r.linux.unwrap().tarball.unwrap().url,
        format!("{base}/c/t.tar.gz")
    );
}

#[test]
fn linux_method_follows_the_installation() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("gitkwaken");
    let both = parse_manifest(&linux_manifest(
        Some(("https://x/t.tar.gz", SHA0)),
        Some(("https://x/p.pkg.tar.zst", SHA0)),
    ))
    .unwrap();
    assert!(matches!(
        linux_method(&both, &exe, false).unwrap(),
        Method::Tarball(a) if a.url == "https://x/t.tar.gz"
    ));
    assert!(matches!(
        linux_method(&both, &exe, true).unwrap(),
        Method::Pacman(a) if a.url == "https://x/p.pkg.tar.zst"
    ));
    let tar_only =
        parse_manifest(&linux_manifest(Some(("https://x/t.tar.gz", SHA0)), None)).unwrap();
    let err = linux_method(&tar_only, &exe, true).unwrap_err();
    assert!(format!("{err}").contains("no Arch package"), "{err}");
    let windows_only = parse_manifest(&manifest_json("1.0.2", "https://x/w.zip", SHA0)).unwrap();
    let err = linux_method(&windows_only, &exe, false).unwrap_err();
    assert!(format!("{err}").contains("no Linux build"), "{err}");
    let missing_dir = tmp.path().join("nope").join("gitkwaken");
    let err = linux_method(&both, &missing_dir, false).unwrap_err();
    assert!(format!("{err}").contains("no write access"), "{err}");
}

#[test]
fn tarball_update_stages_the_executable() {
    let bin: Vec<u8> = (0..150_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let tgz = make_tgz(&[
        ("GitKwaken-1.0.2-x86_64-linux/README.md", b"readme"),
        ("GitKwaken-1.0.2-x86_64-linux/icons/gitkwaken.png", b"png"),
        ("GitKwaken-1.0.2-x86_64-linux/gitkwaken", &bin),
    ]);
    let base = serve(HashMap::from([("/t.tar.gz", tgz.clone())]));
    let release = parse_manifest(&linux_manifest(
        Some((&format!("{base}/t.tar.gz"), &sha_hex(&tgz))),
        None,
    ))
    .unwrap();
    let method = Method::Tarball(release.linux.clone().unwrap().tarball.unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_in(&release, &method, tmp.path(), |_, _| {}).unwrap();
    let Staged::Exe(exe) = staged else {
        panic!("{staged:?}")
    };
    assert_eq!(fs::read(&exe).unwrap(), bin);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    let left: Vec<_> = fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "only the executable is kept: {left:?}");

    // A tarball without the executable, and a wrong checksum, stage nothing.
    let empty = make_tgz(&[("x/README.md", b"r")]);
    let base = serve(HashMap::from([("/e.tar.gz", empty.clone())]));
    let bad = Method::Tarball(Asset {
        url: format!("{base}/e.tar.gz"),
        sha256: sha_hex(&empty),
    });
    let tmp = tempfile::tempdir().unwrap();
    let err = stage_in(&release, &bad, tmp.path(), |_, _| {}).unwrap_err();
    assert!(format!("{err:#}").contains("not found"), "{err:#}");
    let wrong = Method::Tarball(Asset {
        url: format!("{base}/e.tar.gz"),
        sha256: SHA0.into(),
    });
    let err = stage_in(&release, &wrong, tmp.path(), |_, _| {}).unwrap_err();
    assert!(format!("{err:#}").contains("SHA-256 mismatch"), "{err:#}");
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn pacman_update_stages_the_package() {
    let pkg = b"\x28\xb5\x2f\xfd fake zstd package".to_vec();
    let base = serve(HashMap::from([(
        "/gitkwaken-bin-1.0.2-1-x86_64.pkg.tar.zst",
        pkg.clone(),
    )]));
    let release = parse_manifest(&linux_manifest(None, None)).unwrap();
    let method = Method::Pacman(Asset {
        url: format!("{base}/gitkwaken-bin-1.0.2-1-x86_64.pkg.tar.zst"),
        sha256: sha_hex(&pkg),
    });
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_in(&release, &method, tmp.path(), |_, _| {}).unwrap();
    let Staged::Package(p) = staged else {
        panic!("{staged:?}")
    };
    assert_eq!(
        p.file_name().unwrap(),
        "gitkwaken-bin-1.0.2-1-x86_64.pkg.tar.zst"
    );
    assert_eq!(fs::read(&p).unwrap(), pkg);
}

#[test]
fn local_folder_channel_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let exe = b"MZ fake exe from a release folder";
    let zip = make_zip(&[("GitKwaken-9.9.9-x64/gitkwaken.exe", exe)]);
    std::fs::write(dir.path().join("GitKwaken-9.9.9-x64.zip"), &zip).unwrap();
    // relative package URL, resolved against the manifest location
    std::fs::write(
        dir.path().join("update.json"),
        manifest_json("9.9.9", "./GitKwaken-9.9.9-x64.zip", &sha_hex(&zip)),
    )
    .unwrap();
    let url = format!(
        "file:///{}",
        dir.path()
            .join("update.json")
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/")
    );
    let rel = check("1.0.0", &url).unwrap().expect("newer release");
    assert!(rel.url.starts_with("file:///"));
    let stage = tempfile::tempdir().unwrap();
    let staged = download_and_stage_in(&rel, stage.path(), |_, _| {}).unwrap();
    assert_eq!(std::fs::read(staged).unwrap(), exe);
    assert!(check("9.9.9", &url).unwrap().is_none());
}

#[test]
fn local_path_forms() {
    assert!(local_path("https://x/y").is_none());
    assert!(local_path("file:///C:/r/update.json").is_some());
    assert_eq!(
        local_path("file://srv/share/u.json").unwrap(),
        PathBuf::from("//srv/share/u.json")
    );
    assert_eq!(
        local_path(r"D:\rel\update.json").unwrap(),
        PathBuf::from(r"D:\rel\update.json")
    );
}
