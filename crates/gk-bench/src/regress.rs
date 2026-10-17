//! `gk-bench regress`: a fixed set of measurements compared against a
//! committed baseline (`bench/baseline-<os>.json`), failing on regressions.
//!
//! ```text
//! gk-bench regress [--data DIR] [--runs N] [--baseline FILE] [--out FILE]
//!                  [--record] [--threshold PCT] [--floor-ms MS]
//! gk-bench gen-dag <dir> <commits>
//! ```
//!
//! Each metric is the median of `--runs` (default 5) runs. A metric regresses
//! when it is more than `--threshold` percent (default 10) slower than the
//! baseline median *and* more than `--floor-ms` (default 2 ms) slower in
//! absolute terms, so sub-millisecond metrics do not flap on timer noise.
//! Without a baseline file (or with `--record`) the results become the new
//! baseline and the command succeeds.

use anyhow::{Context, Result, bail};
use gk_cli::{CommandLog, Git};
use gk_status::{Engine, Options, Untracked};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Bump when the generated data or a measurement changes meaning; a baseline
/// with another schema is ignored (re-recorded) instead of compared.
const SCHEMA: u32 = 1;
const STATUS_FILES: usize = 50_000;
const DAG_COMMITS: usize = 50_000;
const DIFF_LINES: usize = 50_000;
const LAYOUT_ROWS: u32 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub median_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub os: String,
    pub runs: usize,
    pub threads: usize,
    pub git: String,
    pub metrics: BTreeMap<String, Metric>,
}

struct Args {
    data: PathBuf,
    runs: usize,
    baseline: PathBuf,
    out: Option<PathBuf>,
    record: bool,
    threshold: f64,
    floor_ms: f64,
}

fn parse_args(args: &[String]) -> Result<Args> {
    let mut a = Args {
        data: PathBuf::from("target/bench-regress"),
        runs: 5,
        baseline: PathBuf::from(format!("bench/baseline-{}.json", std::env::consts::OS)),
        out: None,
        record: false,
        threshold: 10.0,
        floor_ms: 2.0,
    };
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let mut val = || it.next().with_context(|| format!("{k} needs a value"));
        match k.as_str() {
            "--data" => a.data = val()?.into(),
            "--runs" => a.runs = val()?.parse()?,
            "--baseline" => a.baseline = val()?.into(),
            "--out" => a.out = Some(val()?.into()),
            "--record" => a.record = true,
            "--threshold" => a.threshold = val()?.parse()?,
            "--floor-ms" => a.floor_ms = val()?.parse()?,
            other => bail!("unknown regress option {other}"),
        }
    }
    if a.runs == 0 {
        bail!("--runs must be at least 1");
    }
    Ok(a)
}

/// Runs `f` `runs` times after one warm-up run; returns the timing summary.
fn measure(name: &str, runs: usize, mut f: impl FnMut() -> Result<()>) -> Result<(String, Metric)> {
    f().with_context(|| format!("{name} (warm-up)"))?;
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        f().with_context(|| name.to_string())?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let m = Metric {
        median_ms: samples[samples.len() / 2],
        min_ms: samples[0],
        max_ms: samples[samples.len() - 1],
    };
    println!(
        "{name:<28} median {:>9.2} ms   min {:>9.2}   max {:>9.2}",
        m.median_ms, m.min_ms, m.max_ms
    );
    Ok((name.to_string(), m))
}

pub fn run(args: &[String]) -> Result<()> {
    let a = parse_args(args)?;
    std::fs::create_dir_all(&a.data)?;
    let status_dir = a.data.join(format!("status-{STATUS_FILES}"));
    let dag_dir = a.data.join(format!("dag-{DAG_COMMITS}"));
    if !status_dir.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&status_dir);
        crate::generate(&status_dir, STATUS_FILES)?;
    }
    if !dag_dir.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&dag_dir);
        gen_dag(&dag_dir, DAG_COMMITS)?;
    }

    let mut metrics = BTreeMap::new();
    let mut add = |r: (String, Metric)| {
        metrics.insert(r.0, r.1);
    };

    // ---- status on 50k files, 10 of them modified
    own_repo(&status_dir)?;
    own_repo(&dag_dir)?;
    let git = Git::new(&status_dir, CommandLog::default());
    let tracked: Vec<String> = git.read(&["ls-files"])?.lines().map(String::from).collect();
    let sample: Vec<String> = tracked
        .iter()
        .step_by((tracked.len() / 10).max(1))
        .take(10)
        .cloned()
        .collect();
    let originals: Vec<Vec<u8>> = sample
        .iter()
        .map(|p| std::fs::read(status_dir.join(p)))
        .collect::<std::io::Result<_>>()?;
    for (p, orig) in sample.iter().zip(&originals) {
        let mut c = orig.clone();
        c.extend_from_slice(b"bench edit\n");
        std::fs::write(status_dir.join(p), c)?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let full = |engine| {
        let opts = Options {
            engine,
            untracked: Untracked::All,
        };
        let (git, dir, stop) = (&git, &status_dir, &stop);
        move || -> Result<()> {
            let st = gk_status::compute(dir, git, &[], &opts, stop)?;
            anyhow::ensure!(st.unstaged.len() == 10, "expected 10 changes");
            Ok(())
        }
    };
    let result = (|| -> Result<()> {
        add(measure("status_full_cli_50k", a.runs, full(Engine::Cli))?);
        add(measure("status_full_gix_50k", a.runs, full(Engine::Gix))?);
        let scope: Vec<bstr::BString> = sample.iter().map(|s| s.as_str().into()).collect();
        let mut fast = gk_status::fast::FastStatus::new(&status_dir)?;
        add(measure("status_fast_10_paths", a.runs, || {
            match fast.refresh(&scope, Untracked::All)? {
                gk_status::fast::Outcome::Updated(v) if v.len() == 10 => Ok(()),
                _ => bail!("fast path did not report the 10 edits"),
            }
        })?);
        Ok(())
    })();
    for (p, orig) in sample.iter().zip(&originals) {
        std::fs::write(status_dir.join(p), orig)?;
    }
    result?;

    // ---- history: refs + walk + topo order + lane layout of the 50k-commit DAG
    let repo = gk_core::Repo::open(&dag_dir, CommandLog::default())?;
    add(measure("history_build_50k", a.runs, || {
        use gk_core::history;
        let refs = gk_core::refs::load(&repo)?;
        let tips = history::resolve_tips(&repo, &history::Scope::All, &refs.tips());
        let h = history::build(&repo, &tips, &AtomicBool::new(false))?;
        anyhow::ensure!(h.len() == DAG_COMMITS, "walked {} commits", h.len());
        Ok(())
    })?);

    // ---- diff of a 50k-line file with scattered edits
    let (old, new) = diff_inputs(DIFF_LINES);
    add(measure("diff_50k_lines", a.runs, || {
        let d = gk_diff::diff(&old, &new, gk_diff::Options::default());
        anyhow::ensure!(d.additions > 0 && !d.hunks.is_empty());
        Ok(())
    })?);
    add(measure("diff_50k_lines_full_context", a.runs, || {
        let d = gk_diff::diff(&old, &new, gk_diff::Options::full());
        anyhow::ensure!(d.lines.len() >= DIFF_LINES);
        Ok(())
    })?);

    // ---- lane layout of 1M synthetic rows
    add(measure("layout_1m_rows", a.runs, || {
        let l = synthetic_layout(LAYOUT_ROWS);
        anyhow::ensure!(l.len() == LAYOUT_ROWS as usize);
        Ok(())
    })?);

    let report = Report {
        schema: SCHEMA,
        os: std::env::consts::OS.to_string(),
        runs: a.runs,
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        git: Git::new(".", CommandLog::default())
            .version()
            .map(|(a, b, c)| format!("{a}.{b}.{c}"))
            .unwrap_or_default(),
        metrics,
    };
    let json = serde_json::to_string_pretty(&report)? + "\n";
    if let Some(out) = &a.out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, &json)?;
        println!("wrote {}", out.display());
    }

    let baseline: Option<Report> = std::fs::read_to_string(&a.baseline)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|b: &Report| b.schema == SCHEMA);
    let Some(base) = baseline.filter(|_| !a.record) else {
        if let Some(parent) = a.baseline.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&a.baseline, &json)?;
        println!(
            "recorded baseline {} (no comparable baseline, or --record)",
            a.baseline.display()
        );
        summary(&format!(
            "### gk-bench regress\n\nRecorded a new baseline `{}`; commit it to enable the gate.\n",
            a.baseline.display()
        ));
        return Ok(());
    };

    let (table, failures) = compare(&base, &report, a.threshold, a.floor_ms);
    println!("\n{table}");
    summary(&format!("### gk-bench regress\n\n{table}"));
    if failures.is_empty() {
        println!(
            "no regressions (threshold {}%, floor {} ms)",
            a.threshold, a.floor_ms
        );
        Ok(())
    } else {
        bail!("performance regression in: {}", failures.join(", "))
    }
}

/// Markdown table of baseline vs current plus the names of regressed metrics.
pub fn compare(
    base: &Report,
    cur: &Report,
    threshold: f64,
    floor_ms: f64,
) -> (String, Vec<String>) {
    let mut t = String::from(
        "| metric | baseline ms | current ms | change | |\n|---|---:|---:|---:|---|\n",
    );
    let mut failures = Vec::new();
    for (name, m) in &cur.metrics {
        let Some(b) = base.metrics.get(name) else {
            let _ = writeln!(t, "| {name} | – | {:.2} | new | |", m.median_ms);
            continue;
        };
        let delta = m.median_ms - b.median_ms;
        let pct = if b.median_ms > 0.0 {
            delta / b.median_ms * 100.0
        } else {
            0.0
        };
        let regressed = pct > threshold && delta > floor_ms;
        if regressed {
            failures.push(name.clone());
        }
        let _ = writeln!(
            t,
            "| {name} | {:.2} | {:.2} | {pct:+.1}% | {} |",
            b.median_ms,
            m.median_ms,
            if regressed { "REGRESSION" } else { "ok" }
        );
    }
    for name in base
        .metrics
        .keys()
        .filter(|k| !cur.metrics.contains_key(*k))
    {
        let _ = writeln!(
            t,
            "| {name} | {:.2} | – | removed | |",
            base.metrics[name].median_ms
        );
    }
    (t, failures)
}

/// Safety: benchmarks modify and reset working trees, so they only ever run git
/// in a folder that is its own repository. Without this check git would walk up
/// from a half-generated folder (e.g. under `target/`) into the project repo.
pub fn own_repo(dir: &Path) -> Result<()> {
    if !dir.join(".git").exists() {
        bail!(
            "refusing to run: {} is not the root of a git repository",
            dir.display()
        );
    }
    Ok(())
}

/// Appends to the GitHub Actions job summary when running in CI.
fn summary(text: &str) {
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// 1M rows: a long first-parent chain with a merge every 10 rows (same shape
/// as the gk-graph unit test).
pub fn synthetic_layout(n: u32) -> gk_graph::Layout {
    let mut l = gk_graph::Layout::new();
    for i in 0..n {
        if i % 10 == 0 && i + 2 < n {
            l.push(i, &[i + 1, i + 2]);
        } else if i + 1 < n {
            l.push(i, &[i + 1]);
        } else {
            l.push(i, &[]);
        }
    }
    l
}

/// Deterministic source-like text and a copy with ~3% of lines changed,
/// inserted or removed.
pub fn diff_inputs(lines: usize) -> (Vec<u8>, Vec<u8>) {
    let mut old = String::new();
    let mut new = String::new();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in 0..lines {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let line = format!(
            "    let value_{i} = compute({}, {}); // {:x}\n",
            i % 97,
            i % 13,
            x % 4096
        );
        old.push_str(&line);
        match x % 100 {
            0 => new.push_str(&format!(
                "    let value_{i} = changed({}); // edited\n",
                x % 7
            )),
            1 => {
                new.push_str(&line);
                new.push_str("    // inserted line\n");
            }
            2 => {} // removed
            _ => new.push_str(&line),
        }
    }
    (old.into_bytes(), new.into_bytes())
}

/// Builds a synthetic repository with `n` commits through `git fast-import`:
/// a main line plus six topic branches that are merged back periodically,
/// tags every 2000 commits and a commit-graph.
pub fn gen_dag(dir: &Path, n: usize) -> Result<()> {
    let t = Instant::now();
    std::fs::create_dir_all(dir)?;
    let git = Git::new(dir, CommandLog::default());
    git.run(&["init", "-q", "-b", "main"])?;
    own_repo(dir)?;
    git.run(&["config", "core.autocrlf", "false"])?;
    const TOPICS: usize = 6;
    let mut s = String::with_capacity(n * 320);
    // Tip mark of main (index 0) and each topic; 0 = not started.
    let mut tips = [0usize; TOPICS + 1];
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut ts: i64 = 1_500_000_000;
    for i in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let mark = i + 1;
        ts += 37 + (x % 600) as i64;
        let lane = if i == 0 {
            0
        } else {
            (x % (TOPICS as u64 + 1)) as usize
        };
        // Merge a topic into main every 40 commits.
        let merge_topic = i % 40 == 39 && tips[1 + (i / 40) % TOPICS] != 0;
        let (branch, from, merge) = if merge_topic {
            let k = 1 + (i / 40) % TOPICS;
            let m = tips[k];
            tips[k] = 0;
            ("main".to_string(), tips[0], Some(m))
        } else if lane == 0 {
            ("main".to_string(), tips[0], None)
        } else {
            let from = if tips[lane] == 0 { tips[0] } else { tips[lane] };
            (format!("topic-{lane}"), from, None)
        };
        let msg = if merge.is_some() {
            format!("Merge topic into main ({i})\n")
        } else {
            format!("Change {i}: fix module {} and update docs\n", x % 500)
        };
        let content = format!("{i}\n");
        let _ = write!(
            s,
            "commit refs/heads/{branch}\nmark :{mark}\ncommitter Bench <bench@gitkwaken> {ts} +0000\ndata {}\n{msg}",
            msg.len()
        );
        if from != 0 {
            let _ = writeln!(s, "from :{from}");
        }
        if let Some(m) = merge {
            let _ = writeln!(s, "merge :{m}");
        }
        let _ = write!(
            s,
            "M 100644 inline src/m{}/f{}.txt\ndata {}\n{content}\n",
            x % 50,
            x % 2000,
            content.len()
        );
        if branch == "main" {
            tips[0] = mark;
        } else {
            tips[lane] = mark;
        }
        if branch == "main" && i % 2000 == 1999 {
            let _ = write!(s, "reset refs/tags/v{}\nfrom :{mark}\n\n", i / 2000);
        }
    }
    // Leave unmerged topics as branches; point every topic ref at its tip.
    for (k, tip) in tips.iter().enumerate().skip(1) {
        if *tip != 0 {
            let _ = write!(s, "reset refs/heads/topic-{k}\nfrom :{tip}\n\n");
        }
    }
    git.run_stdin(&["fast-import", "--quiet"], s.as_bytes())?;
    git.run(&["commit-graph", "write", "--reachable"])?;
    let count = git.read(&["rev-list", "--all", "--count"])?;
    println!(
        "generated {} commits in {:.1}s ({})",
        count.trim(),
        t.elapsed().as_secs_f32(),
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(vals: &[(&str, f64)]) -> Report {
        Report {
            schema: SCHEMA,
            os: "test".into(),
            runs: 5,
            threads: 1,
            git: String::new(),
            metrics: vals
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        Metric {
                            median_ms: *v,
                            min_ms: *v,
                            max_ms: *v,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn gate_uses_percentage_and_absolute_floor() {
        let base = report(&[("slow", 100.0), ("tiny", 0.2), ("ok", 50.0), ("gone", 1.0)]);
        let cur = report(&[("slow", 111.0), ("tiny", 0.9), ("ok", 54.0), ("new", 3.0)]);
        let (table, fails) = compare(&base, &cur, 10.0, 2.0);
        // +11% and +11 ms: fails. +350% but +0.7 ms: under the floor. +8%: ok.
        assert_eq!(fails, ["slow"]);
        assert!(table.contains("| new |"), "{table}");
        assert!(table.contains("removed"), "{table}");
    }

    #[test]
    fn own_repo_refuses_plain_folders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(own_repo(dir.path()).is_err());
        Git::new(dir.path(), CommandLog::default())
            .run(&["init", "-q"])
            .unwrap();
        own_repo(dir.path()).unwrap();
    }

    #[test]
    fn report_roundtrips_as_json() {
        let r = report(&[("a", 1.5)]);
        let s = serde_json::to_string(&r).unwrap();
        let back: Report = serde_json::from_str(&s).unwrap();
        assert_eq!(back.metrics["a"].median_ms, 1.5);
    }

    #[test]
    fn diff_inputs_are_deterministic_and_differ() {
        let (a, b) = diff_inputs(2000);
        assert_eq!(diff_inputs(2000), (a.clone(), b.clone()));
        assert_ne!(a, b);
        assert_eq!(a.iter().filter(|c| **c == b'\n').count(), 2000);
    }

    #[test]
    fn small_dag_has_expected_shape() {
        let dir = tempfile::tempdir().unwrap();
        gen_dag(dir.path(), 500).unwrap();
        let git = Git::new(dir.path(), CommandLog::default());
        assert_eq!(
            git.read(&["rev-list", "--all", "--count"]).unwrap().trim(),
            "500"
        );
        let merges = git
            .read(&["rev-list", "--all", "--merges", "--count"])
            .unwrap();
        assert!(merges.trim().parse::<u32>().unwrap() > 5, "{merges}");
        let repo = gk_core::Repo::open(dir.path(), CommandLog::default()).unwrap();
        let refs = gk_core::refs::load(&repo).unwrap();
        let tips =
            gk_core::history::resolve_tips(&repo, &gk_core::history::Scope::All, &refs.tips());
        let h = gk_core::history::build(&repo, &tips, &AtomicBool::new(false)).unwrap();
        assert_eq!(h.len(), 500);
    }
}
