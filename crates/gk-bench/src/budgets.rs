//! Measurements for performance budgets that are not covered elsewhere.
//!
//! ```text
//! gk-bench stage <repo> [files]        stage + unstage N modified files (default 100000)
//! gk-bench discard <repo>              discard of one file with and without the backup
//! gk-bench filter [paths]              fuzzy-filter N synthetic paths with nucleo-matcher (default 1M)
//! gk-bench diff [lines]                gk_diff::diff on an N-line file (default 50000)
//! gk-bench file-history <repo> <path>… `git log --follow -n 50` per path
//! ```

use anyhow::{Context, Result, bail};
use gk_cli::{CommandLog, Git};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;
use std::path::Path;
use std::time::Instant;

/// Median of `runs` timed runs in milliseconds (after one warm-up), printed.
fn median<T>(label: &str, runs: usize, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut last = f()?;
    let mut v = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        last = f()?;
        v.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    v.sort_by(f64::total_cmp);
    println!(
        "{label:<52} median {:>9.2} ms  (min {:.2}, max {:.2}, n={runs})",
        v[v.len() / 2],
        v[0],
        v[v.len() - 1]
    );
    Ok(last)
}

fn once<T>(label: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let t = Instant::now();
    let r = f()?;
    println!(
        "{label:<52}        {:>9.2} ms  (single run)",
        t.elapsed().as_secs_f64() * 1000.0
    );
    Ok(r)
}

/// Modifies `n` tracked files, then times `Repo::stage` (in-process blob
/// writes plus one index write above 1000 paths) and `Repo::unstage`;
/// restores everything.
pub fn stage(dir: &Path, n: usize) -> Result<()> {
    crate::regress::own_repo(dir)?;
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let git = repo.git();
    let all: Vec<String> = git.read(&["ls-files"])?.lines().map(String::from).collect();
    if all.len() < n {
        bail!("{} has only {} tracked files", dir.display(), all.len());
    }
    let step = all.len() / n;
    let paths: Vec<String> = all.iter().step_by(step.max(1)).take(n).cloned().collect();
    println!(
        "repository {} ({} files), staging {}",
        dir.display(),
        all.len(),
        paths.len()
    );
    let originals: Vec<Vec<u8>> = paths
        .par_iter()
        .map(|p| std::fs::read(dir.join(p)))
        .collect::<std::io::Result<_>>()?;
    // `unique`: every file gets distinct content (one new blob per file).
    let touch = |suffix: &[u8], unique: bool| -> Result<()> {
        paths
            .par_iter()
            .zip(&originals)
            .try_for_each(|(p, o)| {
                let mut c = o.clone();
                c.extend_from_slice(suffix);
                if unique {
                    c.extend_from_slice(p.as_bytes());
                    c.push(b'\n');
                }
                std::fs::write(dir.join(p), c)
            })
            .map_err(Into::into)
    };
    let result = (|| -> Result<()> {
        touch(b"bench stage edit\n", false)?;
        // Prime git's index stat cache so the first run is not penalised.
        git.run(&["status", "--porcelain", "-uno"])?;
        once(
            &format!("Repo::stage {} modified files", paths.len()),
            || repo.stage(&paths),
        )?;
        let staged = git
            .read(&["diff", "--cached", "--name-only"])?
            .lines()
            .count();
        anyhow::ensure!(staged == paths.len(), "staged {staged}");
        // The staged entries must carry valid stat data: otherwise git (and
        // the status engine) re-hash every file on the next status.
        let unstaged = once("git diff --name-only after stage", || {
            Ok(git.read(&["diff", "--name-only"])?.lines().count())
        })?;
        anyhow::ensure!(unstaged == 0, "{unstaged} files still unstaged");
        once(&format!("Repo::unstage {} files", paths.len()), || {
            repo.unstage(&paths)
        })?;
        // Second round: distinct content per file, so every blob is new.
        // A per-run nonce: blobs of earlier runs must not exist already.
        let nonce = format!(
            "bench stage edit 2 {:?}\n",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?
        );
        touch(nonce.as_bytes(), true)?;
        once(
            &format!("Repo::stage {} files, all-new blobs", paths.len()),
            || repo.stage(&paths),
        )?;
        once("Repo::unstage_all", || repo.unstage_all())?;
        Ok(())
    })();
    // Restore content and index.
    paths
        .par_iter()
        .zip(&originals)
        .try_for_each(|(p, o)| std::fs::write(dir.join(p), o))?;
    let _ = git.run(&["reset", "-q"]);
    let _ = git.run(&["status", "--porcelain", "-uno"]);
    result
}

/// Deterministic, realistic-looking repository paths.
pub fn synthetic_paths(n: usize) -> Vec<String> {
    const TOP: &[&str] = &[
        "src",
        "crates",
        "docs",
        "tests",
        "assets",
        "tools",
        "third_party",
        "lib",
        "include",
        "scripts",
        "build",
        "platform",
    ];
    const MID: &[&str] = &[
        "core", "ui", "net", "render", "storage", "util", "widgets", "parser", "compiler",
        "runtime", "graphics", "audio", "input", "history", "status", "diff", "common", "server",
    ];
    const LEAF: &[&str] = &[
        "main", "mod", "lib", "config", "handler", "manager", "buffer", "context", "layout",
        "session", "command", "request", "response", "theme", "matcher", "index", "cache",
    ];
    const EXT: &[&str] = &[
        "rs", "c", "h", "cpp", "md", "txt", "toml", "json", "py", "ts",
    ];
    let mut x: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x as usize
    };
    (0..n)
        .map(|i| {
            let depth = 1 + next() % 4;
            let mut p = String::with_capacity(64);
            p.push_str(TOP[next() % TOP.len()]);
            for _ in 0..depth {
                p.push('/');
                p.push_str(MID[next() % MID.len()]);
                if next() % 3 == 0 {
                    p.push_str(&format!("_{}", next() % 100));
                }
            }
            p.push('/');
            p.push_str(LEAF[next() % LEAF.len()]);
            p.push_str(&format!("_{i}."));
            p.push_str(EXT[next() % EXT.len()]);
            p
        })
        .collect()
}

/// Fuzzy filter over `n` synthetic paths with nucleo-matcher, as the file
/// list does per keystroke: single-threaded and split across rayon threads.
pub fn filter(n: usize) -> Result<()> {
    let paths = once(&format!("generate {n} synthetic paths"), || {
        Ok(synthetic_paths(n))
    })?;
    // Pre-converted haystacks (what a list keeps between keystrokes).
    let utf32: Vec<nucleo_matcher::Utf32String> = once("convert to Utf32String", || {
        Ok(paths.par_iter().map(|p| p.as_str().into()).collect())
    })?;
    let cfg = Config::DEFAULT.match_paths();
    for query in [
        "s",
        "src",
        "core/main",
        "rndrlay",
        "history session rs",
        "zzzz",
    ] {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let hits = median(&format!("nucleo 1 thread, query {query:?}"), 5, || {
            let mut m = Matcher::new(cfg.clone());
            Ok(utf32
                .iter()
                .filter(|h| pattern.score(h.slice(..), &mut m).is_some())
                .count())
        })?;
        let par = median(&format!("nucleo rayon, query {query:?}"), 5, || {
            Ok(utf32
                .par_iter()
                .map_init(
                    || Matcher::new(cfg.clone()),
                    |m, h| pattern.score(h.slice(..), m).is_some() as usize,
                )
                .sum::<usize>())
        })?;
        anyhow::ensure!(hits == par);
        println!("{:<52} {hits} hits", "");
    }
    // Matching straight from &str (no cached Utf32String), single thread.
    let pattern = Pattern::parse("core/main", CaseMatching::Smart, Normalization::Smart);
    median("nucleo 1 thread from &str, query \"core/main\"", 5, || {
        let mut m = Matcher::new(cfg.clone());
        let mut buf = Vec::new();
        Ok(paths
            .iter()
            .filter(|p| pattern.score(Utf32Str::new(p, &mut buf), &mut m).is_some())
            .count())
    })?;
    // Reference: plain case-insensitive substring filter.
    median("substring (ascii lowercase) \"main\"", 5, || {
        Ok(paths
            .par_iter()
            .filter(|p| p.to_ascii_lowercase().contains("main"))
            .count())
    })?;
    Ok(())
}

pub fn diff(lines: usize) -> Result<()> {
    let (old, new) = crate::regress::diff_inputs(lines);
    let d = median(
        &format!("gk_diff::diff {lines} lines, ~3% edited, 3 ctx"),
        5,
        || Ok(gk_diff::diff(&old, &new, gk_diff::Options::default())),
    )?;
    println!(
        "{:<52} {} hunks, +{} -{}",
        "",
        d.hunks.len(),
        d.additions,
        d.deletions
    );
    median(
        &format!("gk_diff::diff {lines} lines, whole file"),
        5,
        || Ok(gk_diff::diff(&old, &new, gk_diff::Options::full())),
    )?;
    median(
        &format!("gk_diff::diff new file of {lines} lines"),
        5,
        || Ok(gk_diff::diff(b"", &new, gk_diff::Options::default())),
    )?;
    let ws = gk_diff::Options {
        ignore_whitespace: true,
        ..Default::default()
    };
    median(
        &format!("gk_diff::diff {lines} lines, ignore whitespace"),
        5,
        || Ok(gk_diff::diff(&old, &new, ws)),
    )?;
    median(&format!("gk_diff::to_unified {lines} lines"), 5, || {
        Ok(gk_diff::to_unified("a", "b", &d).len())
    })?;
    Ok(())
}

/// Time to the first 50 commits of a file's history, as the File History view
/// asks git (`log --follow`), plus the variant without rename following.
pub fn file_history(dir: &Path, paths: &[String]) -> Result<()> {
    crate::regress::own_repo(dir)?;
    let git = Git::new(dir, CommandLog::default());
    let paths: Vec<String> = if paths.is_empty() {
        vec!["Makefile".into(), "builtin/log.c".into(), "diff.c".into()]
    } else {
        paths.to_vec()
    };
    for p in &paths {
        let n = median(&format!("log --follow -n 50 -- {p}"), 5, || {
            Ok(git
                .run(&["log", "--follow", "-n", "50", "--format=%H", "--", p])?
                .stdout_str()
                .lines()
                .count())
        })?;
        median(&format!("log -n 50 -- {p} (no --follow)"), 5, || {
            Ok(git
                .run(&["log", "-n", "50", "--format=%H", "--", p])?
                .stdout
                .len())
        })?;
        let all = median(&format!("log --follow (all) -- {p}"), 3, || {
            Ok(git
                .run(&["log", "--follow", "--format=%H", "--", p])?
                .stdout_str()
                .lines()
                .count())
        })?;
        println!("{:<52} first page {n}, total {all} commits", "");
    }
    Ok(())
}

/// Cost of the discard backup: `Repo::discard` of one modified file (backup
/// plus checkout) against the checkout alone, and the backup by itself.
pub fn discard(dir: &Path) -> Result<()> {
    crate::regress::own_repo(dir)?;
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let git = repo.git();
    let path = git
        .read(&["ls-files"])?
        .lines()
        .next()
        .context("no tracked files")?
        .to_string();
    let original = std::fs::read(dir.join(&path))?;
    let edit = || std::fs::write(dir.join(&path), b"bench discard edit\n");
    let p = std::slice::from_ref(&path);
    let result = (|| -> Result<()> {
        median("Repo::backup_discard 1 file", 7, || {
            edit()?;
            repo.backup_discard(p, &[], &[])
        })?;
        median("Repo::discard 1 file (backup + checkout)", 7, || {
            edit()?;
            repo.discard(p, &[])
        })?;
        median("git checkout 1 file (no backup)", 7, || {
            edit()?;
            git.run(&["checkout", "--", &path])
        })?;
        Ok(())
    })();
    std::fs::write(dir.join(&path), original)?;
    result
}

pub fn run(cmd: &str, args: &[String]) -> Result<()> {
    match cmd {
        "discard" => discard(Path::new(args.first().context("discard <repo>")?)),
        "diff-open" => diff_open(
            Path::new(args.first().context("diff-open <repo> <path>")?),
            args.get(1).context("diff-open <repo> <path>")?,
        ),
        "stage" => {
            let dir = args.first().context("stage <repo> [files]")?;
            let n = args
                .get(1)
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(100_000);
            stage(Path::new(dir), n)
        }
        "filter" => filter(
            args.first()
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(1_000_000),
        ),
        "diff" => diff(
            args.first()
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(50_000),
        ),
        "file-history" => {
            let dir = args.first().context("file-history <repo> [paths…]")?;
            file_history(Path::new(dir), &args[1..])
        }
        _ => bail!("unknown budget command {cmd}"),
    }
}

/// What opening an unstaged file's diff costs in the app, step by step:
/// reading both sides (index blob + worktree file through the filter
/// pipeline) and the diff itself, as `Session::diff` runs them.
pub fn diff_open(dir: &Path, path: &str) -> Result<()> {
    crate::regress::own_repo(dir)?;
    let repo = gk_core::Repo::open(dir, CommandLog::default())?;
    let target = gk_core::content::DiffTarget::Unstaged { path: path.into() };
    let (old, new) = median(&format!("load_sides {path}"), 7, || {
        gk_core::content::load_sides(&repo, &target)
    })?;
    let (old, new) = (old.unwrap_or_default(), new.unwrap_or_default());
    let d = median("gk_diff::diff (3 lines context)", 7, || {
        Ok(gk_diff::diff(&old, &new, gk_diff::Options::default()))
    })?;
    median("load_sides + diff", 7, || {
        let (o, n) = gk_core::content::load_sides(&repo, &target)?;
        Ok(gk_diff::diff(
            o.as_deref().unwrap_or_default(),
            n.as_deref().unwrap_or_default(),
            gk_diff::Options::default(),
        ))
    })?;
    println!(
        "{:<52} {} + {} bytes, {} diff lines",
        "",
        old.len(),
        new.len(),
        d.lines.len()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_paths_are_unique_and_stable() {
        let a = synthetic_paths(1000);
        assert_eq!(a, synthetic_paths(1000));
        let mut s = a.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), 1000);
        assert!(a.iter().all(|p| p.contains('/') && p.contains('.')));
    }

    #[test]
    fn nucleo_matches_path_subsequence() {
        let cfg = Config::DEFAULT.match_paths();
        let mut m = Matcher::new(cfg);
        let p = Pattern::parse("core/main", CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        assert!(
            p.score(Utf32Str::new("src/core/ui/main_1.rs", &mut buf), &mut m)
                .is_some()
        );
        assert!(
            p.score(Utf32Str::new("docs/readme.md", &mut buf), &mut m)
                .is_none()
        );
    }
}
