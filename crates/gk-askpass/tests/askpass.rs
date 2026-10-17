//! End-to-end: real git / ssh-keygen invoke the helper binary, which talks to
//! an in-process `Server` whose prompts are answered by a background thread.

use gk_askpass::{PromptKind, Server};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_gk-askpass-test");

/// Answers every prompt via `respond` and records what was asked.
fn with_answerer<T>(
    server: &Server,
    respond: impl Fn(PromptKind, &str) -> Option<String> + Sync,
    body: impl FnOnce() -> T,
) -> (T, Vec<(PromptKind, String)>) {
    let done = AtomicBool::new(false);
    let seen = Mutex::new(Vec::new());
    let out = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                match server.pending() {
                    Some(p) => {
                        seen.lock().unwrap().push((p.kind, p.text.clone()));
                        server.answer(p.id, respond(p.kind, &p.text));
                    }
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        let out = body();
        done.store(true, Ordering::SeqCst);
        out
    });
    (out, seen.into_inner().unwrap())
}

fn askpass_cmd(server: &Server, program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.envs(server.env(Path::new(HELPER))).stdin(Stdio::null());
    cmd
}

#[test]
fn helper_prints_answer_through_pipe() {
    let server = Server::start(Box::new(|| {})).unwrap();
    let run = |prompt: &str| askpass_cmd(&server, HELPER).arg(prompt).output().unwrap();
    let respond = |kind, _: &str| (kind == PromptKind::Password).then(|| "p@ss w0rd".to_string());

    let (out, seen) = with_answerer(&server, respond, || run("Password for 'https://x@h': "));
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"p@ss w0rd\n");
    assert_eq!(
        seen,
        [(
            PromptKind::Password,
            "Password for 'https://x@h': ".to_string()
        )]
    );

    let (out, _) = with_answerer(&server, respond, || run("Username for 'https://h': "));
    assert_eq!(out.status.code(), Some(1), "cancel must exit 1");
    assert!(out.stdout.is_empty());

    // Without GITKWAKEN_ASKPASS the hook returns None (normal GUI start).
    let plain = Command::new(HELPER)
        .env_remove("GITKWAKEN_ASKPASS")
        .output()
        .unwrap();
    assert_eq!(plain.status.code(), Some(2));
}

#[test]
fn helper_fails_cleanly_with_bad_token() {
    let server = Server::start(Box::new(|| {})).unwrap();
    let port = server.env(Path::new(HELPER)).pop().unwrap().1;
    let port = port.split_once(':').unwrap().0;
    let out = askpass_cmd(&server, HELPER)
        .env("GITKWAKEN_ASKPASS", format!("{port}:{}", "0".repeat(64)))
        .arg("Password: ")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(server.pending().is_none());
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Minimal HTTP server: 401 + `WWW-Authenticate: Basic` until the expected
/// credentials arrive, then a dumb-protocol `info/refs`. Records every
/// Authorization header it sees.
fn spawn_http(expected_auth: String) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            conn.set_read_timeout(Some(Duration::from_secs(10))).ok();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match conn.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let req = String::from_utf8_lossy(&req);
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            let auth = req
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Authorization: ")
                        .or(l.strip_prefix("authorization: "))
                })
                .map(str::to_string);
            if let Some(a) = &auth {
                log.lock().unwrap().push(a.clone());
            }
            let resp = if auth.as_deref() != Some(expected_auth.as_str()) {
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"test\"\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            } else if path.starts_with("/x.git/info/refs") {
                let body = "0123456789abcdef0123456789abcdef01234567\trefs/heads/main\n";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            };
            let _ = conn.write_all(resp.as_bytes());
        }
    });
    (port, seen)
}

#[test]
fn git_http_basic_auth_via_askpass() {
    let expected = format!("Basic {}", base64(b"alice:s3cr et"));
    let (http_port, auth_seen) = spawn_http(expected.clone());
    let server = Server::start(Box::new(|| {})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let empty_cfg = dir.path().join("empty.gitconfig");
    std::fs::write(&empty_cfg, "").unwrap();

    let url = format!("http://127.0.0.1:{http_port}/x.git");
    let respond = |kind, _: &str| match kind {
        PromptKind::Username => Some("alice".to_string()),
        PromptKind::Password => Some("s3cr et".to_string()),
        _ => None,
    };
    let (out, seen): (Output, _) = with_answerer(&server, respond, || {
        askpass_cmd(&server, "git")
            .args(["-c", "credential.helper=", "ls-remote", &url])
            .current_dir(dir.path())
            // Same as gk-cli's runner, plus isolation from user/system config.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &empty_cfg)
            .env("NO_PROXY", "*")
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("GIT_ASKPASS_OVERRIDE")
            .output()
            .unwrap()
    });
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "git failed: {stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("refs/heads/main"));
    let kinds: Vec<_> = seen.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        kinds,
        [PromptKind::Username, PromptKind::Password],
        "{seen:?}"
    );
    assert!(seen[0].1.contains(&format!("127.0.0.1:{http_port}")));
    assert!(auth_seen.lock().unwrap().contains(&expected));
}

#[test]
fn ssh_keygen_passphrase_via_ssh_askpass() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("k");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "pass phrase", "-C", "t", "-f"])
        .arg(&key)
        .stdin(Stdio::null())
        .output();
    if !generated.is_ok_and(|o| o.status.success()) {
        eprintln!("ssh-keygen unavailable, skipping");
        return;
    }

    let server = Server::start(Box::new(|| {})).unwrap();
    let respond =
        |kind, _: &str| (kind == PromptKind::Passphrase).then(|| "pass phrase".to_string());
    let (out, seen) = with_answerer(&server, respond, || {
        askpass_cmd(&server, "ssh-keygen")
            .arg("-y")
            .arg("-f")
            .arg(&key)
            .output()
            .unwrap()
    });
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ssh-keygen failed: {stderr} {seen:?}");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ssh-ed25519 "));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, PromptKind::Passphrase, "{seen:?}");
}
