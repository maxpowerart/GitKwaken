//! Object reads through one long-lived `git cat-file` process.
//!
//! [`CatFile`] keeps `git cat-file --batch-command` running with its stdin
//! and stdout piped: each request is one line (`contents <name>` or
//! `info <name>`), each reply is git's `--batch` / `--batch-check` format,
//! `<oid> <type> <size>\n<content>\n`, or `<name> missing\n`. Content is
//! read as exactly `<size>` raw bytes, so binary data (NUL, CR LF) and large
//! objects pass through unchanged. A process that died (killed, crashed, a
//! desynchronized stream) is replaced transparently on the next request; the
//! process is killed on drop. [`CatFilePool`] shares a few of them between
//! threads.
//!
//! `--batch-command` (git 2.36+) is `--batch` and `--batch-check` in one
//! process; both reply formats are identical to the separate modes.

use crate::{Git, Output};
use anyhow::{Result, anyhow, bail};
use parking_lot::{Condvar, Mutex};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::Arc;
use std::time::Instant;

const ARGS: [&str; 2] = ["cat-file", "--batch-command"];
/// Bytes of the process's stderr kept for error messages.
const STDERR_TAIL: usize = 4096;

/// Type of a git object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl ObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
            ObjectKind::Commit => "commit",
            ObjectKind::Tag => "tag",
        }
    }

    fn parse(s: &[u8]) -> Option<Self> {
        Some(match s {
            b"blob" => ObjectKind::Blob,
            b"tree" => ObjectKind::Tree,
            b"commit" => ObjectKind::Commit,
            b"tag" => ObjectKind::Tag,
            _ => return None,
        })
    }
}

/// `--batch-check` reply: object id (hex), type and size in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub oid: String,
    pub kind: ObjectKind,
    pub size: u64,
}

/// A read object. `data` is `None` when the object was larger than the
/// limit passed to [`CatFile::read_limited`] (its content was skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub info: ObjectInfo,
    pub data: Option<Vec<u8>>,
}

enum Reply {
    Found(Object),
    Missing,
    Ambiguous,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    started: Instant,
    served: u64,
}

impl Proc {
    fn spawn(git: &Git) -> Result<Proc> {
        let mut cmd = git.command(&ARGS);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to start git cat-file ({}): {e}", git.exe.display()))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = BufReader::with_capacity(64 * 1024, child.stdout.take().expect("piped"));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        // Drained continuously so git never blocks on a full stderr pipe.
        let reader = child.stderr.take().map(|mut err| {
            let tail = stderr.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut t = tail.lock();
                    t.extend_from_slice(&buf[..n]);
                    if t.len() > STDERR_TAIL {
                        let cut = t.len() - STDERR_TAIL;
                        t.drain(..cut);
                    }
                }
            })
        });
        Ok(Proc {
            child,
            stdin,
            stdout,
            stderr,
            reader,
            started: Instant::now(),
            served: 0,
        })
    }

    fn exchange(&mut self, request: &[u8], data: bool, limit: u64) -> std::io::Result<Reply> {
        use std::io::{Error, ErrorKind};
        self.stdin.write_all(request)?;
        self.stdin.flush()?;
        let mut header = Vec::with_capacity(96);
        self.stdout.read_until(b'\n', &mut header)?;
        if header.pop() != Some(b'\n') {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "git cat-file exited unexpectedly",
            ));
        }
        self.served += 1;
        let Some(info) = parse_header(&header) else {
            if header.ends_with(b" missing") {
                return Ok(Reply::Missing);
            }
            if header.ends_with(b" ambiguous") {
                return Ok(Reply::Ambiguous);
            }
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "unexpected git cat-file reply: {}",
                    String::from_utf8_lossy(&header)
                ),
            ));
        };
        if !data {
            return Ok(Reply::Found(Object { info, data: None }));
        }
        let content = if info.size <= limit {
            let mut buf = Vec::with_capacity(usize::try_from(info.size).unwrap_or(0));
            (&mut self.stdout).take(info.size).read_to_end(&mut buf)?;
            if buf.len() as u64 != info.size {
                return Err(Error::new(ErrorKind::UnexpectedEof, "object truncated"));
            }
            Some(buf)
        } else {
            let skipped = std::io::copy(
                &mut (&mut self.stdout).take(info.size),
                &mut std::io::sink(),
            )?;
            if skipped != info.size {
                return Err(Error::new(ErrorKind::UnexpectedEof, "object truncated"));
            }
            None
        };
        let mut lf = [0u8; 1];
        self.stdout.read_exact(&mut lf)?;
        if lf[0] != b'\n' {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "missing object terminator",
            ));
        }
        Ok(Reply::Found(Object {
            info,
            data: content,
        }))
    }

    fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock())
            .trim()
            .to_string()
    }

    /// Kills the process and records its lifetime in the command log.
    fn stop(mut self, git: &Git) {
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        let out = Output {
            code: status.and_then(|s| s.code()),
            stdout: format!("{} objects served\n", self.served).into_bytes(),
            stderr: self.stderr.lock().clone(),
        };
        git.log.push(&git.cwd, &ARGS, self.started.elapsed(), &out);
    }
}

/// `<oid> <type> <size>` with a hex id; `None` for any other line.
fn parse_header(line: &[u8]) -> Option<ObjectInfo> {
    let mut parts = line.rsplitn(3, |b| *b == b' ');
    let size = parts.next()?;
    let kind = ObjectKind::parse(parts.next()?)?;
    let oid = parts.next()?;
    if !(oid.len() == 40 || oid.len() == 64) || !oid.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    if size.is_empty() || !size.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(ObjectInfo {
        oid: String::from_utf8(oid.to_vec()).ok()?,
        kind,
        size: std::str::from_utf8(size).ok()?.parse().ok()?,
    })
}

/// A long-lived `git cat-file --batch-command` process, started on the first
/// request and restarted when it died. Not `Sync`: share it through a
/// [`CatFilePool`] (or a mutex).
pub struct CatFile {
    git: Git,
    proc: Option<Proc>,
    starts: u64,
}

impl CatFile {
    /// A reader for the repository at `git`'s working directory (same
    /// executable, environment and window flags as `git`'s other commands).
    /// No process runs until the first request.
    pub fn new(git: &Git) -> Self {
        Self {
            git: git.clone(),
            proc: None,
            starts: 0,
        }
    }

    /// Whether a process is currently attached.
    pub fn is_running(&self) -> bool {
        self.proc.is_some()
    }

    /// Processes started so far (1 after the first request, more after restarts).
    pub fn starts(&self) -> u64 {
        self.starts
    }

    /// OS process id of the running process.
    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(|p| p.child.id())
    }

    /// Kills the process without detaching it, as if it had crashed: the next
    /// request notices and starts a new one. For diagnostics and tests.
    pub fn kill(&mut self) {
        if let Some(p) = &mut self.proc {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }

    /// Type and size of `name` (an object id or any revision, e.g.
    /// `HEAD:path`); `None` when it does not exist.
    pub fn info(&mut self, name: &str) -> Result<Option<ObjectInfo>> {
        Ok(self.request("info", name, 0)?.map(|o| o.info))
    }

    /// Type and full content of `name`; `None` when it does not exist.
    pub fn read(&mut self, name: &str) -> Result<Option<(ObjectKind, Vec<u8>)>> {
        Ok(self
            .request("contents", name, u64::MAX)?
            .map(|o| (o.info.kind, o.data.unwrap_or_default())))
    }

    /// Like [`read`](Self::read) with id and size, skipping (not buffering)
    /// the content of objects larger than `limit` bytes.
    pub fn read_limited(&mut self, name: &str, limit: u64) -> Result<Option<Object>> {
        self.request("contents", name, limit)
    }

    fn request(&mut self, cmd: &str, name: &str, limit: u64) -> Result<Option<Object>> {
        if name.is_empty() || name.contains(['\n', '\r', '\0']) {
            bail!("invalid object name {name:?}");
        }
        let line = format!("{cmd} {name}\n");
        let data = cmd == "contents";
        // One retry: a process that died since the last request is replaced.
        for attempt in 0..2 {
            if self.proc.is_none() {
                self.proc = Some(Proc::spawn(&self.git)?);
                self.starts += 1;
            }
            let proc = self.proc.as_mut().expect("started");
            match proc.exchange(line.as_bytes(), data, limit) {
                Ok(Reply::Found(o)) => return Ok(Some(o)),
                Ok(Reply::Missing) => return Ok(None),
                Ok(Reply::Ambiguous) => bail!("ambiguous object name {name:?}"),
                Err(e) => {
                    // The stream state is unknown: never reuse this process.
                    let proc = self.proc.take().expect("started");
                    let tail = proc.stderr_tail();
                    proc.stop(&self.git);
                    if attempt == 1 {
                        return Err(if tail.is_empty() {
                            anyhow!("git cat-file: {e}")
                        } else {
                            anyhow!("git cat-file: {e}: {tail}")
                        });
                    }
                }
            }
        }
        unreachable!("loop returns")
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        if let Some(p) = self.proc.take() {
            p.stop(&self.git);
        }
    }
}

struct PoolState {
    idle: Vec<CatFile>,
    leased: usize,
    starts: u64,
}

/// Thread-safe set of up to `max` [`CatFile`] processes, started on demand
/// and reused; callers beyond `max` wait for a free one.
pub struct CatFilePool {
    git: Git,
    max: usize,
    state: Mutex<PoolState>,
    freed: Condvar,
}

impl CatFilePool {
    pub fn new(git: Git, max: usize) -> Self {
        Self {
            git,
            max: max.max(1),
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                leased: 0,
                starts: 0,
            }),
            freed: Condvar::new(),
        }
    }

    /// Runs `f` with exclusive use of one reader.
    pub fn with<R>(&self, f: impl FnOnce(&mut CatFile) -> R) -> R {
        let cf = {
            let mut st = self.state.lock();
            loop {
                if let Some(cf) = st.idle.pop() {
                    st.leased += 1;
                    break cf;
                }
                if st.leased < self.max {
                    st.leased += 1;
                    break CatFile::new(&self.git);
                }
                self.freed.wait(&mut st);
            }
        };
        let mut lease = Lease {
            pool: self,
            starts_before: cf.starts,
            cf: Some(cf),
        };
        f(lease.cf.as_mut().expect("leased"))
    }

    pub fn info(&self, name: &str) -> Result<Option<ObjectInfo>> {
        self.with(|c| c.info(name))
    }

    pub fn read(&self, name: &str) -> Result<Option<(ObjectKind, Vec<u8>)>> {
        self.with(|c| c.read(name))
    }

    pub fn read_limited(&self, name: &str, limit: u64) -> Result<Option<Object>> {
        self.with(|c| c.read_limited(name, limit))
    }

    /// Processes started over the pool's lifetime.
    pub fn starts(&self) -> u64 {
        self.state.lock().starts
    }

    /// Idle readers with a live process.
    pub fn running(&self) -> usize {
        self.state
            .lock()
            .idle
            .iter()
            .filter(|c| c.is_running())
            .count()
    }
}

/// Returns a leased reader to its pool, also when the closure panicked.
struct Lease<'a> {
    pool: &'a CatFilePool,
    starts_before: u64,
    cf: Option<CatFile>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let cf = self.cf.take().expect("leased");
        let mut st = self.pool.state.lock();
        st.starts += cf.starts - self.starts_before;
        st.leased -= 1;
        if !std::thread::panicking() {
            st.idle.push(cf);
        }
        drop(st);
        self.pool.freed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parsing() {
        let oid = "a".repeat(40);
        assert_eq!(
            parse_header(format!("{oid} blob 12").as_bytes()),
            Some(ObjectInfo {
                oid: oid.clone(),
                kind: ObjectKind::Blob,
                size: 12
            })
        );
        assert!(parse_header(format!("{} commit 0", "b".repeat(64)).as_bytes()).is_some());
        assert!(parse_header(b"HEAD:a missing").is_none());
        assert!(parse_header(format!("{oid} blob").as_bytes()).is_none());
        assert!(parse_header(format!("{oid} blobby 1").as_bytes()).is_none());
        assert!(parse_header(b"zzzz blob 1").is_none());
    }
}
