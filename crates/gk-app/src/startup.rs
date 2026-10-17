//! Startup trace for measuring the "first frame from cache" budget end to end.
//!
//! With `GITKWAKEN_STARTUP_LOG=<file>` set, each milestone is appended once as
//! `<label> <ms> <unix ms>` (ms since `main()`, or the measured duration for
//! [`record`]); the unix time lets a harness add the
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
    let Some(start) = START.get() else { return };
    write(label, start.elapsed(), last);
}

/// Records a measured duration (e.g. diff request to rendered) under `label`,
/// once per label; does not end the trace.
pub fn record(label: &str, took: std::time::Duration) {
    write(label, took, false);
}

fn write(label: &str, took: std::time::Duration, last: bool) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let Some(Some(path)) = LOG.get() else {
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
    let ms = took.as_secs_f64() * 1000.0;
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
