//! Session scheduling tests: priorities, watcher pause, cache, latency.

use super::*;
use gk_cli::{CommandLog, Git};
use std::sync::atomic::AtomicUsize;

struct Tmp {
    dir: tempfile::TempDir,
    git: Git,
    repo: Repo,
}

fn tmp_repo() -> Tmp {
    let dir = tempfile::tempdir().unwrap();
    let git = Git::new(dir.path(), CommandLog::default());
    git.run(&["init", "-q", "-b", "main"]).unwrap();
    for (k, v) in [
        ("user.name", "T"),
        ("user.email", "t@t"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", ".git/no-hooks"),
    ] {
        git.run(&["config", k, v]).unwrap();
    }
    let repo = Repo::open(dir.path(), CommandLog::default()).unwrap();
    Tmp { dir, git, repo }
}

impl Tmp {
    fn commit(&self, file: &str, msg: &str) {
        std::fs::write(self.dir.path().join(file), msg).unwrap();
        self.git.run(&["add", "--", file]).unwrap();
        self.git.run(&["commit", "-q", "-m", msg]).unwrap();
    }

    /// `n` linear commits on main via fast-import (fast even for thousands).
    fn linear(&self, n: usize) {
        let mut s = String::new();
        for i in 1..=n {
            let msg = format!("commit {i}\n");
            s += &format!(
                "commit refs/heads/main\nmark :{i}\ncommitter T <t@t> {} +0000\ndata {}\n{msg}",
                1_700_000_000 + i,
                msg.len()
            );
            if i > 1 {
                s += &format!("from :{}\n", i - 1);
            }
            s += "\n";
        }
        self.git
            .run_stdin(&["fast-import", "--quiet"], s.as_bytes())
            .unwrap();
        self.git.run(&["reset", "-q", "--hard", "main"]).unwrap();
    }
}

fn opts(watch: bool) -> SessionOptions {
    SessionOptions {
        watch,
        cache: false,
        ..Default::default()
    }
}

fn wait(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn loaded(s: &Snapshot, commits: usize) -> bool {
    s.history.len() == commits
        && !s.history_loading
        && s.status_gen > 0
        && !s.status_loading
        && !s.refs.branches.is_empty()
}

#[test]
fn metas_complete_while_background_pool_is_busy() {
    let t = tmp_repo();
    t.commit("a.txt", "first");
    t.commit("b.txt", "second");
    let ss = Session::open(t.repo.clone(), opts(false), || {});
    wait("load", 20, || loaded(&ss.snapshot(), 2));
    let ids = ss.snapshot().history.ids.clone();

    // A long search occupies every background thread.
    let (release_tx, release_rx) = crossbeam_channel::unbounded::<()>();
    let started = Arc::new(AtomicUsize::new(0));
    let n = rayon::current_num_threads();
    for _ in 0..n {
        let rx = release_rx.clone();
        let started = started.clone();
        rayon::spawn(move || {
            started.fetch_add(1, Ordering::SeqCst);
            let _ = rx.recv_timeout(Duration::from_secs(30));
        });
    }
    wait("background pool saturated", 10, || {
        started.load(Ordering::SeqCst) == n
    });
    assert!(matches!(ss.search("second"), Load::Loading));

    let t0 = Instant::now();
    wait("metas while the background pool is busy", 5, || {
        ss.metas(&ids).iter().all(Option::is_some)
    });
    eprintln!("metas under background load: {:?}", t0.elapsed());
    let d = ss.commit_details(ids[0]);
    assert!(!matches!(d, Load::Failed(_)));
    wait("commit details while busy", 5, || {
        matches!(ss.commit_details(ids[0]), Load::Ready(_))
    });
    // The search is still queued behind the blockers.
    assert!(matches!(ss.search("second"), Load::Loading));
    for _ in 0..n {
        release_tx.send(()).unwrap();
    }
    wait(
        "search after release",
        10,
        || matches!(ss.search("second"), Load::Ready(r) if r.len() == 1),
    );
}

#[test]
fn own_op_is_refreshed_once() {
    let t = tmp_repo();
    t.commit("a.txt", "first");
    let ss = Session::open(t.repo.clone(), opts(true), || {});
    wait("load", 20, || loaded(&ss.snapshot(), 1));
    // Let the watcher settle (our own initial `git status` may touch the index).
    std::thread::sleep(Duration::from_millis(600));
    let g0 = ss.snapshot().status_gen;
    let dir = t.dir.path().to_path_buf();
    ss.run_op("make files", move |repo, _, _| {
        for i in 0..50 {
            std::fs::write(dir.join(format!("f{i}.txt")), "x")?;
            if i % 10 == 0 {
                // Spread the writes over more than one debounce window.
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        repo.git().run(&["add", "-A"])?;
        repo.git().run(&["commit", "-q", "-m", "files"])?;
        Ok(String::new())
    });
    wait("op result", 20, || {
        let s = ss.snapshot();
        s.op.is_none() && loaded(&s, 2) && s.status.is_clean()
    });
    std::thread::sleep(Duration::from_millis(800));
    let s = ss.snapshot();
    let refreshes = s.status_gen - g0;
    eprintln!("status refreshes caused by the op: {refreshes}");
    assert!(s.messages.is_empty(), "{:?}", s.messages);
    assert!(
        refreshes <= 2,
        "{refreshes} status refreshes for one operation"
    );
}

#[test]
fn quiet_ops_are_serialized_and_silent() {
    let t = tmp_repo();
    t.commit("a.txt", "first");
    let ss = Session::open(t.repo.clone(), opts(false), || {});
    wait("load", 20, || loaded(&ss.snapshot(), 1));
    let order = Arc::new(Mutex::new(Vec::new()));
    let o = order.clone();
    ss.run_op("loud", move |_, _, _| {
        std::thread::sleep(Duration::from_millis(150));
        o.lock().push("loud");
        Ok(String::new())
    });
    let o = order.clone();
    let saw_quiet_label = Arc::new(AtomicBool::new(false));
    ss.run_quiet_op("auto-fetch", move |_, _, _| {
        o.lock().push("quiet");
        anyhow::bail!("offline")
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while order.lock().len() < 2 && Instant::now() < deadline {
        if ss.snapshot().op.is_some_and(|op| op.label == "auto-fetch") {
            saw_quiet_label.store(true, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(*order.lock(), ["loud", "quiet"]);
    assert!(!saw_quiet_label.load(Ordering::SeqCst));
    assert!(ss.snapshot().messages.is_empty());
}

#[test]
fn status_after_save_is_fast() {
    let t = tmp_repo();
    t.commit("a.txt", "first");
    let ss = Session::open(t.repo.clone(), opts(true), || {});
    wait("load", 20, || loaded(&ss.snapshot(), 1));
    std::thread::sleep(Duration::from_millis(500));
    let mut best = Duration::MAX;
    for i in 0..4 {
        let name = format!("saved{i}.txt");
        let t0 = Instant::now();
        std::fs::write(t.dir.path().join(&name), "x").unwrap();
        wait("saved file in status", 5, || {
            ss.snapshot().status.unstaged.iter().any(|e| e.path == name)
        });
        best = best.min(t0.elapsed());
        std::thread::sleep(Duration::from_millis(600));
    }
    eprintln!("save -> status snapshot (best of 4): {best:?}");
    assert!(best < Duration::from_millis(150), "{best:?}");
}

#[test]
fn partial_history_then_full() {
    let t = tmp_repo();
    t.linear(history::PARTIAL_ROWS + 300);
    let tips = history::tip_ids(&t.repo, &Scope::All);
    assert_eq!(tips.len(), 1);
    let mut partial = None;
    let full = history::build_with_partial(
        &t.repo,
        &tips,
        &AtomicBool::new(false),
        history::PARTIAL_ROWS,
        &mut |h| partial = Some(h),
    )
    .unwrap();
    let partial = partial.expect("partial history published");
    assert_eq!(partial.len(), history::PARTIAL_ROWS);
    assert_eq!(full.len(), history::PARTIAL_ROWS + 300);
    assert_eq!(partial.ids[..], full.ids[..history::PARTIAL_ROWS]);
    assert_eq!(
        partial.layout.rows[..100],
        full.layout.rows[..100],
        "first screen is laid out the same"
    );

    // A newer request interrupts the running build.
    let stop = AtomicBool::new(false);
    let r = history::build_with_partial(&t.repo, &tips, &stop, history::PARTIAL_ROWS, &mut |_| {
        stop.store(true, Ordering::SeqCst)
    });
    assert!(r.is_err());

    // The session shows the partial history first, then the full one.
    let ss = Session::open(t.repo.clone(), opts(false), || {});
    let mut saw_partial = false;
    wait("full history", 20, || {
        let s = ss.snapshot();
        saw_partial |= s.history_partial && s.history.len() == history::PARTIAL_ROWS;
        loaded(&s, history::PARTIAL_ROWS + 300) && !s.history_partial
    });
    eprintln!("session showed the partial history: {saw_partial}");
}

#[test]
fn cached_snapshot_is_published_then_validated() {
    let t = tmp_repo();
    t.commit("a.txt", "first");
    t.commit("b.txt", "second");
    std::fs::write(t.dir.path().join("dirty.txt"), "x").unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let o = SessionOptions {
        watch: false,
        cache: true,
        cache_file: Some(cache_dir.path().join("cache.redb")),
        ..Default::default()
    };
    {
        let ss = Session::open(t.repo.clone(), o.clone(), || {});
        wait("first load", 20, || loaded(&ss.snapshot(), 2));
        assert!(!ss.snapshot().from_cache);
    }
    let rc = RepoCache::open(o.cache_file.as_deref(), t.repo.workdir()).unwrap();
    wait("cache written", 10, || {
        let c = rc.load();
        c.history.is_some() && c.refs.is_some() && c.status.is_some()
    });

    // Reopen: the cached snapshot shows up at once, marked as such.
    let t0 = Instant::now();
    let ss = Session::open(t.repo.clone(), o.clone(), || {});
    let mut first = None;
    wait("cached history", 10, || {
        let s = ss.snapshot();
        if s.history.len() == 2 && first.is_none() {
            first = Some((t0.elapsed(), s.from_cache, s.status.unstaged.len()));
        }
        first.is_some()
    });
    let (took, from_cache, dirty) = first.unwrap();
    eprintln!("open -> cached history: {took:?}");
    assert!(from_cache, "first history came from the cache");
    assert_eq!(dirty, 1, "cached status shown");
    wait("validated", 20, || {
        let s = ss.snapshot();
        !s.from_cache && loaded(&s, 2)
    });
    drop(ss);

    // A commit made while closed: the cached history is replaced.
    t.commit("c.txt", "third");
    wait("cache flushed", 10, || rc.load().history.is_some());
    let ss = Session::open(t.repo.clone(), o, || {});
    wait("rebuilt history", 20, || {
        let s = ss.snapshot();
        !s.from_cache && loaded(&s, 3)
    });
}
