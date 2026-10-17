//! Streaming blame (`git blame --incremental`) against real repositories:
//! the assembled result must equal the blocking `--porcelain` blame.

use crate::common::*;
use gk_cli::CancelToken;
use gk_core::blame::{Blame, SharedBlameFeed, UNCOMMITTED_ID};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs the streaming blame to completion; returns the assembled blame and
/// how many times it notified.
fn stream(t: &TestRepo, path: &str, rev: Option<&str>) -> (anyhow::Result<Blame>, usize) {
    let feed = SharedBlameFeed::default();
    let calls = AtomicUsize::new(0);
    let notify = || {
        calls.fetch_add(1, Ordering::SeqCst);
    };
    let r = t
        .repo
        .blame_streaming(path, rev, &CancelToken::new(), &feed, &notify);
    let f = feed.lock();
    assert!(f.done);
    let out = r.map(|()| {
        assert!(f.error.is_none());
        assert_eq!(f.lines_done, f.text.as_ref().unwrap().len());
        f.to_blame().expect("every line attributed")
    });
    (out, calls.load(Ordering::SeqCst))
}

/// Same commits per line (ids, not indices: the orders differ), same text.
fn assert_same(a: &Blame, b: &Blame) {
    assert_eq!(a.text, b.text);
    assert_eq!(a.lines.len(), b.lines.len());
    for (i, (x, y)) in a.lines.iter().zip(&b.lines).enumerate() {
        let (cx, mut cy) = (&a.commits[x.commit], b.commits[y.commit].clone());
        if cy.id == UNCOMMITTED_ID {
            // "Now" for the working copy: may tick between the two runs.
            cy.author_time = cx.author_time;
        }
        assert_eq!(cx, &cy, "line {}", i + 1);
        assert_eq!(x.orig_line, y.orig_line, "line {}", i + 1);
    }
}

#[test]
fn streaming_matches_porcelain() {
    let t = TestRepo::new();
    t.commit_file("src/f.rs", "fn a() {}\nfn b() {}\nfn c() {}\n", "first");
    t.git(&["config", "user.name", "Second Author"]);
    t.commit_file(
        "src/f.rs",
        "fn a() {}\nfn b2() {}\nfn c() {}\nfn d() {}\n",
        "second",
    );
    t.git(&["config", "user.name", "Test User"]);
    t.commit_file(
        "src/f.rs",
        "fn a() {}\nfn b2() {}\n  fn c() {}\nfn d() {}\n\tfn e() {}\n",
        "third",
    );
    // Rename into a path git has to quote.
    t.git(&["mv", "src/f.rs", "src/n\u{e4}me f.rs"]);
    t.commit("rename");
    let path = "src/n\u{e4}me f.rs";
    t.write(path, format!("{}fn f() {{}}\n", t.read(path)));

    let (b, notified) = stream(&t, path, None);
    let b = b.unwrap();
    assert_same(&b, &t.repo.blame(path, None).unwrap());
    assert!(notified >= 2, "text + end at least, got {notified}");
    assert_eq!(b.lines.len(), 6);
    assert_eq!(b.commits[b.lines[5].commit].id, UNCOMMITTED_ID);
    // The first commit is a root commit: a boundary.
    assert!(b.commits[b.lines[0].commit].boundary);
    // `previous` points at the old path, unquoted.
    let second = &b.commits[b.lines[1].commit];
    let (prev_id, prev_path) = second.previous.clone().unwrap();
    assert_eq!(prev_path, "src/f.rs");
    // ... and blaming that previous revision works.
    let (old, _) = stream(&t, &prev_path, Some(&prev_id));
    let old = old.unwrap();
    assert_same(&old, &t.repo.blame(&prev_path, Some(&prev_id)).unwrap());
    assert_eq!(old.text, ["fn a() {}", "fn b() {}", "fn c() {}"]);

    // At an older revision.
    let (b, _) = stream(&t, "src/f.rs", Some("HEAD~2"));
    assert_same(
        &b.unwrap(),
        &t.repo.blame("src/f.rs", Some("HEAD~2")).unwrap(),
    );

    // Errors land in the result and in the feed.
    let feed = SharedBlameFeed::default();
    let r = t
        .repo
        .blame_streaming("missing.rs", None, &CancelToken::new(), &feed, &|| {});
    assert!(r.is_err());
    let f = feed.lock();
    assert!(f.done && f.error.is_some());
}

/// Many commits, interleaved: several groups per commit.
#[test]
fn streaming_many_groups() {
    let t = TestRepo::new();
    let mut lines: Vec<String> = (0..400).map(|i| format!("line {i}")).collect();
    t.commit_file("big.txt", &(lines.join("\n") + "\n"), "c0");
    for round in 1..8 {
        for (i, l) in lines.iter_mut().enumerate() {
            if i % (round + 3) == 0 {
                *l = format!("line {i} edited in round {round}");
            }
        }
        t.commit_file("big.txt", &(lines.join("\n") + "\n"), &format!("c{round}"));
    }
    let (b, _) = stream(&t, "big.txt", None);
    let b = b.unwrap();
    assert_eq!(b.commits.len(), 8);
    assert_same(&b, &t.repo.blame("big.txt", None).unwrap());
}

#[test]
fn streaming_cancelled_before_start() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\nb\n", "a");
    let feed = SharedBlameFeed::default();
    let cancel = CancelToken::new();
    cancel.cancel();
    let r = t
        .repo
        .blame_streaming("a.txt", None, &cancel, &feed, &|| {});
    assert!(r.is_err());
    assert!(feed.lock().done);
}

#[test]
fn streaming_crlf_working_copy() {
    let t = TestRepo::new();
    t.commit_file("w.txt", "one\r\ntwo\r\n", "crlf");
    let (b, _) = stream(&t, "w.txt", None);
    let b = b.unwrap();
    assert_eq!(b.text, ["one", "two"]);
    assert_same(&b, &t.repo.blame("w.txt", None).unwrap());
}
