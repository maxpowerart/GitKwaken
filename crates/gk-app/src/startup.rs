//! Startup trace for measuring the "first frame from cache" budget end to end.
//!
//! With `GITKWAKEN_STARTUP_LOG=<file>` set, each milestone is appended once as
//! `<label> <ms since main()> <unix ms>`; the unix time lets a harness add the
//! time from process creation to `main()`. Without the variable nothing runs
//! beyond one environment lookup.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<Instant> = OnceLock::new();
static LOG: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
/// Set once every milestone is written, so callers can skip their checks.
static DONE: AtomicBool = AtomicBool::new(false);

/// Called first thing in `main`.
pub fn init() {
    START.get_or_init(Instant::now);
    let log = LOG.get_or_init(|| std::env::var_os("GITKWAKEN_STARTUP_LOG").map(Into::into));
    if log.is_none() {
        DONE.store(true, Ordering::Relaxed);
    }
}

/// True while milestones are still being recorded.
pub fn active() -> bool {
    !DONE.load(Ordering::Relaxed)
}

/// Records `label` (once per label). `last` marks the final milestone.
pub fn mark(label: &str, last: bool) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let (Some(start), Some(Some(path))) = (START.get(), LOG.get()) else {
        return;
    };
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.iter().any(|s| s == label) {
        return;
    }
    seen.push(label.to_string());
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{label} {ms:.1} {unix}");
    }
    if last {
        DONE.store(true, Ordering::Relaxed);
    }
}
