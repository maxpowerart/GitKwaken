//! Startup and latency budgets measured through a real [`gk_core::Session`].
//!
//! `gk-bench open <repo> [runs]`          Session::open -> first history rows, cold and from the cache
//! `gk-bench history-first <repo> [runs]` tips + partial (first screen) and full history build
//! `gk-bench save-latency <repo> [runs]`  file write -> status snapshot through the watcher

use anyhow::{Context, Result, bail};
use gk_cli::CommandLog;
use gk_core::cache::RepoCache;
use gk_core::history;
use gk_core::{Session, SessionOptions, Snapshot};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub fn run(cmd: &str, args: &[String]) -> Result<()> {
    let dir = PathBuf::from(args.first().context("repository dir")?);
    let runs: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(5);
    match cmd {
        "open" => bench_open(&dir, runs),
        "history-first" => bench_history_first(&dir, runs),
        "save-latency" => bench_save(&dir, runs),
        _ => bail!("unknown startup bench {cmd}"),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn report(label: &str, mut v: Vec<Duration>) {
    v.sort();
    let med = v[v.len() / 2];
    println!(
        "{label:<46} best {:>8.1} ms  median {:>8.1} ms  worst {:>8.1} ms",
        ms(v[0]),
        ms(med),
        ms(*v.last().unwrap())
    );
}

/// Polls the session until `cond` holds; returns the time since `t0`.
fn until(
    ss: &Session,
    t0: Instant,
    secs: u64,
    mut cond: impl FnMut(&Snapshot) -> bool,
) -> Result<Duration> {
    let deadline = t0 + Duration::from_secs(secs);
    loop {
        if cond(&ss.snapshot()) {
            return Ok(t0.elapsed());
        }
        if Instant::now() > deadline {
            bail!("timed out");
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn fully_loaded(s: &Snapshot) -> bool {
    !s.history.is_empty()
        && !s.history_loading
        && !s.history_partial
        && s.status_gen > 0
        && !s.status_loading
        && !s.from_cache
        && s.refs.head != gk_core::Head::Unknown
}

fn bench_open(dir: &Path, runs: usize) -> Result<()> {
    let t = Instant::now();
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    println!(
        "repository: {}  (Repo::open {:.1} ms)",
        dir.display(),
        ms(t.elapsed())
    );

    // Cold: no cache.
    let (mut first, mut refs, mut full) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..runs {
        let opts = SessionOptions {
            watch: false,
            cache: false,
            ..Default::default()
        };
        let t0 = Instant::now();
        let ss = Session::open(repo.clone(), opts, || {});
        first.push(until(&ss, t0, 60, |s| !s.history.is_empty())?);
        refs.push(until(&ss, t0, 60, |s| {
            s.refs.head != gk_core::Head::Unknown
        })?);
        full.push(until(&ss, t0, 120, fully_loaded)?);
    }
    report("cold: open -> first history rows", first);
    report("cold: open -> refs", refs);
    report("cold: open -> everything loaded", full);

    // Warm: populate a private cache file, then reopen.
    let cache_dir = std::env::temp_dir().join(format!("gk-bench-cache-{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir)?;
    let file = cache_dir.join("cache.redb");
    let opts = SessionOptions {
        watch: false,
        cache: true,
        cache_file: Some(file.clone()),
        ..Default::default()
    };
    {
        let ss = Session::open(repo.clone(), opts.clone(), || {});
        until(&ss, Instant::now(), 120, fully_loaded)?;
    }
    let rc = RepoCache::open(Some(&file), repo.workdir()).context("cache")?;
    let t = Instant::now();
    while rc.load().history.is_none() {
        if t.elapsed() > Duration::from_secs(30) {
            bail!("cache was not written");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = Instant::now();
    let c = rc.load();
    println!(
        "cache read + decode {:.1} ms ({} commits)",
        ms(t.elapsed()),
        c.history.as_ref().map(|h| h.history.len()).unwrap_or(0)
    );
    drop(c);
    let (mut first, mut snap, mut full) = (Vec::new(), Vec::new(), Vec::new());
    let mut cached_first = 0;
    for _ in 0..runs {
        let t0 = Instant::now();
        let ss = Session::open(repo.clone(), opts.clone(), || {});
        let mut from_cache = false;
        first.push(until(&ss, t0, 60, |s| {
            from_cache = s.from_cache;
            !s.history.is_empty()
        })?);
        cached_first += from_cache as usize;
        snap.push(until(&ss, t0, 60, |s| {
            !s.history.is_empty() && s.status_gen > 0 && s.refs.head != gk_core::Head::Unknown
        })?);
        full.push(until(&ss, t0, 120, fully_loaded)?);
    }
    report("warm cache: open -> first history rows", first);
    report("warm cache: open -> refs+status+history", snap);
    report("warm cache: open -> validated (live)", full);
    println!("first rows came from the cache in {cached_first}/{runs} runs");
    drop(rc);
    let _ = std::fs::remove_dir_all(&cache_dir);
    Ok(())
}

fn bench_history_first(dir: &Path, runs: usize) -> Result<()> {
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let (mut tips_t, mut partial_t, mut full_t) = (Vec::new(), Vec::new(), Vec::new());
    let mut rows = 0;
    for _ in 0..runs {
        let t0 = Instant::now();
        let tips = history::tip_ids(&repo, &history::Scope::All);
        tips_t.push(t0.elapsed());
        let mut partial = None;
        let h = history::build_with_partial(
            &repo,
            &tips,
            &AtomicBool::new(false),
            history::PARTIAL_ROWS,
            &mut |_| partial = Some(t0.elapsed()),
        )?;
        full_t.push(t0.elapsed());
        partial_t.push(partial.unwrap_or(t0.elapsed()));
        rows = h.len();
    }
    println!(
        "{rows} commits, first screen = {} rows",
        history::PARTIAL_ROWS
    );
    report("tip_ids (in-process refs)", tips_t);
    report("tips + partial history (first screen)", partial_t);
    report("tips + full history", full_t);
    Ok(())
}

fn bench_save(dir: &Path, runs: usize) -> Result<()> {
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let opts = SessionOptions {
        watch: true,
        cache: false,
        ..Default::default()
    };
    let ss = Session::open(repo.clone(), opts, || {});
    until(&ss, Instant::now(), 300, fully_loaded)?;
    std::thread::sleep(Duration::from_millis(800));
    let (mut add, mut remove) = (Vec::new(), Vec::new());
    for i in 0..runs {
        let name = format!("gk-bench-save-{i}.tmp");
        let path = repo.workdir().join(&name);
        let t0 = Instant::now();
        std::fs::write(&path, b"saved\n")?;
        add.push(until(&ss, t0, 30, |s| {
            s.status.unstaged.iter().any(|e| e.path == name)
        })?);
        // Stay below the watcher's busy threshold.
        std::thread::sleep(Duration::from_millis(600));
        let t0 = Instant::now();
        std::fs::remove_file(&path)?;
        remove.push(until(&ss, t0, 30, |s| {
            !s.status.unstaged.iter().any(|e| e.path == name)
        })?);
        std::thread::sleep(Duration::from_millis(600));
    }
    println!("repository: {}", dir.display());
    report("save (new file) -> status snapshot", add);
    report("delete -> status snapshot", remove);
    Ok(())
}
