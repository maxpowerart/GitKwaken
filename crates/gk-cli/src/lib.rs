//! Runs the system `git` executable: blocking calls, streaming calls with
//! progress, cancellation, and a global log of every executed command.

mod job;
mod log;

pub use log::{CommandLog, LogEntry};

use anyhow::{Context as _, Result, anyhow};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Minimum git version the client relies on (pathspec-from-file, fsmonitor, reftable).
pub const MIN_VERSION: (u32, u32) = (2, 45);

/// Output of a finished git process.
#[derive(Debug, Clone)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Converts a failed run into an error carrying git's own message.
    pub fn check(self) -> Result<Self> {
        if self.success() {
            Ok(self)
        } else {
            let msg = self.stderr_str();
            let msg = msg.trim();
            Err(anyhow!(if msg.is_empty() {
                format!("git exited with {:?}", self.code)
            } else {
                msg.to_string()
            }))
        }
    }
}

/// Shared flag used to cancel a running command.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A line of streamed output.
#[derive(Debug, Clone)]
pub enum Line {
    Stdout(String),
    /// Stderr; git writes progress here, using `\r` for in-place updates.
    Stderr(String),
}

/// Handle to the git executable bound to one working directory.
#[derive(Clone)]
pub struct Git {
    exe: PathBuf,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    log: CommandLog,
}

impl Git {
    pub fn new(cwd: impl Into<PathBuf>, log: CommandLog) -> Self {
        Self { exe: find_git(), cwd: cwd.into(), env: Vec::new(), log }
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn log(&self) -> &CommandLog {
        &self.log
    }

    pub fn with_cwd(&self, cwd: impl Into<PathBuf>) -> Self {
        let mut g = self.clone();
        g.cwd = cwd.into();
        g
    }

    /// Adds an environment variable to every command spawned by this handle.
    pub fn env(&self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let mut g = self.clone();
        g.env.push((key.into(), value.into()));
        g
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        // Never let git block on a terminal prompt or take optional locks
        // that would race with the user's other tools.
        cmd.arg("--no-optional-locks")
            .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
            .args(args)
            .current_dir(&self.cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("GIT_PAGER", "cat")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        hide_window(&mut cmd);
        cmd
    }

    /// Runs git to completion and returns its output, without checking the exit code.
    pub fn run_raw(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<Output> {
        let started = Instant::now();
        let mut cmd = self.command(args);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to start git ({})", self.exe.display()))?;
        let writer = stdin.map(|data| {
            let mut pipe = child.stdin.take().expect("stdin piped");
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })
        });
        let out = child.wait_with_output()?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        let output = Output { code: out.status.code(), stdout: out.stdout, stderr: out.stderr };
        self.log.push(&self.cwd, args, started.elapsed(), &output);
        Ok(output)
    }

    /// Runs git and fails with git's stderr on a non-zero exit code.
    pub fn run(&self, args: &[&str]) -> Result<Output> {
        self.run_raw(args, None)?.check()
    }

    /// Runs git feeding `stdin`, failing on non-zero exit.
    pub fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<Output> {
        self.run_raw(args, Some(stdin))?.check()
    }

    /// Runs git and returns trimmed stdout as a string.
    pub fn read(&self, args: &[&str]) -> Result<String> {
        Ok(self.run(args)?.stdout_str().trim_end().to_string())
    }

    /// Runs git, streaming stdout/stderr lines to `on_line` as they arrive.
    /// Kills the whole process tree when `cancel` fires.
    pub fn run_streaming(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        cancel: &CancelToken,
        mut on_line: impl FnMut(Line) + Send,
    ) -> Result<Output> {
        let started = Instant::now();
        let mut cmd = self.command(args);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to start git ({})", self.exe.display()))?;
        let job = job::Job::assign(&child);
        if let Some(data) = stdin {
            let mut pipe = child.stdin.take().expect("stdin piped");
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            });
        }

        let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
        let readers = spawn_readers(&mut child, tx);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut pending_out = Vec::new();
        let mut pending_err = Vec::new();
        let mut cancelled = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((is_err, chunk)) => {
                    if is_err {
                        stderr.extend_from_slice(&chunk);
                        pending_err.extend_from_slice(&chunk);
                        drain_lines(&mut pending_err, |l| on_line(Line::Stderr(l)));
                    } else {
                        stdout.extend_from_slice(&chunk);
                        pending_out.extend_from_slice(&chunk);
                        drain_lines(&mut pending_out, |l| on_line(Line::Stdout(l)));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !cancelled && cancel.is_cancelled() {
                cancelled = true;
                match &job {
                    Some(j) => j.terminate(),
                    None => {
                        let _ = child.kill();
                    }
                }
            }
        }
        for r in readers {
            let _ = r.join();
        }
        if !pending_out.is_empty() {
            on_line(Line::Stdout(String::from_utf8_lossy(&pending_out).into_owned()));
        }
        if !pending_err.is_empty() {
            on_line(Line::Stderr(String::from_utf8_lossy(&pending_err).into_owned()));
        }
        let status = child.wait()?;
        let output = Output { code: status.code(), stdout, stderr };
        self.log.push(&self.cwd, args, started.elapsed(), &output);
        if cancelled {
            return Err(anyhow!("cancelled"));
        }
        Ok(output)
    }

    /// Parses `git --version`.
    pub fn version(&self) -> Result<(u32, u32, u32)> {
        let out = self.with_cwd(std::env::temp_dir()).read(&["--version"])?;
        parse_version(&out).ok_or_else(|| anyhow!("unrecognized git version: {out}"))
    }
}

fn spawn_readers(
    child: &mut Child,
    tx: std::sync::mpsc::Sender<(bool, Vec<u8>)>,
) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 || tx.send((false, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(mut err) = child.stderr.take() {
        handles.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 || tx.send((true, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }));
    }
    handles
}

/// Splits complete lines (terminated by `\n` or `\r`) off the front of `buf`.
fn drain_lines(buf: &mut Vec<u8>, mut emit: impl FnMut(String)) {
    let mut start = 0;
    for i in 0..buf.len() {
        if buf[i] == b'\n' || buf[i] == b'\r' {
            if i > start {
                emit(String::from_utf8_lossy(&buf[start..i]).into_owned());
            }
            start = i + 1;
        }
    }
    buf.drain(..start);
}

pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let v = s.trim().strip_prefix("git version ")?;
    let mut it = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Locates the git executable: `GITKWAKEN_GIT`, then PATH, then common install dirs.
pub fn find_git() -> PathBuf {
    if let Some(p) = std::env::var_os("GITKWAKEN_GIT") {
        return PathBuf::from(p);
    }
    let exe = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(exe);
            if cand.is_file() {
                return cand;
            }
        }
    }
    if cfg!(windows) {
        for base in [
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramFiles(x86)"),
            std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Programs").into()),
        ]
        .into_iter()
        .flatten()
        {
            let cand = PathBuf::from(base).join("Git").join("cmd").join("git.exe");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from(exe)
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

/// NUL-joins paths for `--pathspec-from-file=- --pathspec-file-nul`, which
/// sidesteps the 32k command-line limit on Windows.
pub fn nul_paths<I, S>(paths: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut out = Vec::new();
    for p in paths {
        out.extend_from_slice(p.as_ref());
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("git version 2.45.1.windows.1"), Some((2, 45, 1)));
        assert_eq!(parse_version("git version 2.39.3 (Apple Git-146)"), Some((2, 39, 3)));
        assert_eq!(parse_version("nope"), None);
    }

    #[test]
    fn line_splitting_handles_cr_progress() {
        let mut buf = b"Receiving objects:  10%\rReceiving objects:  20%\rdone\npartial".to_vec();
        let mut lines = Vec::new();
        drain_lines(&mut buf, |l| lines.push(l));
        assert_eq!(lines, ["Receiving objects:  10%", "Receiving objects:  20%", "done"]);
        assert_eq!(buf, b"partial");
    }

    #[test]
    fn runs_git_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let log = CommandLog::default();
        let git = Git::new(dir.path(), log.clone());
        git.run(&["init", "-q"]).unwrap();
        let out = git.read(&["rev-parse", "--is-inside-work-tree"]).unwrap();
        assert_eq!(out, "true");
        assert_eq!(log.entries().len(), 2);
        assert!(git.run(&["rev-parse", "HEAD"]).is_err());
    }

    #[test]
    fn streaming_reports_lines() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new(dir.path(), CommandLog::default());
        let mut lines = Vec::new();
        let out = git
            .run_streaming(&["--version"], None, &CancelToken::new(), |l| lines.push(l))
            .unwrap();
        assert!(out.success());
        assert!(matches!(&lines[0], Line::Stdout(s) if s.starts_with("git version")));
    }
}
