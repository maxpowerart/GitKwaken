//! `gk-bench status-mem <dir> [entries]`: heap held by a status snapshot.
//!
//! A counting global allocator tracks net heap bytes and blocks; the
//! difference before and after building a [`gk_status::Status`] is what the
//! snapshot retains. Two measurements: a real full scan of `<dir>` (gix
//! engine), and a synthetic status with `entries` (default 1M) paths shaped
//! like the scan's, built the way the engines build theirs.

use anyhow::{Context, Result};
use gk_cli::{CommandLog, Git};
use gk_status::{Engine, Kind, Options, Status, StatusBuilder, Untracked, compute};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

/// System allocator that counts net heap bytes and blocks while [`ON`] is
/// set; other benchmarks pay one uncontended relaxed load per call.
pub struct Counting;

static ON: AtomicBool = AtomicBool::new(false);
/// Net bytes / blocks allocated while counting (wrapping: frees of older
/// blocks can take them "below zero"; only differences are meaningful).
static BYTES: AtomicUsize = AtomicUsize::new(0);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize, blocks: usize) {
    if ON.load(Ordering::Relaxed) {
        BYTES.fetch_add(n, Ordering::Relaxed);
        BLOCKS.fetch_add(blocks, Ordering::Relaxed);
    }
}

fn sub(n: usize, blocks: usize) {
    if ON.load(Ordering::Relaxed) {
        BYTES.fetch_sub(n, Ordering::Relaxed);
        BLOCKS.fetch_sub(blocks, Ordering::Relaxed);
    }
}

// SAFETY: forwards to `System`, only adding accounting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        add(l.size(), 1);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        sub(l.size(), 1);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        add(l.size(), 1);
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        add(new, 0);
        sub(l.size(), 0);
        unsafe { System.realloc(p, l, new) }
    }
}

#[derive(Clone, Copy)]
struct Mark(usize, usize);

fn mark() -> Mark {
    Mark(
        BYTES.load(Ordering::Relaxed),
        BLOCKS.load(Ordering::Relaxed),
    )
}

/// (bytes, blocks) allocated since `m` and still live.
fn since(m: Mark) -> (usize, usize) {
    let now = mark();
    (now.0.wrapping_sub(m.0), now.1.wrapping_sub(m.1))
}

/// Requested bytes plus ~16 bytes of allocator header / rounding per block
/// (Windows heap and glibc malloc are both in that range).
fn report(label: &str, s: &Status, (bytes, blocks): (usize, usize)) {
    let n = (s.staged.len() + s.unstaged.len()).max(1);
    let est = bytes + blocks * 16;
    println!(
        "{label:<14} {n:>8} entries  {:>6.1} MB requested in {blocks:>8} blocks  ~{:>6.1} MB with overhead  ({:>5.1} B/entry)",
        bytes as f64 / 1e6,
        est as f64 / 1e6,
        est as f64 / n as f64,
    );
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

pub fn run(args: &[String]) -> Result<()> {
    let dir = Path::new(args.first().context("dir")?);
    let target: usize = args
        .get(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1_000_000);
    ON.store(true, Ordering::SeqCst);
    let git = Git::new(dir, CommandLog::default());
    let stop = Arc::new(AtomicBool::new(false));
    println!(
        "size_of::<Entry>() = {} bytes",
        std::mem::size_of::<gk_status::Entry>()
    );

    for engine in [Engine::Gix, Engine::Cli] {
        let opts = Options {
            engine,
            untracked: Untracked::All,
        };
        let mut times = Vec::new();
        let mut scanned = Status::default();
        let mut retained = (0, 0);
        for _ in 0..5 {
            drop(std::mem::take(&mut scanned));
            let m = mark();
            let t = Instant::now();
            scanned = compute(dir, &git, &[], &opts, &stop)?;
            times.push(ms(t));
            retained = since(m);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "full scan ({engine:?}): median {:.0} ms, best {:.0} ms of 5",
            times[2], times[0]
        );
        report("  retained", &scanned, retained);
    }

    // Synthetic: the scan's paths, repeated under distinct prefixes.
    let scanned = compute(
        dir,
        &git,
        &[],
        &Options {
            engine: Engine::Gix,
            untracked: Untracked::All,
        },
        &stop,
    )?;
    let sample: Vec<&[u8]> = scanned
        .staged
        .iter()
        .chain(&scanned.unstaged)
        .map(|e| e.path.as_bytes())
        .collect();
    anyhow::ensure!(!sample.is_empty(), "the scan found no changes to replicate");
    let m = mark();
    let t = Instant::now();
    let mut b = StatusBuilder::new();
    let mut round = 0;
    'fill: loop {
        let prefix = format!("r{round}/");
        for p in &sample {
            if b.len() == target {
                break 'fill;
            }
            b.push_parts(false, &[prefix.as_bytes(), p], Kind::Untracked, None);
        }
        round += 1;
    }
    let mut s = b.finish();
    let build = ms(t);
    report("synthetic", &s, since(m));
    println!("build: {build:.1} ms");

    let t = Instant::now();
    let copy = s.clone();
    println!("clone: {:.1} ms", ms(t));
    let t = Instant::now();
    drop(copy);
    println!("drop: {:.1} ms", ms(t));
    let t = Instant::now();
    s.sort();
    println!("sort + dedup: {:.1} ms", ms(t));
    Ok(())
}
