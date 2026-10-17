//! `gk-bench objects <repo> [n]`: object read throughput, gitoxide in-process
//! vs the long-lived `git cat-file --batch-command` reader (the fallback /
//! `GITKWAKEN_OBJECT_READER=cli` path), plus a process per object for scale.

use anyhow::{Context, Result, anyhow};
use gk_cli::{CatFile, CommandLog};
use gk_core::{ObjectId, ObjectReader, Repo, content};
use std::path::Path;
use std::time::Instant;

pub fn run(args: &[String]) -> Result<()> {
    let dir = Path::new(args.first().context("usage: gk-bench objects <repo> [n]")?);
    let limit: usize = args
        .get(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(50_000);
    let repo = Repo::open(dir, CommandLog::default())?;
    let git = repo.git();
    let out = git.run(&["rev-list", "--objects", "--all"])?;
    let ids: Vec<ObjectId> = out
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| l.len() >= 40)
        .filter_map(|l| ObjectId::from_hex(&l[..40]).ok())
        .take(limit)
        .collect();
    let hex: Vec<String> = ids.iter().map(ObjectId::to_string).collect();
    println!("{} objects from {}", ids.len(), dir.display());

    let g = repo.gix();
    let gix_bytes = best("gitoxide find_object (in-process)", ids.len(), 3, || {
        let mut bytes = 0usize;
        for id in &ids {
            bytes += g.find_object(*id).map_err(|e| anyhow!("{e}"))?.data.len();
        }
        Ok(bytes)
    })?;

    let mut cf = CatFile::new(git);
    cf.info(&hex[0])?; // process start excluded
    let cli_bytes = best(
        "git cat-file --batch-command (1 process)",
        ids.len(),
        3,
        || {
            let mut bytes = 0usize;
            for h in &hex {
                bytes += cf.read(h)?.context("missing object")?.1.len();
            }
            Ok(bytes)
        },
    )?;
    if gix_bytes != cli_bytes {
        anyhow::bail!("readers disagree: {gix_bytes} vs {cli_bytes} bytes");
    }
    println!(
        "  {:.1} MB read per pass by both readers",
        gix_bytes as f64 / 1e6
    );

    let spawn_n = hex.len().min(100);
    best("git cat-file -p (process per object)", spawn_n, 1, || {
        let mut bytes = 0usize;
        for h in &hex[..spawn_n] {
            bytes += git.run(&["cat-file", "-p", h])?.stdout.len();
        }
        Ok(bytes)
    })?;

    // Commit details (commit + parent + tree diff with rename detection).
    let commits: Vec<ObjectId> = git
        .read(&["rev-list", "--all", "--max-count=300"])?
        .lines()
        .filter_map(|l| ObjectId::from_hex(l.as_bytes()).ok())
        .collect();
    let cli = repo.clone().with_object_reader(ObjectReader::Cli);
    for (label, r) in [("gitoxide", &repo), ("git cat-file", &cli)] {
        best(
            &format!("commit_details via {label}"),
            commits.len(),
            3,
            || {
                let mut files = 0;
                for id in &commits {
                    files += content::commit_details(r, *id)?.files.len();
                }
                Ok(files)
            },
        )?;
    }
    Ok(())
}

fn best<T>(label: &str, n: usize, runs: usize, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..runs {
        let t = Instant::now();
        last = Some(f()?);
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "{label:<44} {n:>7} in {:>8.1} ms  {:>9.0}/s  {:>7.2} µs each",
        best * 1000.0,
        n as f64 / best,
        best * 1e6 / n.max(1) as f64
    );
    Ok(last.expect("runs > 0"))
}
