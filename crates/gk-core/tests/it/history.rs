use crate::common::*;
use gk_core::ObjectId;
use gk_core::history::{self, History, MetaCache, Scope};
use gk_core::log_filter::{Filter, Pickaxe};
use gk_core::refs;
use std::sync::atomic::AtomicBool;

fn build(t: &TestRepo, scope: &Scope) -> History {
    let r = refs::load(&t.repo).unwrap();
    let tips = history::resolve_tips(&t.repo, scope, &r.tips());
    history::build(&t.repo, &tips, &AtomicBool::new(false)).unwrap()
}

/// Structural invariants every history must satisfy.
fn check(t: &TestRepo, h: &History) {
    assert_eq!(h.layout.len(), h.len());
    assert_eq!(h.row_of.len(), h.len());
    let g = t.repo.gix();
    for (row, id) in h.ids.iter().enumerate() {
        assert_eq!(h.row(id), Some(row));
        let c = g.find_commit(*id).unwrap();
        for p in c.parent_ids() {
            let prow = h.row(&p.detach()).expect("parent in history");
            assert!(
                prow > row,
                "parent {p} at row {prow} not after child at {row}"
            );
        }
    }
}

fn oids(t: &TestRepo, revs: &[&str]) -> Vec<ObjectId> {
    revs.iter().map(|r| t.oid(r)).collect()
}

#[test]
fn empty_and_linear() {
    let t = TestRepo::new();
    let h = build(&t, &Scope::All);
    assert!(h.is_empty());

    for i in 0..5 {
        t.commit_file("f.txt", &format!("{i}\n"), &format!("c{i}"));
    }
    let h = build(&t, &Scope::All);
    assert_eq!(h.len(), 5);
    check(&t, &h);
    assert_eq!(
        h.ids,
        oids(&t, &["HEAD", "HEAD~1", "HEAD~2", "HEAD~3", "HEAD~4"])
    );
}

fn branchy(t: &TestRepo) {
    t.commit_file("f.txt", "0\n", "root");
    t.git(&["checkout", "-q", "-b", "feature"]);
    t.commit_file("g.txt", "1\n", "feat 1");
    t.commit_file("g.txt", "2\n", "feat 2");
    t.git(&["checkout", "-q", "main"]);
    t.commit_file("f.txt", "1\n", "main 1");
    t.git_dated(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
    t.git(&["checkout", "-q", "-b", "other", "main~1"]);
    t.commit_file("h.txt", "x\n", "other 1");
    t.git(&["checkout", "-q", "main"]);
}

#[test]
fn merges_and_multiple_tips() {
    let t = TestRepo::new();
    branchy(&t);
    let all = build(&t, &Scope::All);
    // root, feat1, feat2, main1, merge, other1
    assert_eq!(all.len(), 6);
    check(&t, &all);
    assert_eq!(all.ids[0], t.oid("other"), "newest tip first");
    let merge_row = all.row(&t.oid("main")).unwrap();
    assert!(all.layout.rows[merge_row].is_merge);

    let cur = build(&t, &Scope::Current);
    assert_eq!(cur.len(), 5);
    check(&t, &cur);
    assert!(cur.row(&t.oid("other")).is_none());

    let revs = build(&t, &Scope::Revs(vec![s("feature")]));
    assert_eq!(revs.len(), 3);
    check(&t, &revs);

    let both = build(
        &t,
        &Scope::Revs(vec![s("other"), s("feature"), s("does-not-exist")]),
    );
    assert_eq!(both.len(), 5);
    check(&t, &both);
}

#[test]
fn tag_tips_are_walked() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "0\n", "root");
    t.git(&["checkout", "-q", "-b", "tmp"]);
    t.commit_file("f.txt", "1\n", "tagged only");
    t.git(&["tag", "-a", "-m", "t", "v1"]);
    t.git(&["checkout", "-q", "main"]);
    t.git(&["branch", "-D", "tmp"]);
    let h = build(&t, &Scope::All);
    assert_eq!(h.len(), 2);
    let h = build(&t, &Scope::Revs(vec![s("v1")]));
    assert_eq!(h.len(), 2);
}

#[test]
fn cancelled_build() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "0\n", "root");
    assert!(history::build(&t.repo, &[t.oid("HEAD")], &AtomicBool::new(true)).is_err());
}

#[test]
fn commit_meta_and_cache() {
    let t = TestRepo::new();
    t.write("f.txt", "x\n");
    t.git(&["add", "f.txt"]);
    t.repo
        .git()
        .env("GIT_AUTHOR_DATE", "1600000000 +0200")
        .run(&[
            "commit",
            "-q",
            "--author=Ann Author <ann@example.com>",
            "-m",
            "Summary line\n\nBody text",
        ])
        .unwrap();
    let id = t.oid("HEAD");
    let m = history::commit_meta(&t.repo.gix(), id).unwrap();
    assert_eq!(m.summary, "Summary line");
    assert_eq!(m.author, "Ann Author");
    assert_eq!(m.email, "ann@example.com");
    assert_eq!(m.time, 1_600_000_000);
    assert_eq!(m.offset, 7200);

    let cache = MetaCache::new(t.repo.clone());
    let a = cache.get_sync(id).unwrap();
    let b = cache.get_sync(id).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b), "second lookup is cached");
    assert!(
        cache
            .get_sync(ObjectId::null(gix::hash::Kind::Sha1))
            .is_none()
    );

    // Background loading.
    let cache = std::sync::Arc::new(MetaCache::new(t.repo.clone()));
    let (tx, rx) = crossbeam_channel::bounded(1);
    let first = cache.get_many(&[id], move || {
        let _ = tx.send(());
    });
    if first[0].is_none() {
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    }
    assert_eq!(
        cache.get_many(&[id], || {})[0].as_ref().unwrap().summary,
        "Summary line"
    );
}

#[test]
fn search_message_author_id() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "0\n", "Initial import");
    t.write("f.txt", "1\n");
    t.git(&["add", "f.txt"]);
    t.git_dated(&[
        "commit",
        "-q",
        "--author=Zed Zebra <zed@zoo.org>",
        "-m",
        "Fix the parser\n\nDetails about NEEDLE",
    ]);
    t.commit_file("f.txt", "2\n", "Another change");
    let h = build(&t, &Scope::All);
    let no = AtomicBool::new(false);
    let row = |rev: &str| h.row(&t.oid(rev)).unwrap() as u32;

    assert_eq!(
        history::search(&t.repo, &h, "parser", &no),
        vec![row("HEAD~1")]
    );
    assert_eq!(
        history::search(&t.repo, &h, "needle", &no),
        vec![row("HEAD~1")],
        "body, case-insensitive"
    );
    assert_eq!(
        history::search(&t.repo, &h, "zebra", &no),
        vec![row("HEAD~1")]
    );
    assert_eq!(
        history::search(&t.repo, &h, "zoo.org", &no),
        vec![row("HEAD~1")]
    );
    let id = t.rev("HEAD~2");
    assert_eq!(
        history::search(&t.repo, &h, &id[..8].to_uppercase(), &no),
        vec![row("HEAD~2")]
    );
    let mut all = history::search(&t.repo, &h, "test user", &no);
    all.sort();
    assert_eq!(all, vec![row("HEAD"), row("HEAD~2")]);
    assert!(history::search(&t.repo, &h, "", &no).is_empty());
    assert!(history::search(&t.repo, &h, "nothing-matches-this", &no).is_empty());
}

#[test]
fn tags_to_trees_and_blobs_are_skipped() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "0\n", "root");
    t.git(&["tag", "tree-tag", "HEAD^{tree}"]);
    t.git(&["tag", "-a", "-m", "blob", "blob-tag", "HEAD:f.txt"]);
    let h = build(&t, &Scope::All);
    assert_eq!(h.len(), 1);
    let h = build(&t, &Scope::Revs(vec![s("tree-tag"), s("main")]));
    assert_eq!(h.len(), 1);
}

// ------------------------------------------------------------------ filters

/// Commits with a fixed author and date (author and committer).
fn commit_as(t: &TestRepo, path: &str, content: &str, msg: &str, who: &str, day: &str) -> String {
    t.write(path, content);
    t.git(&["add", "--", path]);
    let date = format!("{day}T12:00:00 +0000");
    t.repo
        .git()
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .read(&["commit", "-q", "-m", msg, &format!("--author={who}")])
        .unwrap();
    t.head()
}

/// Runs the History filter's `git log` arguments, like the History view does.
fn filtered(t: &TestRepo, f: &Filter, revs: &[&str]) -> Vec<String> {
    let revs: Vec<String> = revs.iter().map(|r| r.to_string()).collect();
    let args = f.args(&revs);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    t.git(&args).lines().map(str::to_string).collect()
}

#[test]
fn history_filters_select_exact_commits() {
    let t = TestRepo::new();
    let alice = "Alice Doe <alice@example.com>";
    let bob = "Bob Roe <bob@example.com>";
    let carol = "Carol Poe <carol@example.com>";
    let src = "src/parser.rs";
    let c1 = commit_as(
        &t,
        src,
        "fn parse() {}\n",
        "Add parser",
        alice,
        "2026-01-10",
    );
    let c2 = commit_as(
        &t,
        src,
        "fn parse() { fixed() }\n",
        "Fix parser bug",
        bob,
        "2026-02-10",
    );
    let c3 = commit_as(
        &t,
        "docs/README.md",
        "a needle here\n",
        "Docs: readme",
        alice,
        "2026-03-10",
    );
    let c4 = commit_as(
        &t,
        "docs/README.md",
        "gone\n",
        "Remove the word",
        carol,
        "2026-04-10",
    );
    let c5 = commit_as(
        &t,
        src,
        "fn parse() { fixed(); more() }\n",
        "Refactor parser",
        bob,
        "2026-05-10",
    );
    t.git(&["checkout", "-q", "-b", "side", &c2]);
    let c6 = commit_as(
        &t,
        "src/side.rs",
        "fn side() {}\n",
        "Side parser tweak",
        carol,
        "2026-06-10",
    );
    t.git(&["checkout", "-q", "main"]);

    let run = |f: Filter| filtered(&t, &f, &[]);
    let ids = |v: &[&String]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // Message text: --grep, case-insensitive, trimmed.
    let text = |s: &str| Filter {
        text: s.into(),
        ..Default::default()
    };
    assert_eq!(run(text("parser")), ids(&[&c5, &c2, &c1]));
    assert_eq!(run(text("  PARSER BUG ")), ids(&[&c2]));
    assert_eq!(run(text("no such message")), Vec::<String>::new());

    // Author: name or email, case-insensitive.
    let author = |s: &str| Filter {
        author: s.into(),
        ..Default::default()
    };
    assert_eq!(run(author("alice")), ids(&[&c3, &c1]));
    assert_eq!(run(author("BOB@example")), ids(&[&c5, &c2]));

    // Dates: --since / --until on the committer date.
    let dates = |since: &str, until: &str| Filter {
        since: since.into(),
        until: until.into(),
        ..Default::default()
    };
    assert_eq!(run(dates("2026-03-01", "")), ids(&[&c5, &c4, &c3]));
    assert_eq!(run(dates("", "2026-02-15")), ids(&[&c2, &c1]));
    assert_eq!(run(dates("2026-02-01", "2026-03-31")), ids(&[&c3, &c2]));

    // Path: a folder or a file; Windows separators are converted.
    let path = |s: &str| Filter {
        path: s.into(),
        ..Default::default()
    };
    assert_eq!(run(path("docs")), ids(&[&c4, &c3]));
    assert_eq!(run(path("src\\parser.rs")), ids(&[&c5, &c2, &c1]));

    // Pickaxe: -S counts occurrences, -G matches changed lines.
    let pickaxe = |s: &str, mode| Filter {
        pickaxe: s.into(),
        pickaxe_mode: mode,
        ..Default::default()
    };
    assert_eq!(run(pickaxe("fixed", Pickaxe::String)), ids(&[&c2]));
    assert_eq!(run(pickaxe("fixed", Pickaxe::Regex)), ids(&[&c5, &c2]));
    assert_eq!(run(pickaxe("needle", Pickaxe::String)), ids(&[&c4, &c3]));
    assert_eq!(run(pickaxe("fixed[(][)];", Pickaxe::Regex)), ids(&[&c5]));

    // Fields combine (AND).
    let f = Filter {
        author: "alice".into(),
        path: "src".into(),
        ..Default::default()
    };
    assert_eq!(run(f), ids(&[&c1]));
    let f = Filter {
        text: "parser".into(),
        since: "2026-02-01".into(),
        pickaxe: "fixed".into(),
        pickaxe_mode: Pickaxe::Regex,
        ..Default::default()
    };
    assert_eq!(run(f), ids(&[&c5, &c2]));

    // Revisions: HEAD by default, else the History scope's arguments.
    assert_eq!(
        filtered(&t, &text("parser"), &["side"]),
        ids(&[&c6, &c2, &c1])
    );
    assert_eq!(
        filtered(&t, &text("parser"), &["--branches"]),
        ids(&[&c6, &c5, &c2, &c1])
    );
}
