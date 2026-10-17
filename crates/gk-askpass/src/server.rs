//! GUI side: accepts helper connections and queues their prompts for the UI.

use crate::protocol::{ct_eq, read_request, write_reply};
use crate::{ENV_VAR, Prompt, PromptKind};
use anyhow::{Result, bail};
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Unanswered prompts are cancelled after this long.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Cap on concurrently handled helper connections.
const MAX_CONNECTIONS: usize = 32;
/// How often a waiting handler checks for timeout / helper death / shutdown.
const POLL: Duration = Duration::from_millis(200);

type Repaint = Box<dyn Fn() + Send + Sync>;

struct Slot {
    prompt: Prompt,
    tx: mpsc::Sender<Option<String>>,
}

struct Inner {
    token: String,
    repaint: Repaint,
    timeout: Duration,
    queue: Mutex<VecDeque<Slot>>,
    /// Exact prompt text -> remembered answer. Memory only, never logged.
    cache: Mutex<HashMap<String, String>>,
    next_id: AtomicU64,
    connections: AtomicUsize,
    shutdown: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Askpass endpoint living in the GUI process. Dropping it stops the
/// listener and cancels every waiting prompt.
pub struct Server {
    inner: Arc<Inner>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    /// Binds `127.0.0.1:0`; `repaint` is called whenever the queue changes
    /// because of a helper (prompt arrived, timed out or helper died).
    pub fn start(repaint: Repaint) -> Result<Self> {
        Self::start_with_timeout(repaint, PROMPT_TIMEOUT)
    }

    fn start_with_timeout(repaint: Repaint, timeout: Duration) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
        let token = raw.iter().map(|b| format!("{b:02x}")).collect();
        let inner = Arc::new(Inner {
            token,
            repaint,
            timeout,
            queue: Mutex::default(),
            cache: Mutex::default(),
            next_id: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
        });
        let accept = std::thread::Builder::new()
            .name("askpass-accept".into())
            .spawn({
                let inner = inner.clone();
                move || accept_loop(&inner, listener)
            })?;
        Ok(Self {
            inner,
            addr,
            accept: Some(accept),
        })
    }

    /// Env vars to set on every git command that may prompt. `helper_exe` is
    /// the GUI executable (whose `main()` calls [`crate::client_main`]).
    pub fn env(&self, helper_exe: &Path) -> Vec<(String, String)> {
        let exe = helper_exe.to_string_lossy().into_owned();
        vec![
            ("GIT_ASKPASS".into(), exe.clone()),
            ("SSH_ASKPASS".into(), exe),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            ("DISPLAY".into(), ":0".into()),
            (
                ENV_VAR.into(),
                format!("{}:{}", self.addr.port(), self.inner.token),
            ),
        ]
    }

    /// Oldest unanswered prompt, if any. Non-blocking.
    pub fn pending(&self) -> Option<Prompt> {
        lock(&self.inner.queue).front().map(|s| s.prompt.clone())
    }

    /// Answers prompt `id`; `None` cancels. Unknown / expired ids are ignored.
    pub fn answer(&self, id: u64, reply: Option<String>) {
        let slot = {
            let mut q = lock(&self.inner.queue);
            q.iter()
                .position(|s| s.prompt.id == id)
                .and_then(|i| q.remove(i))
        };
        if let Some(slot) = slot {
            let _ = slot.tx.send(reply);
        }
    }

    /// Auto-answers future prompts with exactly this text (in memory only,
    /// for credential prompts; host-key questions are never cached).
    pub fn remember(&self, prompt_text: &str, reply: &str) {
        lock(&self.inner.cache).insert(prompt_text.to_owned(), reply.to_owned());
    }

    /// Drops a remembered answer, e.g. after git reports an auth failure.
    pub fn forget(&self, prompt_text: &str) {
        lock(&self.inner.cache).remove(prompt_text);
    }

    pub fn forget_all(&self) {
        lock(&self.inner.cache).clear();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        // Wake the blocking accept().
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        lock(&self.inner.cache).clear();
    }
}

fn accept_loop(inner: &Arc<Inner>, listener: TcpListener) {
    for conn in listener.incoming() {
        if inner.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        if inner.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            inner.connections.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let spawned = std::thread::Builder::new()
            .name("askpass-conn".into())
            .spawn({
                let inner = inner.clone();
                move || {
                    let _ = handle(&inner, stream);
                    inner.connections.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            inner.connections.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn handle(inner: &Inner, mut stream: TcpStream) -> Result<()> {
    if !stream.peer_addr()?.ip().is_loopback() {
        bail!("askpass connection from non-loopback address");
    }
    // Requests are sent in one go; a slow sender is not a helper.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let req = read_request(&mut stream)?;
    if !ct_eq(req.token.as_bytes(), inner.token.as_bytes()) {
        let _ = write_reply(&mut stream, None);
        bail!("askpass token mismatch");
    }
    let kind = PromptKind::detect(&req.prompt, &req.hint);
    if kind != PromptKind::YesNo {
        let cached = lock(&inner.cache).get(&req.prompt).cloned();
        if let Some(reply) = cached {
            return write_reply(&mut stream, Some(&reply));
        }
    }

    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    lock(&inner.queue).push_back(Slot {
        prompt: Prompt {
            id,
            text: req.prompt,
            kind,
        },
        tx,
    });
    (inner.repaint)();

    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + inner.timeout;
    let reply = loop {
        match rx.recv_timeout(POLL) {
            Ok(reply) => break reply,
            Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline
                    || inner.shutdown.load(Ordering::SeqCst)
                    || helper_gone(&stream)
                {
                    break None;
                }
            }
        }
    };
    // Still queued means timeout / helper death / shutdown: withdraw it.
    let withdrawn = {
        let mut q = lock(&inner.queue);
        let pos = q.iter().position(|s| s.prompt.id == id);
        pos.and_then(|i| q.remove(i)).is_some()
    };
    if withdrawn {
        (inner.repaint)();
    }
    stream.set_nonblocking(false)?;
    write_reply(&mut stream, reply.as_deref())
}

/// True once the helper closed or reset the connection (git/ssh was killed).
fn helper_gone(stream: &TcpStream) -> bool {
    let mut b = [0u8; 1];
    match stream.peek(&mut b) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != ErrorKind::WouldBlock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Request, read_reply, write_request};
    use std::sync::atomic::AtomicUsize;

    fn port_token(s: &Server) -> (u16, String) {
        let env = s.env(Path::new("x.exe"));
        let v = &env.iter().find(|(k, _)| k == ENV_VAR).unwrap().1;
        let (p, t) = v.split_once(':').unwrap();
        (p.parse().unwrap(), t.to_owned())
    }

    fn ask(port: u16, token: &str, prompt: &str) -> Result<Option<String>> {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        let req = Request {
            token: token.into(),
            prompt: prompt.into(),
            hint: String::new(),
        };
        write_request(&mut s, &req)?;
        read_reply(&mut s)
    }

    fn wait_pending(s: &Server) -> Prompt {
        let t = Instant::now();
        loop {
            if let Some(p) = s.pending() {
                return p;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "no prompt arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn env_contents() {
        let s = Server::start(Box::new(|| {})).unwrap();
        let env = s.env(Path::new(r"C:\Program Files\GitKwaken\gitkwaken.exe"));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            get("GIT_ASKPASS"),
            Some(r"C:\Program Files\GitKwaken\gitkwaken.exe")
        );
        assert_eq!(get("SSH_ASKPASS"), get("GIT_ASKPASS"));
        assert_eq!(get("SSH_ASKPASS_REQUIRE"), Some("force"));
        assert_eq!(get("DISPLAY"), Some(":0"));
        let (_, token) = port_token(&s);
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn answer_and_cancel() {
        let repaints = Arc::new(AtomicUsize::new(0));
        let r = repaints.clone();
        let s = Server::start(Box::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        let (port, token) = port_token(&s);
        std::thread::scope(|sc| {
            let h = sc.spawn(|| ask(port, &token, "Username for 'https://github.com': "));
            let p = wait_pending(&s);
            assert_eq!(p.kind, PromptKind::Username);
            s.answer(p.id, Some("alice".into()));
            assert_eq!(h.join().unwrap().unwrap().as_deref(), Some("alice"));

            let h = sc.spawn(|| ask(port, &token, "Password for 'https://alice@github.com': "));
            let p = wait_pending(&s);
            assert_eq!(p.kind, PromptKind::Password);
            s.answer(p.id, None);
            assert_eq!(h.join().unwrap().unwrap(), None);
        });
        assert!(s.pending().is_none());
        assert!(repaints.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn rejects_bad_token() {
        let s = Server::start(Box::new(|| {})).unwrap();
        let (port, token) = port_token(&s);
        let mut wrong = token.clone().into_bytes();
        wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
        let wrong = String::from_utf8(wrong).unwrap();
        for bad in [wrong.as_str(), "", &token[..63]] {
            assert_eq!(ask(port, bad, "Password: ").unwrap(), None);
        }
        assert!(s.pending().is_none(), "rejected request must not be queued");
    }

    #[test]
    fn times_out_and_withdraws() {
        let s = Server::start_with_timeout(Box::new(|| {}), Duration::from_millis(300)).unwrap();
        let (port, token) = port_token(&s);
        assert_eq!(ask(port, &token, "Password: ").unwrap(), None);
        assert!(s.pending().is_none());
    }

    #[test]
    fn helper_death_withdraws_prompt() {
        let s = Server::start(Box::new(|| {})).unwrap();
        let (port, token) = port_token(&s);
        {
            let mut c = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            let req = Request {
                token,
                prompt: "Password: ".into(),
                hint: String::new(),
            };
            write_request(&mut c, &req).unwrap();
            wait_pending(&s);
        } // connection dropped, like a killed git
        let t = Instant::now();
        while s.pending().is_some() {
            assert!(t.elapsed() < Duration::from_secs(5), "prompt not withdrawn");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn remembered_answers() {
        let s = Server::start(Box::new(|| {})).unwrap();
        let (port, token) = port_token(&s);
        let text = "Password for 'https://alice@example.com': ";
        s.remember(text, "pw");
        assert_eq!(ask(port, &token, text).unwrap().as_deref(), Some("pw"));
        // Host-key questions always go to the user.
        let yn = "Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        s.remember(yn, "yes");
        std::thread::scope(|sc| {
            let h = sc.spawn(|| ask(port, &token, yn));
            let p = wait_pending(&s);
            assert_eq!(p.kind, PromptKind::YesNo);
            s.answer(p.id, None);
            assert_eq!(h.join().unwrap().unwrap(), None);
        });
        s.forget(text);
        std::thread::scope(|sc| {
            let h = sc.spawn(|| ask(port, &token, text));
            let p = wait_pending(&s);
            s.answer(p.id, Some("new".into()));
            assert_eq!(h.join().unwrap().unwrap().as_deref(), Some("new"));
        });
    }

    #[test]
    fn drop_cancels_waiting() {
        let s = Server::start(Box::new(|| {})).unwrap();
        let (port, token) = port_token(&s);
        let h = std::thread::spawn(move || ask(port, &token, "Password: "));
        wait_pending(&s);
        drop(s);
        assert_eq!(h.join().unwrap().unwrap(), None);
    }
}
