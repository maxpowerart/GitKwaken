//! Worker pools by priority.
//!
//! - [`foreground`]: a small dedicated pool for what is on screen right now
//!   (row metadata, the selected diff, commit details). It never runs long
//!   jobs, so a request starts within milliseconds.
//! - background: rayon's global pool, for searches, prefetching, compares and
//!   other bulk work ([`background`]).
//!
//! A long search therefore cannot delay visible rows: they do not share a
//! queue.

use std::sync::OnceLock;

/// Threads of the foreground pool: enough for metas + diff + details at once.
pub const FOREGROUND_THREADS: usize = 3;

/// The high-priority pool for visible-row work.
pub fn foreground() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(FOREGROUND_THREADS)
            .thread_name(|i| format!("gk-fg-{i}"))
            // Visible work is shallow; small stacks keep the footprint low.
            .stack_size(1 << 20)
            .build()
            .expect("foreground pool")
    })
}

/// Runs `f` on the background (global rayon) pool.
pub fn background(f: impl FnOnce() + Send + 'static) {
    rayon::spawn(f);
}

/// Caps rayon's global (background) pool at `max_threads` (default: one per
/// logical CPU). Call once at startup, before any other gk-core call; later
/// calls, or calls after the pool started, return false and change nothing.
pub fn init_background(max_threads: usize) -> bool {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(max_threads.max(1));
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .thread_name(|i| format!("gk-bg-{i}"))
        .build_global()
        .is_ok()
}

/// Which pool a [`crate::session`] load runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Priority {
    Visible,
    Background,
}

pub(crate) fn spawn(p: Priority, f: impl FnOnce() + Send + 'static) {
    match p {
        Priority::Visible => foreground().spawn(f),
        Priority::Background => background(f),
    }
}
