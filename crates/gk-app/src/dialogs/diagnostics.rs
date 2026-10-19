//! Optimize › Diagnostics: measures how fast git writes loose objects on the
//! repository's volume (200 blobs with `hash-object -w` into a throw-away
//! repository next to it). Slow writes on Windows are almost always real-time
//! antivirus scanning; the hint names the exact Defender exclusion and Dev
//! Drive commands (never run by the app).
//!
//! Also the Authentication diagnostics: credential helpers and Git Credential
//! Manager, which ssh git runs, whether an ssh agent answers (and its keys),
//! and a connection test for a remote URL. Detection goes through [`Probe`]
//! so the parsing and decision logic is tested with fakes.

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Color32, Context, RichText};
use gk_cli::{CancelToken, CommandLog, Git};
use gk_core::Snapshot;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Objects written by the measurement.
pub const OBJECTS: usize = 200;
/// Above this many milliseconds per object, writes count as slow.
pub const SLOW_MS: f64 = 2.0;

#[derive(Debug, Clone)]
pub struct WriteSpeed {
    pub per_object_ms: f64,
    pub total_ms: f64,
    /// `Some(true)` on a Dev Drive (from `fsutil devdrv query`), `None` when unknown.
    pub dev_drive: Option<bool>,
}

impl WriteSpeed {
    pub fn slow(&self) -> bool {
        self.per_object_ms > SLOW_MS
    }
}

/// A scratch folder on the same volume as `workdir`: next to it, else in its `.git`.
fn scratch_dir(workdir: &Path, git_dir: &Path) -> Result<PathBuf, String> {
    let name = format!(".gitkwaken-diag-{}", std::process::id());
    let candidates = [
        workdir.parent().map(|p| p.join(&name)),
        Some(git_dir.join(&name)),
    ];
    for c in candidates.into_iter().flatten() {
        if std::fs::create_dir_all(&c).is_ok() {
            return Ok(c);
        }
    }
    Err("Cannot create a scratch folder on the repository's volume.".into())
}

/// Runs the measurement (blocking; call from a worker thread).
pub fn measure(workdir: &Path, git_dir: &Path, log: &CommandLog) -> Result<WriteSpeed, String> {
    let dir = scratch_dir(workdir, git_dir)?;
    let r = measure_in(&dir, log);
    let _ = std::fs::remove_dir_all(&dir);
    let (per_object_ms, total_ms) = r?;
    Ok(WriteSpeed {
        per_object_ms,
        total_ms,
        dev_drive: dev_drive(workdir, log),
    })
}

fn measure_in(dir: &Path, log: &CommandLog) -> Result<(f64, f64), String> {
    let e = |e: std::io::Error| e.to_string();
    let git = Git::new(dir, log.clone());
    git.run(&["init", "-q", "."])
        .map_err(|e| format!("{e:#}"))?;
    let blobs = dir.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(e)?;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut list = String::new();
    for i in 0..OBJECTS {
        let p = blobs.join(format!("b{i:03}"));
        // Unique content: every object is a new file in .git/objects.
        let body = format!("gitkwaken write test {seed} {i}\n{}\n", "x".repeat(64 + i));
        std::fs::write(&p, body).map_err(e)?;
        list.push_str(&p.to_string_lossy());
        list.push('\n');
    }
    let start = Instant::now();
    let out = git
        .run_raw(
            &["hash-object", "-w", "--stdin-paths"],
            Some(list.as_bytes()),
        )
        .map_err(|e| format!("{e:#}"))?;
    let total = start.elapsed().as_secs_f64() * 1000.0;
    if !out.success() {
        return Err(out.stderr_str().trim().to_string());
    }
    let written = out.stdout_str().lines().count();
    if written != OBJECTS {
        return Err(format!("git wrote {written} of {OBJECTS} objects"));
    }
    Ok((total / OBJECTS as f64, total))
}

/// `fsutil devdrv query X:` (Windows 11): whether the volume is a Dev Drive.
fn dev_drive(path: &Path, log: &CommandLog) -> Option<bool> {
    if !cfg!(windows) {
        return None;
    }
    let _ = log;
    let abs = std::path::absolute(path).ok()?;
    let s = abs.to_string_lossy();
    let drive = s.get(..2).filter(|d| d.ends_with(':'))?;
    let out = std::process::Command::new("fsutil")
        .args(["devdrv", "query", drive])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    if text.contains("not a developer volume") || text.contains("not a dev drive") {
        Some(false)
    } else if text.contains("developer volume") || text.contains("dev drive") {
        Some(true)
    } else {
        None
    }
}

/// PowerShell commands (elevated) that exclude the repository and git from
/// Defender real-time scanning.
pub fn defender_commands(workdir: &Path, git_exe: &Path) -> String {
    let repo = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    format!(
        "Add-MpPreference -ExclusionPath '{}'\nAdd-MpPreference -ExclusionProcess '{}'",
        repo.display(),
        git_exe.display()
    )
}

/// Dev Drive hint: how to check a volume and where to create one.
pub fn dev_drive_hint(workdir: &Path) -> String {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let s = abs.to_string_lossy().into_owned();
    let drive = s.get(..2).filter(|d| d.ends_with(':')).unwrap_or("D:");
    format!(
        "fsutil devdrv query {drive}\n# Create one: Settings › System › Storage › Advanced storage settings › Disks & volumes › Create Dev Drive"
    )
}

// ================================================================ authentication

/// Output of a probed program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcOut {
    /// `None`: killed after the timeout (or no exit code).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ProcOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout and stderr, trimmed and joined.
    fn text(&self) -> String {
        [self.stdout.trim(), self.stderr.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The outside world as authentication detection sees it (faked in tests).
pub trait Probe {
    /// `git <args>` in the repository; `None` when git cannot start.
    fn git(&self, args: &[&str]) -> Option<ProcOut>;
    /// Runs a program (with a short timeout); `None` when it cannot start.
    fn run(&self, program: &Path, args: &[&str]) -> Option<ProcOut>;
    fn is_file(&self, path: &Path) -> bool;
    fn env(&self, key: &str) -> Option<String>;
}

/// The real [`Probe`]: git of the repository, processes, files, environment.
pub struct SystemProbe {
    git: Git,
}

impl SystemProbe {
    pub fn new(git: Git) -> Self {
        Self { git }
    }
}

/// Timeout of the quick probes (`ssh-add -l`, `sc query`, `--version`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout of a connection test.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

impl Probe for SystemProbe {
    fn git(&self, args: &[&str]) -> Option<ProcOut> {
        let o = self.git.run_raw(args, None).ok()?;
        Some(ProcOut {
            code: o.code,
            stdout: o.stdout_str(),
            stderr: o.stderr_str(),
        })
    }

    fn run(&self, program: &Path, args: &[&str]) -> Option<ProcOut> {
        run_with_timeout(program, args, &[], PROBE_TIMEOUT)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Runs `program` without a console window, killing it after `timeout`
/// (the result then has no exit code). `None` when it cannot start.
pub fn run_with_timeout(
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Option<ProcOut> {
    use std::io::Read;
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut b);
            }
            b
        })
    }
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    if timed_out {
        stderr.push_str(&format!("\nTimed out after {} s.", timeout.as_secs()));
    }
    Some(ProcOut {
        code,
        stdout: String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned(),
        stderr,
    })
}

// ---------------------------------------------------------------- credentials

/// One configured `credential.helper` value and its config scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helper {
    /// `system`, `global`, `local`... (empty when unknown).
    pub scope: String,
    pub value: String,
}

/// Parses `git config --show-scope --get-all credential.helper` (one
/// `scope<TAB>value` per line). An empty value resets the list, as in git.
pub fn parse_helpers(out: &str) -> Vec<Helper> {
    let mut v = Vec::new();
    for line in out.lines() {
        let (scope, value) = match line.split_once('\t') {
            Some((s, val)) => (s.trim(), val.trim()),
            None => ("", line.trim()),
        };
        if value.is_empty() {
            v.clear();
            continue;
        }
        v.push(Helper {
            scope: scope.to_string(),
            value: value.to_string(),
        });
    }
    v
}

/// Whether a `credential.helper` value runs Git Credential Manager.
pub fn is_gcm_helper(value: &str) -> bool {
    let v = value.trim().to_ascii_lowercase().replace('\\', "/");
    let first = v
        .trim_start_matches('!')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\'']);
    matches!(first, "manager" | "manager-core") || v.contains("git-credential-manager")
}

/// Install root of Git for Windows from `git --exec-path`
/// (`C:/Program Files/Git/mingw64/libexec/git-core` → `C:/Program Files/Git`).
/// `None` for other layouts (Linux / macOS packages).
pub fn git_root_from_exec_path(exec: &str) -> Option<PathBuf> {
    let norm = exec.trim().replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    let lower = norm.to_ascii_lowercase();
    for tail in [
        "/mingw64/libexec/git-core",
        "/mingw32/libexec/git-core",
        "/clangarm64/libexec/git-core",
    ] {
        if lower.ends_with(tail) {
            let root = &norm[..norm.len() - tail.len()];
            return (!root.is_empty()).then(|| PathBuf::from(root));
        }
    }
    None
}

const GCM_NAMES: [&str; 2] = ["git-credential-manager", "git-credential-manager-core"];

/// Finds the GCM executable: in the Git for Windows install (`mingw64/bin`
/// since Git 2.39, `libexec/git-core` before), git's exec path, then PATH.
pub fn find_gcm(
    p: &dyn Probe,
    git_root: Option<&Path>,
    exec_path: Option<&Path>,
    windows: bool,
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(r) = git_root {
        for sub in [
            "mingw64/bin",
            "mingw64/libexec/git-core",
            "clangarm64/bin",
            "clangarm64/libexec/git-core",
        ] {
            dirs.push(r.join(sub));
        }
    }
    if let Some(e) = exec_path {
        dirs.push(e.to_path_buf());
    }
    if let Some(path) = p.env("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    for name in GCM_NAMES {
        let file = if windows {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        if let Some(c) = dirs.iter().map(|d| d.join(&file)).find(|c| p.is_file(c)) {
            return Some(c);
        }
    }
    None
}

/// `2.5.0+d349307...` → `2.5.0`.
pub fn parse_gcm_version(out: &str) -> Option<String> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    let v = line.split('+').next()?.trim();
    v.starts_with(|c: char| c.is_ascii_digit())
        .then(|| v.to_string())
}

/// The `credential.helper` value that selects this GCM executable.
pub fn gcm_helper_name(exe: &Path) -> &'static str {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if stem.ends_with("manager-core") {
        "manager-core"
    } else {
        "manager"
    }
}

// ---------------------------------------------------------------- ssh

/// Where git takes its ssh command from, in git's order of precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshSource {
    /// `GIT_SSH_COMMAND` environment variable.
    GitSshCommandEnv,
    /// `core.sshCommand` config.
    CoreSshCommand,
    /// `GIT_SSH` environment variable (a program, no arguments).
    GitSshEnv,
    /// Plain `ssh` on git's PATH.
    #[default]
    Default,
}

impl SshSource {
    pub fn label(self) -> &'static str {
        match self {
            SshSource::GitSshCommandEnv => "GIT_SSH_COMMAND environment variable",
            SshSource::CoreSshCommand => "core.sshCommand",
            SshSource::GitSshEnv => "GIT_SSH environment variable",
            SshSource::Default => "default (ssh on git's PATH)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshFlavor {
    /// OpenSSH bundled with Git for Windows (agent via `SSH_AUTH_SOCK`).
    GitForWindows,
    /// `C:\Windows\System32\OpenSSH` (agent: the `ssh-agent` service).
    WindowsOpenSsh,
    /// PuTTY plink / TortoisePlink (agent: Pageant).
    Plink,
    #[default]
    Other,
}

impl SshFlavor {
    pub fn label(self) -> &'static str {
        match self {
            SshFlavor::GitForWindows => "OpenSSH bundled with Git for Windows",
            SshFlavor::WindowsOpenSsh => "Windows OpenSSH",
            SshFlavor::Plink => "PuTTY plink (keys from Pageant)",
            SshFlavor::Other => "OpenSSH",
        }
    }
}

/// The ssh git runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshCommand {
    pub source: SshSource,
    /// The command as configured.
    pub command: String,
    /// Split into program and arguments.
    pub words: Vec<String>,
    /// The program to start (a bare `ssh` resolved to Git's own on Windows).
    pub exe: PathBuf,
    pub flavor: SshFlavor,
}

/// Splits a command line on whitespace, honoring '…' and "…" quotes.
/// Backslashes stay literal (Windows paths).
pub fn split_command(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            None => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn norm_lower(p: &str) -> String {
    p.replace('\\', "/").to_ascii_lowercase()
}

fn classify_ssh(program: &str, git_root: Option<&Path>, windows: bool) -> SshFlavor {
    let p = norm_lower(program);
    let stem = p
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .to_string();
    if p.contains("system32/openssh/") {
        return SshFlavor::WindowsOpenSsh;
    }
    if stem == "plink" || stem == "tortoiseplink" {
        return SshFlavor::Plink;
    }
    if let Some(root) = git_root {
        let root = norm_lower(&root.to_string_lossy());
        let root = root.trim_end_matches('/');
        if p.starts_with(&format!("{root}/")) {
            return SshFlavor::GitForWindows;
        }
        // Git for Windows puts its own usr/bin first on PATH.
        if windows && !p.contains('/') && stem == "ssh" {
            return SshFlavor::GitForWindows;
        }
    }
    SshFlavor::Other
}

/// The ssh git uses: `GIT_SSH_COMMAND`, then `core.sshCommand`, then
/// `GIT_SSH`, else `ssh` (Git for Windows' own on Windows).
pub fn resolve_ssh(
    env_command: Option<&str>,
    core_command: Option<&str>,
    env_ssh: Option<&str>,
    git_root: Option<&Path>,
    windows: bool,
) -> SshCommand {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    let (source, command, words) = if let Some(c) = set(env_command) {
        (SshSource::GitSshCommandEnv, c.clone(), split_command(&c))
    } else if let Some(c) = set(core_command) {
        (SshSource::CoreSshCommand, c.clone(), split_command(&c))
    } else if let Some(c) = set(env_ssh) {
        (SshSource::GitSshEnv, c.clone(), vec![c])
    } else if let (true, Some(root)) = (windows, git_root) {
        let exe = root
            .join("usr/bin/ssh.exe")
            .to_string_lossy()
            .replace('\\', "/");
        (SshSource::Default, exe.clone(), vec![exe])
    } else {
        (
            SshSource::Default,
            "ssh".to_string(),
            vec!["ssh".to_string()],
        )
    };
    let program = words.first().cloned().unwrap_or_else(|| "ssh".into());
    let flavor = classify_ssh(&program, git_root, windows);
    let bare = !program.contains(['/', '\\']);
    let exe = match git_root {
        Some(root) if flavor == SshFlavor::GitForWindows && bare => {
            let name = if program.to_ascii_lowercase().ends_with(".exe") {
                program.clone()
            } else {
                format!("{program}.exe")
            };
            root.join("usr/bin").join(name)
        }
        _ => PathBuf::from(&program),
    };
    SshCommand {
        source,
        command,
        words,
        exe,
        flavor,
    }
}

/// The `ssh-add` that talks to the same agent as `ssh`; `None` for plink.
pub fn ssh_add_path(ssh: &SshCommand, windows: bool) -> Option<PathBuf> {
    if ssh.flavor == SshFlavor::Plink {
        return None;
    }
    let name = if windows { "ssh-add.exe" } else { "ssh-add" };
    match ssh.exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => Some(dir.join(name)),
        None => Some(PathBuf::from(name)),
    }
}

/// A key the agent holds (`ssh-add -l`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKey {
    pub bits: String,
    pub fingerprint: String,
    pub comment: String,
    /// `ED25519`, `RSA`...
    pub kind: String,
}

/// Parses `256 SHA256:abc… user@host (ED25519)`.
pub fn parse_key_line(line: &str) -> Option<SshKey> {
    let mut it = line.trim().splitn(3, ' ');
    let bits = it.next()?.to_string();
    let fingerprint = it.next()?.to_string();
    let rest = it.next().unwrap_or("").trim();
    let (comment, kind) = match rest.rsplit_once(" (") {
        Some((c, k)) if k.ends_with(')') => (c.trim(), k.trim_end_matches(')')),
        _ if rest.starts_with('(') && rest.ends_with(')') => ("", &rest[1..rest.len() - 1]),
        _ => (rest, ""),
    };
    Some(SshKey {
        bits,
        fingerprint,
        comment: comment.to_string(),
        kind: kind.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Agent {
    /// The agent answered with these keys.
    Keys(Vec<SshKey>),
    /// The agent runs but holds no keys.
    NoKeys,
    /// No agent reachable (ssh-add's message).
    Unreachable(String),
    Error(String),
    /// Not checked, with the reason.
    #[default]
    NotChecked,
}

/// `ssh-add -l`: exit 0 = keys listed, 1 = no keys, 2 = no agent.
pub fn parse_ssh_add(out: &ProcOut) -> Agent {
    match out.code {
        Some(0) => {
            let keys: Vec<SshKey> = out.stdout.lines().filter_map(parse_key_line).collect();
            if keys.is_empty() {
                Agent::NoKeys
            } else {
                Agent::Keys(keys)
            }
        }
        Some(1) => Agent::NoKeys,
        Some(2) => Agent::Unreachable(out.text()),
        _ => Agent::Error(out.text()),
    }
}

/// State of the Windows `ssh-agent` service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    Running,
    Stopped,
    /// Not installed (the OpenSSH client feature is missing).
    Missing,
    Other(String),
}

/// Parses `sc query ssh-agent` (`STATE : 4  RUNNING`; error 1060 when missing).
pub fn parse_sc_query(out: &ProcOut) -> Service {
    let text = out.text();
    if out.code == Some(1060) || text.contains("1060") {
        return Service::Missing;
    }
    let state = text
        .lines()
        .find(|l| l.trim_start().starts_with("STATE"))
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("");
    match state {
        "RUNNING" => Service::Running,
        "STOPPED" => Service::Stopped,
        "" => Service::Other(text),
        s => Service::Other(s.to_string()),
    }
}

/// `START_TYPE : 4   DISABLED` from `sc qc ssh-agent`.
pub fn parse_sc_start_type(out: &str) -> Option<String> {
    out.lines()
        .find(|l| l.trim_start().starts_with("START_TYPE"))
        .and_then(|l| l.split_whitespace().last())
        .map(str::to_string)
}

/// Everything the Authentication diagnostics show.
#[derive(Debug, Clone, Default)]
pub struct AuthInfo {
    /// Effective `credential.helper` list, in the order git asks them.
    pub helpers: Vec<Helper>,
    pub gcm: Option<PathBuf>,
    pub gcm_version: Option<String>,
    pub ssh: SshCommand,
    pub ssh_add: Option<PathBuf>,
    pub agent: Agent,
    pub auth_sock: Option<String>,
    /// Windows only.
    pub service: Option<Service>,
    pub service_start: Option<String>,
}

impl AuthInfo {
    pub fn gcm_configured(&self) -> bool {
        self.helpers.iter().any(|h| is_gcm_helper(&h.value))
    }

    /// GCM is installed but no helper uses it.
    pub fn offer_gcm(&self) -> bool {
        self.gcm.is_some() && !self.gcm_configured()
    }
}

/// Gathers the authentication setup (blocking: a few short processes).
pub fn detect_auth(p: &dyn Probe, windows: bool) -> AuthInfo {
    let exec = p
        .git(&["--exec-path"])
        .filter(ProcOut::ok)
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());
    let git_root = exec.as_deref().and_then(git_root_from_exec_path);
    let helpers = p
        .git(&["config", "--show-scope", "--get-all", "credential.helper"])
        .filter(ProcOut::ok)
        .map(|o| parse_helpers(&o.stdout))
        .unwrap_or_default();
    let gcm = find_gcm(
        p,
        git_root.as_deref(),
        exec.as_deref().map(Path::new),
        windows,
    );
    let gcm_version = gcm.as_ref().and_then(|exe| {
        p.git(&["credential-manager", "--version"])
            .filter(ProcOut::ok)
            .and_then(|o| parse_gcm_version(&o.stdout))
            .or_else(|| {
                p.run(exe, &["--version"])
                    .filter(ProcOut::ok)
                    .and_then(|o| parse_gcm_version(&o.stdout))
            })
    });
    let core = p
        .git(&["config", "--get", "core.sshCommand"])
        .filter(ProcOut::ok)
        .map(|o| o.stdout.trim().to_string());
    let env = |k: &str| p.env(k).filter(|v| !v.trim().is_empty());
    let ssh = resolve_ssh(
        env("GIT_SSH_COMMAND").as_deref(),
        core.as_deref(),
        env("GIT_SSH").as_deref(),
        git_root.as_deref(),
        windows,
    );
    // A sibling ssh-add must exist; a bare one is looked up on PATH.
    let ssh_add = ssh_add_path(&ssh, windows)
        .filter(|a| a.parent().is_none_or(|d| d.as_os_str().is_empty()) || p.is_file(a));
    let agent = match &ssh_add {
        None => Agent::NotChecked,
        Some(a) => p
            .run(a, &["-l"])
            .map(|o| parse_ssh_add(&o))
            .unwrap_or_else(|| Agent::Error(format!("Cannot start {}", a.display()))),
    };
    let (service, service_start) = if windows {
        let sc = Path::new("sc.exe");
        (
            p.run(sc, &["query", "ssh-agent"])
                .map(|o| parse_sc_query(&o)),
            p.run(sc, &["qc", "ssh-agent"])
                .and_then(|o| parse_sc_start_type(&o.stdout)),
        )
    } else {
        (None, None)
    };
    AuthInfo {
        helpers,
        gcm,
        gcm_version,
        ssh,
        ssh_add,
        agent,
        auth_sock: env("SSH_AUTH_SOCK"),
        service,
        service_start,
    }
}

// ---------------------------------------------------------------- connection test

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteKind {
    /// `ssh://[user@]host[:port]/path` or scp-like `user@host:path`.
    Ssh {
        target: String,
        port: Option<u16>,
    },
    Http,
    /// Local paths, `file://`, `git://`...
    Other,
}

/// Classifies a remote URL the way git does (scp-like syntax needs a colon
/// before any slash, and a one-letter "host" is a Windows drive).
pub fn classify_url(url: &str) -> RemoteKind {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return RemoteKind::Http;
    }
    for prefix in ["ssh://", "git+ssh://", "ssh+git://"] {
        if lower.starts_with(prefix) {
            let authority = u[prefix.len()..].split('/').next().unwrap_or("");
            let (target, port) = match authority.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                    (h, p.parse().ok())
                }
                _ => (authority, None),
            };
            let target = target.replace(['[', ']'], "");
            if target.is_empty() || target.ends_with('@') {
                return RemoteKind::Other;
            }
            return RemoteKind::Ssh { target, port };
        }
    }
    if u.contains("://") {
        return RemoteKind::Other;
    }
    match u.split_once(':') {
        Some((before, _))
            if before.chars().count() > 1
                && !before.contains(['/', '\\'])
                && !before.ends_with('@') =>
        {
            RemoteKind::Ssh {
                target: before.to_string(),
                port: None,
            }
        }
        _ => RemoteKind::Other,
    }
}

/// `ssh -T` (or `plink -batch`) arguments that check authentication against
/// an ssh remote without prompting. `None` for non-ssh URLs.
pub fn ssh_test_command(ssh: &SshCommand, kind: &RemoteKind) -> Option<(PathBuf, Vec<String>)> {
    let RemoteKind::Ssh { target, port } = kind else {
        return None;
    };
    let mut args: Vec<String> = ssh.words.iter().skip(1).cloned().collect();
    let s = |v: &str| v.to_string();
    if ssh.flavor == SshFlavor::Plink {
        args.push(s("-batch"));
        if let Some(p) = port {
            args.extend([s("-P"), p.to_string()]);
        }
    } else {
        args.extend([
            s("-T"),
            s("-o"),
            s("BatchMode=yes"),
            s("-o"),
            s("ConnectTimeout=10"),
        ]);
        if let Some(p) = port {
            args.extend([s("-p"), p.to_string()]);
        }
    }
    args.push(target.clone());
    Some((ssh.exe.clone(), args))
}

/// Exit 255 is ssh's own failure; servers answer anything else (GitHub
/// exits 1 with "successfully authenticated, but … no shell access").
pub fn interpret_ssh_test(out: &ProcOut) -> Result<String, String> {
    let text = out.text();
    match out.code {
        None => Err(if text.is_empty() {
            "Timed out.".into()
        } else {
            text
        }),
        Some(255) => Err(if text.is_empty() {
            "ssh failed (exit code 255).".into()
        } else {
            text
        }),
        Some(_) if text.contains("Permission denied") => Err(text),
        Some(_) => Ok(if text.is_empty() {
            "Connected and authenticated.".into()
        } else {
            text
        }),
    }
}

/// Result of `git ls-remote --heads`.
pub fn interpret_ls_remote(out: &ProcOut) -> Result<String, String> {
    if out.ok() {
        let n = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
        Ok(format!("Connected: the remote lists {n} branch(es)."))
    } else {
        let lines: Vec<&str> = out
            .stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        let tail = lines[lines.len().saturating_sub(4)..].join("\n");
        Err(if tail.is_empty() {
            format!("git ls-remote failed (exit code {:?}).", out.code)
        } else {
            tail
        })
    }
}

/// Tests a remote (blocking, at most [`CONNECT_TIMEOUT`]): `ssh -T` with
/// BatchMode for ssh URLs, else `git ls-remote` without any prompt.
pub fn test_connection(
    url: &str,
    ssh: &SshCommand,
    workdir: &Path,
    log: &CommandLog,
) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') {
        return Err("Enter a remote URL.".into());
    }
    let kind = classify_url(url);
    if let Some((exe, args)) = ssh_test_command(ssh, &kind) {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_with_timeout(&exe, &a, &[], CONNECT_TIMEOUT)
            .ok_or_else(|| format!("Cannot start {}.", exe.display()))?;
        return interpret_ssh_test(&out);
    }
    // No askpass: a missing credential fails instead of prompting.
    let git = Git::new(workdir, log.clone()).env("GCM_INTERACTIVE", "never");
    let cancel = CancelToken::new();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let c2 = cancel.clone();
    let timer = std::thread::spawn(move || {
        if rx.recv_timeout(CONNECT_TIMEOUT) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            c2.cancel();
        }
    });
    let r = git.run_streaming(&["ls-remote", "--heads", url], None, &cancel, |_| {});
    drop(tx);
    let _ = timer.join();
    match r {
        Ok(o) => interpret_ls_remote(&ProcOut {
            code: o.code,
            stdout: o.stdout_str(),
            stderr: o.stderr_str(),
        }),
        Err(_) if cancel.is_cancelled() => {
            Err(format!("Timed out after {} s.", CONNECT_TIMEOUT.as_secs()))
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Elevated PowerShell commands that enable and start the agent service.
pub const START_AGENT_SERVICE: &str =
    "Set-Service ssh-agent -StartupType Automatic\nStart-Service ssh-agent";
/// Windows OpenSSH, for `core.sshCommand`.
pub const WINDOWS_SSH: &str = "C:/Windows/System32/OpenSSH/ssh.exe";

// ---------------------------------------------------------------- dialog

const GOOD: Color32 = Color32::from_rgb(0x3F, 0xA3, 0x4D);

/// The Authentication diagnostics dialog (More ▸ Authentication…, and from
/// Optimize ▸ Diagnostics).
pub struct Authentication {
    detect: Option<Task<AuthInfo>>,
    info: Option<AuthInfo>,
    confirm_gcm: bool,
    gcm_task: Option<Task<Result<(), String>>>,
    gcm_result: Option<Result<(), String>>,
    url: String,
    test: Option<Task<Result<String, String>>>,
    test_result: Option<Result<String, String>>,
}

impl Authentication {
    pub fn new(snap: &Snapshot) -> Self {
        let remotes = &snap.refs.remotes;
        let url = remotes
            .iter()
            .find(|r| r.name == "origin")
            .or_else(|| remotes.first())
            .map(|r| r.fetch_url.clone())
            .unwrap_or_default();
        Self {
            detect: None,
            info: None,
            confirm_gcm: false,
            gcm_task: None,
            gcm_result: None,
            url,
            test: None,
            test_result: None,
        }
    }

    fn start_detect(&mut self, ctx: &Context, tab: &RepoTab) {
        let git = tab.session.repo().git().clone();
        self.detect = Some(Task::spawn(ctx, move || {
            detect_auth(&SystemProbe::new(git), cfg!(windows))
        }));
    }

    fn poll(&mut self, ctx: &Context, tab: &RepoTab) {
        if self.info.is_none() && self.detect.is_none() {
            self.start_detect(ctx, tab);
        }
        if let Some(t) = &mut self.detect
            && let Some(i) = t.take()
        {
            self.detect = None;
            self.info = Some(i);
        }
        if let Some(t) = &mut self.gcm_task
            && let Some(r) = t.take()
        {
            self.gcm_task = None;
            let ok = r.is_ok();
            self.gcm_result = Some(r);
            if ok {
                self.start_detect(ctx, tab);
            }
        }
        if let Some(t) = &mut self.test
            && let Some(r) = t.take()
        {
            self.test = None;
            self.test_result = Some(r);
        }
    }
}

/// A path with forward slashes only (joined Windows paths mix both).
fn slashed(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn status_line(ui: &mut egui::Ui, good: Option<bool>, text: impl Into<String>) {
    let text = text.into();
    match good {
        Some(true) => {
            crate::icons::label(ui, crate::icons::Icon::Ok, text, GOOD);
        }
        Some(false) => warning(ui, text),
        None => {
            ui.label(text);
        }
    }
}

impl TabDialog for Authentication {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        shared: &mut Shared,
    ) -> bool {
        self.poll(ctx, tab);
        let mut redetect = false;
        let mut set_gcm: Option<&'static str> = None;
        let mut run_test = false;
        let (out, ()) = modal(
            ctx,
            "authentication",
            "Authentication",
            660.0,
            "OK",
            |ui, ctl| {
                ctl.show_ok = false;
                ctl.cancel_label = "Close";
                let Some(info) = &self.info else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Detecting credential helpers and ssh…");
                    });
                    return;
                };
                // ---- HTTPS
                ui.label(RichText::new("HTTPS credentials").strong());
                egui::Grid::new("auth-https")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("credential.helper:");
                        ui.vertical(|ui| {
                            if info.helpers.is_empty() {
                                warning(ui, "none: git cannot store passwords or tokens");
                            }
                            for h in &info.helpers {
                                let scope = if h.scope.is_empty() {
                                    String::new()
                                } else {
                                    format!("  ({})", h.scope)
                                };
                                ui.label(RichText::new(format!("{}{scope}", h.value)).monospace());
                            }
                        });
                        ui.end_row();
                        ui.label("Git Credential Manager:");
                        ui.vertical(|ui| match &info.gcm {
                            Some(exe) => {
                                let v = info.gcm_version.as_deref().unwrap_or("version unknown");
                                let used = if info.gcm_configured() {
                                    "in use"
                                } else {
                                    "installed, not configured"
                                };
                                status_line(
                                    ui,
                                    Some(info.gcm_configured()),
                                    format!("{v}, {used}"),
                                );
                                hint(ui, slashed(exe));
                            }
                            None => status_line(ui, None, "not found"),
                        });
                        ui.end_row();
                    });
                if let Some(exe) = info.gcm.as_ref().filter(|_| info.offer_gcm()) {
                    let name = gcm_helper_name(exe);
                    if !self.confirm_gcm {
                        if ui
                            .add_enabled(
                                self.gcm_task.is_none(),
                                egui::Button::new(
                                    "Use Git Credential Manager for all repositories…",
                                ),
                            )
                            .clicked()
                        {
                            self.confirm_gcm = true;
                        }
                    } else {
                        egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .inner_margin(6)
                        .show(ui, |ui| {
                            ui.label(format!(
                                "Add credential.helper = {name} to your global git config (~/.gitconfig)? \
                                 Helpers already configured stay and are asked first."
                            ));
                            ui.horizontal(|ui| {
                                if ui.button(RichText::new("Add").strong()).clicked() {
                                    set_gcm = Some(name);
                                    self.confirm_gcm = false;
                                }
                                if ui.button("Cancel").clicked() {
                                    self.confirm_gcm = false;
                                }
                            });
                        });
                    }
                } else if info.gcm.is_none() {
                    hint(
                        ui,
                        "Git Credential Manager (installed with Git for Windows) stores HTTPS credentials and tokens securely and handles OAuth sign-in.",
                    );
                }
                if self.gcm_task.is_some() {
                    ui.spinner();
                }
                match &self.gcm_result {
                    Some(Ok(())) => {
                        crate::icons::label(
                            ui,
                            crate::icons::Icon::Ok,
                            "Git Credential Manager configured.",
                            GOOD,
                        );
                    }
                    Some(Err(e)) => error(ui, e.clone()),
                    None => {}
                }
                // ---- SSH
                ui.add_space(6.0);
                ui.separator();
                ui.label(RichText::new("SSH").strong());
                let ssh = &info.ssh;
                egui::Grid::new("auth-ssh")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("ssh used by git:");
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&ssh.command).monospace());
                            hint(
                                ui,
                                format!("{} · from {}", ssh.flavor.label(), ssh.source.label()),
                            );
                        });
                        ui.end_row();
                        ui.label("Agent:");
                        ui.vertical(|ui| {
                            match &info.agent {
                                Agent::Keys(k) => {
                                    status_line(
                                        ui,
                                        Some(true),
                                        format!("reachable, {} key(s) loaded", k.len()),
                                    );
                                }
                                Agent::NoKeys => {
                                    status_line(ui, Some(false), "reachable, but no keys loaded")
                                }
                                Agent::Unreachable(m) => {
                                    status_line(ui, Some(false), "not reachable");
                                    if !m.is_empty() {
                                        hint(ui, m.clone());
                                    }
                                }
                                Agent::Error(m) => error(ui, m.clone()),
                                Agent::NotChecked if ssh.flavor == SshFlavor::Plink => {
                                    ui.label(
                                        RichText::new(
                                            "not checked (plink takes keys from Pageant)",
                                        )
                                        .weak(),
                                    );
                                }
                                Agent::NotChecked => {
                                    ui.label(
                                        RichText::new("not checked (no ssh-add next to this ssh)")
                                            .weak(),
                                    );
                                }
                            }
                            if let Some(a) = &info.ssh_add {
                                hint(ui, format!("{} -l", slashed(a)));
                            }
                        });
                        ui.end_row();
                        if ssh.flavor != SshFlavor::WindowsOpenSsh && ssh.flavor != SshFlavor::Plink
                        {
                            ui.label("SSH_AUTH_SOCK:");
                            ui.label(
                                RichText::new(info.auth_sock.as_deref().unwrap_or("not set"))
                                    .monospace(),
                            );
                            ui.end_row();
                        }
                        if let Some(s) = &info.service {
                            ui.label("ssh-agent service:");
                            let start = info
                                .service_start
                                .as_deref()
                                .map(|t| format!(" (start type {})", t.to_ascii_lowercase()))
                                .unwrap_or_default();
                            match s {
                                Service::Running => status_line(ui, Some(true), "running"),
                                Service::Stopped => {
                                    status_line(ui, None, format!("stopped{start}"))
                                }
                                Service::Missing => status_line(ui, None, "not installed"),
                                Service::Other(t) => status_line(ui, None, t.clone()),
                            }
                            ui.end_row();
                        }
                    });
                if let Agent::Keys(keys) = &info.agent {
                    for k in keys {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.label(RichText::new(&k.fingerprint).monospace().small());
                            ui.label(
                                RichText::new(format!("{} {} {}", k.kind, k.bits, k.comment))
                                    .weak()
                                    .small(),
                            );
                        });
                    }
                }
                let no_agent = matches!(info.agent, Agent::Unreachable(_));
                match ssh.flavor {
                    SshFlavor::GitForWindows if no_agent => {
                        hint(
                            ui,
                            "Git's bundled OpenSSH finds its agent through SSH_AUTH_SOCK (start-ssh-agent). \
                         To use the Windows ssh-agent service instead, set core.sshCommand to Windows OpenSSH (Settings ▸ SSH):",
                        );
                        super::repo_tools::command_box(
                            ui,
                            &format!("git config --global core.sshCommand {WINDOWS_SSH}"),
                        );
                    }
                    SshFlavor::WindowsOpenSsh
                        if no_agent && info.service.as_ref() != Some(&Service::Running) =>
                    {
                        hint(ui, "Start the agent service (PowerShell as administrator):");
                        super::repo_tools::command_box(ui, START_AGENT_SERVICE);
                    }
                    _ => {}
                }
                if info.agent == Agent::NoKeys {
                    hint(ui, "Load a key into the agent:");
                    super::repo_tools::command_box(ui, "ssh-add ~/.ssh/id_ed25519");
                }
                // ---- test
                ui.add_space(6.0);
                ui.separator();
                ui.label(RichText::new("Test connection").strong());
                ui.horizontal(|ui| {
                let remotes = &snap.refs.remotes;
                if !remotes.is_empty() {
                    egui::ComboBox::from_id_salt("auth-remote")
                        .selected_text("Remote")
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for r in remotes {
                                if ui.selectable_label(r.fetch_url == self.url, &r.name).clicked() {
                                    self.url = r.fetch_url.clone();
                                    self.test_result = None;
                                }
                            }
                        });
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.url)
                        .desired_width(380.0)
                        .hint_text("git@github.com:user/repo.git"),
                );
                if r.changed() {
                    self.test_result = None;
                }
                let busy = self.test.is_some();
                if ui
                    .add_enabled(!busy && !self.url.trim().is_empty(), egui::Button::new("Test connection"))
                    .on_hover_text("ssh remotes: ssh -T with BatchMode; others: git ls-remote (no prompts)")
                    .clicked()
                {
                    run_test = true;
                }
                if busy {
                    ui.spinner();
                }
            });
                let what = match classify_url(&self.url) {
                    RemoteKind::Ssh { target, .. } => format!("ssh -T {target}"),
                    RemoteKind::Http | RemoteKind::Other => "git ls-remote".to_string(),
                };
                hint(
                    ui,
                    format!("Runs {what} (at most {} s).", CONNECT_TIMEOUT.as_secs()),
                );
                match &self.test_result {
                    Some(Ok(m)) => {
                        crate::icons::label(ui, crate::icons::Icon::Ok, m.to_string(), GOOD);
                    }
                    Some(Err(e)) => {
                        error(ui, e.clone());
                        if e.contains("Host key verification failed") {
                            hint(
                                ui,
                                "The host key is not known yet: fetch once from GitKwaken to review and accept it.",
                            );
                        }
                    }
                    None => {}
                }
                ui.add_space(6.0);
                if ui
                    .add_enabled(self.detect.is_none(), egui::Button::new("Detect again"))
                    .clicked()
                {
                    redetect = true;
                }
            },
        );
        if redetect {
            self.start_detect(ctx, tab);
        }
        if let Some(name) = set_gcm {
            let log = shared.log.clone();
            self.gcm_result = None;
            self.gcm_task = Some(Task::spawn(ctx, move || {
                Git::new(std::env::temp_dir(), log)
                    .run(&["config", "--global", "--add", "credential.helper", name])
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}"))
            }));
        }
        if run_test && let Some(info) = &self.info {
            let (url, ssh, wd, log) = (
                self.url.clone(),
                info.ssh.clone(),
                tab.path.clone(),
                shared.log.clone(),
            );
            self.test_result = None;
            self.test = Some(Task::spawn(ctx, move || {
                test_connection(&url, &ssh, &wd, &log)
            }));
        }
        out == Outcome::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_writes_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("gk-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let s = measure(&repo, &repo.join(".git"), &CommandLog::default()).unwrap();
        assert!(s.per_object_ms > 0.0 && s.total_ms >= s.per_object_ms);
        let left: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("repo")],
            "scratch removed"
        );
        let cmd = defender_commands(Path::new("C:/r"), Path::new("C:/Git/cmd/git.exe"));
        assert!(cmd.starts_with("Add-MpPreference -ExclusionPath '"));
        assert!(cmd.contains("-ExclusionProcess 'C:/Git/cmd/git.exe'"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------------------------------------------ auth fakes

    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        /// "git <args>" or "<program> <args>" → output.
        cmds: HashMap<String, ProcOut>,
        files: HashSet<PathBuf>,
        env: HashMap<String, String>,
    }

    fn out(code: i32, stdout: &str, stderr: &str) -> ProcOut {
        ProcOut {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    impl Fake {
        fn cmd(mut self, line: &str, o: ProcOut) -> Self {
            self.cmds.insert(line.to_string(), o);
            self
        }
        fn file(mut self, p: &str) -> Self {
            self.files.insert(PathBuf::from(p));
            self
        }
        fn env(mut self, k: &str, v: &str) -> Self {
            self.env.insert(k.into(), v.into());
            self
        }
    }

    impl Probe for Fake {
        fn git(&self, args: &[&str]) -> Option<ProcOut> {
            Some(
                self.cmds
                    .get(&format!("git {}", args.join(" ")))
                    .cloned()
                    .unwrap_or_else(|| out(1, "", "")),
            )
        }
        fn run(&self, program: &Path, args: &[&str]) -> Option<ProcOut> {
            let p = program.to_string_lossy().replace('\\', "/");
            self.cmds.get(&format!("{p} {}", args.join(" "))).cloned()
        }
        fn is_file(&self, path: &Path) -> bool {
            let p = PathBuf::from(path.to_string_lossy().replace('\\', "/"));
            self.files.contains(&p)
        }
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }
    }

    const EXEC: &str = "C:/Program Files/Git/mingw64/libexec/git-core\n";
    const GCM: &str = "C:/Program Files/Git/mingw64/bin/git-credential-manager.exe";
    #[cfg(windows)]
    const GIT_SSH_ADD: &str = "C:/Program Files/Git/usr/bin/ssh-add.exe";
    #[cfg(windows)]
    const WIN_SSH_ADD: &str = "C:/Windows/System32/OpenSSH/ssh-add.exe";

    #[test]
    fn helper_parsing_and_gcm_names() {
        let h =
            parse_helpers("system\tmanager\nglobal\t\nglobal\tstore\nlocal\tcache --timeout=60\n");
        assert_eq!(
            h,
            [
                Helper {
                    scope: "global".into(),
                    value: "store".into()
                },
                Helper {
                    scope: "local".into(),
                    value: "cache --timeout=60".into()
                }
            ],
            "an empty value resets the list"
        );
        assert_eq!(parse_helpers("manager\n")[0].scope, "");
        for yes in [
            "manager",
            "manager-core",
            " Manager ",
            "/usr/local/share/gcm-core/git-credential-manager",
            r#""C:\\Program Files\\Git\\mingw64\\bin\\git-credential-manager.exe""#,
            "!git-credential-manager-core get",
        ] {
            assert!(is_gcm_helper(yes), "{yes}");
        }
        for no in [
            "store",
            "wincred",
            "cache --timeout=3600",
            "osxkeychain",
            "managers",
        ] {
            assert!(!is_gcm_helper(no), "{no}");
        }
        assert_eq!(
            parse_gcm_version("2.5.0+d34930736e13\n").as_deref(),
            Some("2.5.0")
        );
        assert_eq!(
            parse_gcm_version("\n 2.0.886 \n").as_deref(),
            Some("2.0.886")
        );
        assert_eq!(parse_gcm_version("fatal: not a git command"), None);
        assert_eq!(gcm_helper_name(Path::new(GCM)), "manager");
        assert_eq!(
            gcm_helper_name(Path::new(
                "C:/Git/mingw64/libexec/git-core/git-credential-manager-core.exe"
            )),
            "manager-core"
        );
        assert_eq!(
            git_root_from_exec_path(EXEC),
            Some(PathBuf::from("C:/Program Files/Git"))
        );
        assert_eq!(
            git_root_from_exec_path(r"D:\PortableGit\mingw64\libexec\git-core\"),
            Some(PathBuf::from("D:/PortableGit"))
        );
        assert_eq!(git_root_from_exec_path("/usr/lib/git-core"), None);
    }

    #[test]
    #[cfg(windows)] // Windows install paths
    fn gcm_found_but_not_configured() {
        let p = Fake::default()
            .cmd("git --exec-path", out(0, EXEC, ""))
            .cmd(
                "git config --show-scope --get-all credential.helper",
                out(0, "global\twincred\n", ""),
            )
            .cmd(
                "git credential-manager --version",
                out(0, "2.5.0+abc\n", ""),
            )
            .file(GCM);
        let i = detect_auth(&p, true);
        assert_eq!(i.gcm.as_deref(), Some(Path::new(GCM)));
        assert_eq!(i.gcm_version.as_deref(), Some("2.5.0"));
        assert!(!i.gcm_configured());
        assert!(i.offer_gcm());

        // Configured in the system config: no offer.
        let p = p.cmd(
            "git config --show-scope --get-all credential.helper",
            out(0, "system\tmanager\n", ""),
        );
        let i = detect_auth(&p, true);
        assert!(i.gcm_configured() && !i.offer_gcm());

        // Not installed (GCM only on PATH counts too; here nowhere).
        let p = Fake::default().cmd("git --exec-path", out(0, EXEC, ""));
        let i = detect_auth(&p, true);
        assert!(i.gcm.is_none() && !i.offer_gcm() && i.helpers.is_empty());
    }

    #[test]
    fn gcm_on_path() {
        // Found on PATH (Linux layout); version from the executable itself.
        let p = Fake::default()
            .cmd("git --exec-path", out(0, "/usr/lib/git-core\n", ""))
            .env("PATH", "/usr/bin")
            .file("/usr/bin/git-credential-manager")
            .cmd(
                "/usr/bin/git-credential-manager --version",
                out(0, "2.6.1\n", ""),
            );
        let i = detect_auth(&p, false);
        assert_eq!(
            i.gcm.as_deref(),
            Some(Path::new("/usr/bin/git-credential-manager"))
        );
        assert_eq!(i.gcm_version.as_deref(), Some("2.6.1"));
    }

    #[test]
    #[cfg(windows)] // Windows install paths
    fn ssh_resolution_order_and_flavor() {
        let root = Path::new("C:/Program Files/Git");
        let d = resolve_ssh(None, None, None, Some(root), true);
        assert_eq!(d.source, SshSource::Default);
        assert_eq!(d.flavor, SshFlavor::GitForWindows);
        assert_eq!(d.exe, PathBuf::from("C:/Program Files/Git/usr/bin/ssh.exe"));

        let c = resolve_ssh(
            None,
            Some("C:/Windows/System32/OpenSSH/ssh.exe"),
            None,
            Some(root),
            true,
        );
        assert_eq!(c.source, SshSource::CoreSshCommand);
        assert_eq!(c.flavor, SshFlavor::WindowsOpenSsh);

        // GIT_SSH_COMMAND wins over core.sshCommand, which wins over GIT_SSH.
        let e = resolve_ssh(
            Some("ssh -i ~/.ssh/work"),
            Some(WINDOWS_SSH),
            Some("plink"),
            Some(root),
            true,
        );
        assert_eq!(e.source, SshSource::GitSshCommandEnv);
        assert_eq!(e.words, ["ssh", "-i", "~/.ssh/work"]);
        assert_eq!(e.flavor, SshFlavor::GitForWindows, "bare ssh is Git's own");
        assert_eq!(e.exe, PathBuf::from("C:/Program Files/Git/usr/bin/ssh.exe"));
        let g = resolve_ssh(
            Some("  "),
            None,
            Some(r"C:\Program Files\PuTTY\plink.exe"),
            Some(root),
            true,
        );
        assert_eq!(g.source, SshSource::GitSshEnv);
        assert_eq!(g.flavor, SshFlavor::Plink);
        assert_eq!(ssh_add_path(&g, true), None);

        let q = resolve_ssh(
            None,
            Some(r#""C:\Tools\My SSH\ssh.exe" -v"#),
            None,
            Some(root),
            true,
        );
        assert_eq!(q.words, [r"C:\Tools\My SSH\ssh.exe", "-v"]);
        assert_eq!(q.flavor, SshFlavor::Other);
        assert_eq!(
            ssh_add_path(&q, true),
            Some(PathBuf::from(r"C:\Tools\My SSH").join("ssh-add.exe"))
        );
        let l = resolve_ssh(None, None, None, None, false);
        assert_eq!((l.command.as_str(), l.flavor), ("ssh", SshFlavor::Other));
        assert_eq!(ssh_add_path(&l, false), Some(PathBuf::from("ssh-add")));
        assert_eq!(split_command(r#"a '' "b c"  d"#), ["a", "", "b c", "d"]);
    }

    #[test]
    fn ssh_add_and_service_parsing() {
        let keys = parse_ssh_add(&out(
            0,
            "256 SHA256:AbCdEf me@laptop (ED25519)\n3072 SHA256:XyZ  (RSA)\n",
            "",
        ));
        let Agent::Keys(k) = keys else {
            panic!("{keys:?}")
        };
        assert_eq!(
            k[0],
            SshKey {
                bits: "256".into(),
                fingerprint: "SHA256:AbCdEf".into(),
                comment: "me@laptop".into(),
                kind: "ED25519".into()
            }
        );
        assert_eq!((k[1].comment.as_str(), k[1].kind.as_str()), ("", "RSA"));
        assert_eq!(
            parse_ssh_add(&out(1, "The agent has no identities.\n", "")),
            Agent::NoKeys
        );
        assert!(matches!(
            parse_ssh_add(&out(2, "", "Could not open a connection to your authentication agent.")),
            Agent::Unreachable(m) if m.contains("authentication agent")
        ));
        assert!(matches!(
            parse_ssh_add(&out(255, "", "boom")),
            Agent::Error(_)
        ));

        let q = "SERVICE_NAME: ssh-agent\n        TYPE               : 10  WIN32_OWN_PROCESS\n        STATE              : 1  STOPPED\n";
        assert_eq!(parse_sc_query(&out(1, q, "")), Service::Stopped);
        assert_eq!(
            parse_sc_query(&out(0, &q.replace("1  STOPPED", "4  RUNNING"), "")),
            Service::Running
        );
        assert_eq!(
            parse_sc_query(&out(
                1060,
                "[SC] EnumQueryServicesStatus:OpenService FAILED 1060:\n\nThe specified service does not exist as an installed service.\n",
                ""
            )),
            Service::Missing
        );
        assert_eq!(
            parse_sc_start_type("        START_TYPE         : 4   DISABLED\n").as_deref(),
            Some("DISABLED")
        );
    }

    #[test]
    #[cfg(windows)] // Windows install paths
    fn detect_agent_for_each_ssh() {
        // Default: Git's OpenSSH, no SSH_AUTH_SOCK agent.
        let p = Fake::default()
            .cmd("git --exec-path", out(0, EXEC, ""))
            .file(GIT_SSH_ADD)
            .cmd(
                &format!("{GIT_SSH_ADD} -l"),
                out(
                    2,
                    "",
                    "Could not open a connection to your authentication agent.",
                ),
            )
            .cmd(
                "sc.exe query ssh-agent",
                out(0, "        STATE              : 4  RUNNING\n", ""),
            )
            .cmd(
                "sc.exe qc ssh-agent",
                out(0, "        START_TYPE         : 2   AUTO_START\n", ""),
            );
        let i = detect_auth(&p, true);
        assert_eq!(i.ssh.flavor, SshFlavor::GitForWindows);
        assert_eq!(i.ssh_add.as_deref(), Some(Path::new(GIT_SSH_ADD)));
        assert!(matches!(i.agent, Agent::Unreachable(_)));
        assert_eq!(i.service, Some(Service::Running));
        assert_eq!(i.service_start.as_deref(), Some("AUTO_START"));

        // core.sshCommand → Windows OpenSSH: its ssh-add talks to the service.
        let p = p
            .cmd(
                "git config --get core.sshCommand",
                out(0, &format!("{WINDOWS_SSH}\n"), ""),
            )
            .file(WIN_SSH_ADD)
            .cmd(
                &format!("{WIN_SSH_ADD} -l"),
                out(0, "256 SHA256:k1 me (ED25519)\n", ""),
            );
        let i = detect_auth(&p, true);
        assert_eq!(i.ssh.flavor, SshFlavor::WindowsOpenSsh);
        assert!(matches!(&i.agent, Agent::Keys(k) if k.len() == 1));

        // A custom ssh without ssh-add next to it: not checked.
        let p = Fake::default()
            .cmd("git --exec-path", out(0, EXEC, ""))
            .env("GIT_SSH_COMMAND", "C:/tools/ssh.exe");
        let i = detect_auth(&p, true);
        assert_eq!(i.ssh.source, SshSource::GitSshCommandEnv);
        assert_eq!(i.ssh_add, None);
        assert_eq!(i.agent, Agent::NotChecked);
    }

    #[test]
    fn remote_urls_and_connection_checks() {
        let ssh = |t: &str, port: Option<u16>| RemoteKind::Ssh {
            target: t.into(),
            port,
        };
        assert_eq!(
            classify_url("git@github.com:me/repo.git"),
            ssh("git@github.com", None)
        );
        assert_eq!(
            classify_url("ssh://git@host.example:2222/x.git"),
            ssh("git@host.example", Some(2222))
        );
        assert_eq!(classify_url("ssh://host/x"), ssh("host", None));
        assert_eq!(
            classify_url("git+ssh://u@[::1]:22/x"),
            ssh("u@::1", Some(22))
        );
        assert_eq!(
            classify_url("https://github.com/me/repo.git"),
            RemoteKind::Http
        );
        assert_eq!(classify_url(r"C:\repos\x"), RemoteKind::Other);
        assert_eq!(classify_url("C:/repos/x"), RemoteKind::Other);
        assert_eq!(classify_url("../x"), RemoteKind::Other);
        assert_eq!(classify_url("./a:b"), RemoteKind::Other);
        assert_eq!(classify_url("file:///c/x"), RemoteKind::Other);

        let root = Path::new("C:/Program Files/Git");
        let s = resolve_ssh(None, Some("ssh -i key"), None, Some(root), true);
        let (exe, args) = ssh_test_command(&s, &ssh("git@h", Some(2222))).unwrap();
        if cfg!(windows) {
            assert_eq!(exe, PathBuf::from("C:/Program Files/Git/usr/bin/ssh.exe"));
        }
        assert_eq!(
            args,
            [
                "-i",
                "key",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "git@h"
            ]
        );
        let p = resolve_ssh(None, None, Some("C:/PuTTY/plink.exe"), Some(root), true);
        let (_, args) = ssh_test_command(&p, &ssh("git@h", None)).unwrap();
        assert_eq!(args, ["-batch", "git@h"]);
        assert!(ssh_test_command(&s, &RemoteKind::Http).is_none());

        let gh =
            "Hi me! You've successfully authenticated, but GitHub does not provide shell access.";
        assert_eq!(interpret_ssh_test(&out(1, "", gh)), Ok(gh.to_string()));
        assert!(
            interpret_ssh_test(&out(
                255,
                "",
                "git@github.com: Permission denied (publickey)."
            ))
            .is_err()
        );
        assert!(interpret_ssh_test(&out(255, "", "Host key verification failed.")).is_err());
        assert!(interpret_ssh_test(&ProcOut::default()).is_err(), "timeout");
        assert_eq!(
            interpret_ls_remote(&out(0, "a\trefs/heads/main\nb\trefs/heads/dev\n", "")),
            Ok("Connected: the remote lists 2 branch(es).".into())
        );
        assert_eq!(
            interpret_ls_remote(&out(
                128,
                "",
                "remote: Not found\nfatal: repository not found\n"
            )),
            Err("remote: Not found\nfatal: repository not found".into())
        );
    }

    #[test]
    fn ls_remote_test_against_local_bare_repo_and_timeout_runner() {
        let root = std::env::temp_dir().join(format!("gk-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let log = CommandLog::default();
        let git = Git::new(&root, log.clone());
        git.run(&["init", "-q", "--bare", "remote.git"]).unwrap();
        let url = root.join("remote.git").to_string_lossy().replace('\\', "/");
        let ssh = SshCommand::default();
        let r = test_connection(&url, &ssh, &root, &log);
        assert_eq!(r, Ok("Connected: the remote lists 0 branch(es).".into()));
        assert!(
            test_connection(&root.join("missing").to_string_lossy(), &ssh, &root, &log).is_err()
        );
        assert!(test_connection("--upload-pack=x", &ssh, &root, &log).is_err());
        let _ = std::fs::remove_dir_all(&root);
        // The runner reports exit codes and kills on timeout.
        let exe = gk_cli::find_git();
        let o = run_with_timeout(&exe, &["--version"], &[], Duration::from_secs(20)).unwrap();
        assert!(o.ok() && o.stdout.starts_with("git version"), "{o:?}");
        assert!(
            run_with_timeout(Path::new("gk-no-such-program"), &[], &[], PROBE_TIMEOUT).is_none()
        );
    }
}
