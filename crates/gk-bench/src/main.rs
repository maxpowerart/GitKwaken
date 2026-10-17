//! Benchmark harness.
//!
//! `gk-bench gen <dir> <files>`     create a synthetic repository
//! `gk-bench status <dir> [runs]`   time status engines against `git status`

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
        Some("history") => {
            let dir = PathBuf::from(args.get(2).context("dir")?);
            bench_history(&dir)
        }
        _ => bail!("usage: gk-bench gen <dir> <files> | status <dir> [runs] | history <dir>"),
    }
}

/// Files spread over a three-level tree with 100 files per leaf directory.
fn generate(dir: &Path, n: usize) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let t = Instant::now();
    let leaf_dir = |leaf: usize| dir.join(format!("d{:03}/e{:03}/f{:03}", leaf / 10_000, (leaf / 100) % 100, leaf % 100));
    (0..n.div_ceil(100)).into_par_iter().try_for_each(|l| std::fs::create_dir_all(leaf_dir(l)))?;
    (0..n).into_par_iter().try_for_each(|i| -> Result<()> {
        let p = leaf_dir(i / 100);
        std::fs::write(p.join(format!("file{i}.txt")), format!("content of file {i}\n"))?;
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
        first.iter().map(|id| history::commit_meta(&g, *id)).collect::<Result<Vec<_>>>()
    })?;
    let mid: Vec<_> = h.ids.iter().skip(h.len() / 2).take(60).copied().collect();
    time("commit_meta x60 (middle of history)", 3, || {
        mid.iter().map(|id| history::commit_meta(&g, *id)).collect::<Result<Vec<_>>>()
    })?;
    time("search 'fix' over all commits", 1, || {
        Ok(history::search(&repo, &h, "fix", &AtomicBool::new(false)).len())
    })?;
    Ok(())
}

fn bench_status(dir: &Path, runs: usize) -> Result<()> {
    let git = Git::new(dir, CommandLog::default());
    let stop = Arc::new(AtomicBool::new(false));
    let files = git.run(&["ls-files", "-z"])?.stdout.iter().filter(|b| **b == 0).count();
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
        compute(dir, &git, &[], &Options { engine: Engine::Gix, untracked: Untracked::All }, &stop)
    })?;
    let cli = time("gitkwaken cli engine (full)", runs, || {
        compute(dir, &git, &[], &Options { engine: Engine::Cli, untracked: Untracked::All }, &stop)
    })?;
    println!("changes: gix {} / cli {}", gix.unstaged.len(), cli.unstaged.len());
    let scope: Vec<bstr::BString> = sample.iter().map(|s| s.as_str().into()).collect();
    time("gitkwaken gix engine (10 paths)", runs, || {
        compute(dir, &git, &scope, &Options { engine: Engine::Gix, untracked: Untracked::All }, &stop)
    })?;
    time("gitkwaken cli engine (10 paths)", runs, || {
        compute(dir, &git, &scope, &Options { engine: Engine::Cli, untracked: Untracked::All }, &stop)
    })?;

    // Restore the touched files.
    let mut args = vec!["checkout", "--"];
    args.extend(sample.iter().map(String::as_str));
    git.run(&args)?;
    Ok(())
}
