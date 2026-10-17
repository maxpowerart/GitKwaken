//! Benchmark harness.
//!
//! `gk-bench gen <dir> <files>`     create a synthetic repository
//! `gk-bench status <dir> [runs]`   time status engines against `git status`
//! `gk-bench regress [opts]`        regression gate against bench/baseline-<os>.json (see regress.rs)
//! `gk-bench gen-dag <dir> <n>`     synthetic n-commit DAG via git fast-import
//! `gk-bench stage/filter/diff/file-history …`  budget measurements (see budgets.rs)

mod budgets;
mod regress;
mod startup;

use anyhow::{Context, Result, bail};
use gk_cli::{CommandLog, Git};
use gk_status::{Engine, Options, Untracked, compute};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            let n: usize = args.get(3).context("files")?.parse()?;
            generate(&dir, n)
        }
        Some("status") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            let runs: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(3);
            bench_status(&dir, runs)
        }
        Some("serve") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            let port: u16 = args.get(3).context("port")?.parse()?;
            serve(&dir, port)
        }
        Some("regress") => regress::run(&args[2..]),
        Some("gen-dag") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            let n: usize = args.get(3).context("commits")?.parse()?;
            regress::gen_dag(&dir, n)
        }
        Some(cmd @ ("stage" | "filter" | "diff" | "file-history")) => budgets::run(cmd, &args[2..]),
        // Startup / latency budgets (see startup.rs).
        Some(cmd @ ("open" | "history-first" | "save-latency")) => startup::run(cmd, &args[2..]),
        Some("history") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            bench_history(&dir)
        }
        _ => bail!(
            "usage: gk-bench gen <dir> <files> | status <dir> [runs] | history <dir> | regress [opts] | gen-dag <dir> <n> | stage <repo> [n] | filter [n] | diff [lines] | file-history <repo> [paths]"
        ),
    }
}

/// Minimal static file server on 127.0.0.1 (GET only), used by the
/// self-update end-to-end test.
fn serve(dir: &Path, port: u16) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    println!("serving {} on http://127.0.0.1:{port}/", dir.display());
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let mut line = String::new();
        let mut reader = BufReader::new(s.try_clone()?);
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        // drain headers
        let mut h = String::new();
        while reader.read_line(&mut h).is_ok_and(|n| n > 2) {
            h.clear();
        }
        let path = line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .trim_start_matches('/');
        let file = dir.join(path.split('?').next().unwrap_or(""));
        match std::fs::read(&file) {
            Ok(body) if file.is_file() => {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
            _ => {
                let _ = s.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    }
    Ok(())
}

/// Files spread over a three-level tree with 100 files per leaf directory.
fn generate(dir: &Path, n: usize) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let t = Instant::now();
    let leaf_dir = |leaf: usize| {
        dir.join(format!(
            "d{:03}/e{:03}/f{:03}",
            leaf / 10_000,
            (leaf / 100) % 100,
            leaf % 100
        ))
    };
    (0..n.div_ceil(100))
        .into_par_iter()
        .try_for_each(|l| std::fs::create_dir_all(leaf_dir(l)))?;
    (0..n).into_par_iter().try_for_each(|i| -> Result<()> {
        let p = leaf_dir(i / 100);
        // Few distinct contents keep object writes cheap; status cost is per file anyway.
        std::fs::write(
            p.join(format!("file{i}.txt")),
            format!("content of file {}\n", i % 1000),
        )?;
        Ok(())
    })?;
    println!("wrote {n} files in {:.1}s", t.elapsed().as_secs_f32());
    let git = Git::new(dir, CommandLog::default());
    git.run(&["init", "-q", "-b", "main"])?;
    git.run(&["config", "user.email", "bench@gitkwaken"])?;
    git.run(&["config", "user.name", "bench"])?;
    git.run(&["config", "core.autocrlf", "false"])?;
    let t = Instant::now();
    git.run(&["add", "-A"])?;
    git.run(&["commit", "-q", "-m", "initial"])?;
    println!("git add + commit in {:.1}s", t.elapsed().as_secs_f32());
    Ok(())
}

fn time<T>(label: &str, runs: usize, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..runs {
        let t = Instant::now();
        last = Some(f()?);
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{label:<44} best {best:>9.1} ms");
    Ok(last.unwrap())
}

/// Times refs loading, the full commit walk + topo sort + lane layout, and
/// metadata decoding for one screen of rows.
fn bench_history(dir: &Path) -> Result<()> {
    use gk_core::history;
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let refs = time("refs::load", 3, || gk_core::refs::load(&repo))?;
    let tips = history::resolve_tips(&repo, &history::Scope::All, &refs.tips());
    let h = time("history::build (all branches)", 3, || {
        history::build(&repo, &tips, &AtomicBool::new(false))
    })?;
    println!(
        "commits {}  max graph width {} lanes  layout memory {} KB",
        h.len(),
        h.layout.max_width(),
        h.layout.memory() / 1024
    );
    let g = repo.gix();
    let first: Vec<_> = h.ids.iter().take(60).copied().collect();
    time("commit_meta x60 (one screen)", 3, || {
        first
            .iter()
            .map(|id| history::commit_meta(&g, *id))
            .collect::<Result<Vec<_>>>()
    })?;
    let mid: Vec<_> = h.ids.iter().skip(h.len() / 2).take(60).copied().collect();
    time("commit_meta x60 (middle of history)", 3, || {
        mid.iter()
            .map(|id| history::commit_meta(&g, *id))
            .collect::<Result<Vec<_>>>()
    })?;
    time("search 'fix' over all commits", 1, || {
        Ok(history::search(&repo, &h, "fix", &AtomicBool::new(false)).len())
    })?;
    Ok(())
}

fn bench_status(dir: &Path, runs: usize) -> Result<()> {
    let git = Git::new(dir, CommandLog::default());
    let stop = Arc::new(AtomicBool::new(false));
    let files = git
        .run(&["ls-files", "-z"])?
        .stdout
        .iter()
        .filter(|b| **b == 0)
        .count();
    println!("repository: {} ({files} tracked files)", dir.display());

    // Touch a few files so there is something to report.
    let sample: Vec<String> = git
        .read(&["ls-files"])?
        .lines()
        .step_by((files / 10).max(1))
        .take(10)
        .map(String::from)
        .collect();
    for p in &sample {
        let full = dir.join(p);
        let mut c = std::fs::read(&full)?;
        c.extend_from_slice(b"bench edit\n");
        std::fs::write(&full, c)?;
    }

    time("git status --porcelain=v2 -uall", runs, || {
        git.run(&["status", "--porcelain=v2", "-z", "-uall"])
    })?;
    let gix = time("gitkwaken gix engine (full)", runs, || {
        compute(
            dir,
            &git,
            &[],
            &Options {
                engine: Engine::Gix,
                untracked: Untracked::All,
            },
            &stop,
        )
    })?;
    let cli = time("gitkwaken cli engine (full)", runs, || {
        compute(
            dir,
            &git,
            &[],
            &Options {
                engine: Engine::Cli,
                untracked: Untracked::All,
            },
            &stop,
        )
    })?;
    println!(
        "changes: gix {} / cli {}",
        gix.unstaged.len(),
        cli.unstaged.len()
    );
    let scope: Vec<bstr::BString> = sample.iter().map(|s| s.as_str().into()).collect();
    time("gitkwaken gix engine (10 paths)", runs, || {
        compute(
            dir,
            &git,
            &scope,
            &Options {
                engine: Engine::Gix,
                untracked: Untracked::All,
            },
            &stop,
        )
    })?;
    time("gitkwaken cli engine (10 paths)", runs, || {
        compute(
            dir,
            &git,
            &scope,
            &Options {
                engine: Engine::Cli,
                untracked: Untracked::All,
            },
            &stop,
        )
    })?;

    let mut fast = gk_status::fast::FastStatus::new(dir)?;
    // First call loads the index (once per index change), later ones are what
    // a file-watcher event costs.
    time("gk fast path, index load + 10 paths", 1, || {
        fast.refresh(&scope, Untracked::All)
    })?;
    let out = time("gk fast path (10 paths, index cached)", runs, || {
        fast.refresh(&scope, Untracked::All)
    })?;
    if let gk_status::fast::Outcome::Updated(v) = out {
        println!("fast path changes: {}", v.len());
    }

    // Restore the touched files.
    let mut args = vec!["checkout", "--"];
    args.extend(sample.iter().map(String::as_str));
    git.run(&args)?;
    Ok(())
}
