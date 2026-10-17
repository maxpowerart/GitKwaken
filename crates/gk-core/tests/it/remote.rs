use crate::common::*;
use gk_core::ops::{FetchOptions, PushOptions};
use gk_core::refs;

/// A local repo with one commit pushed to a fresh bare remote as `origin/main`.
fn setup() -> (Remote, TestRepo) {
    let remote = Remote::new();
    let a = TestRepo::new();
    a.commit_file("f.txt", "base\n", "base");
    a.repo.add_remote("origin", &remote.url()).unwrap();
    let (c, mut p) = net();
    a.repo
        .push(
            &PushOptions { remote: s("origin"), refspecs: vec![(s("main"), s("main"))], set_upstream: true, ..Default::default() },
            &c,
            &mut p,
        )
        .unwrap();
    (remote, a)
}

#[test]
fn push_set_upstream_and_fetch() {
    let (remote, a) = setup();
    let r = refs::load(&a.repo).unwrap();
    assert_eq!(r.branch("main").unwrap().upstream.as_deref(), Some("origin/main"));
    assert_eq!(remote.git(&["rev-parse", "main"]), a.head());

    let b = TestRepo::clone_from(&remote.path());
    b.commit_file("g.txt", "g\n", "from b");
    b.git(&["push", "-q", "origin", "main"]);

    let (c, mut p) = net();
    a.repo.fetch(Some("origin"), &FetchOptions::default(), &c, &mut p).unwrap();
    let r = refs::load(&a.repo).unwrap();
    assert_eq!(r.branch("main").unwrap().behind, 1);
    a.repo.fetch(None, &FetchOptions { prune: true, tags: true }, &c, &mut p).unwrap();
    assert!(a.repo.fetch(Some("nope"), &FetchOptions::default(), &c, &mut p).is_err());
}

#[test]
fn push_new_branch_without_refspec_sets_upstream() {
    let (remote, a) = setup();
    a.git(&["checkout", "-q", "-b", "feature"]);
    a.commit_file("x.txt", "x\n", "feature work");
    let (c, mut p) = net();
    a.repo
        .push(&PushOptions { remote: s("origin"), set_upstream: true, ..Default::default() }, &c, &mut p)
        .unwrap();
    assert_eq!(remote.git(&["rev-parse", "feature"]), a.head());
    let r = refs::load(&a.repo).unwrap();
    assert_eq!(r.branch("feature").unwrap().upstream.as_deref(), Some("origin/feature"));
}

#[test]
fn push_rejected_then_force_with_lease() {
    let (remote, a) = setup();
    let b = TestRepo::clone_from(&remote.path());
    b.commit_file("g.txt", "g\n", "from b");
    b.git(&["push", "-q", "origin", "main"]);

    // Fetch so the lease matches, then rewrite and force.
    let (c, mut p) = net();
    a.repo.fetch(Some("origin"), &FetchOptions::default(), &c, &mut p).unwrap();
    a.commit_file("h.txt", "h\n", "from a");
    let opts = PushOptions { remote: s("origin"), refspecs: vec![(s("main"), String::new())], ..Default::default() };
    assert!(a.repo.push(&opts, &c, &mut p).is_err(), "non-fast-forward is rejected");
    a.repo.push(&PushOptions { force: true, ..opts.clone() }, &c, &mut p).unwrap();
    assert_eq!(remote.git(&["rev-parse", "main"]), a.head());

    // Stale lease: the remote moved after our last fetch.
    b.git(&["fetch", "-q"]);
    b.git(&["reset", "-q", "--hard", "origin/main"]);
    b.commit_file("i.txt", "i\n", "b again");
    b.git(&["push", "-q", "origin", "main"]);
    a.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
    assert!(a.repo.push(&PushOptions { force: true, ..opts }, &c, &mut p).is_err());
}

#[test]
fn push_tags_and_delete() {
    let (remote, a) = setup();
    a.repo.create_tag("v1", "HEAD", Some("release 1"), false).unwrap();
    a.repo.create_tag("light", "HEAD", None, false).unwrap();
    let (c, mut p) = net();
    a.repo
        .push(&PushOptions { remote: s("origin"), refspecs: vec![(s("main"), s("main"))], tags: true, ..Default::default() }, &c, &mut p)
        .unwrap();
    // --follow-tags pushes annotated tags only.
    assert_eq!(remote.git(&["tag", "-l"]), "v1");
    a.repo.push_tag("origin", "light", false, &c, &mut p).unwrap();
    assert_eq!(remote.git(&["tag", "-l"]), "light\nv1");
    a.repo.push_tag("origin", "light", true, &c, &mut p).unwrap();
    assert_eq!(remote.git(&["tag", "-l"]), "v1");

    a.git(&["push", "-q", "origin", "main:doomed"]);
    a.repo.delete_remote_branch("origin", "doomed", &c, &mut p).unwrap();
    assert!(remote.git(&["branch", "--list", "doomed"]).is_empty());
}

#[test]
fn pull_merge_and_rebase() {
    let (remote, a) = setup();
    let b = TestRepo::clone_from(&remote.path());
    b.commit_file("g.txt", "g\n", "from b");
    b.git(&["push", "-q", "origin", "main"]);
    a.commit_file("h.txt", "h\n", "from a");

    let (c, mut p) = net();
    a.repo.pull(Some("origin"), Some("main"), false, &c, &mut p).unwrap();
    assert_eq!(a.git(&["rev-list", "--parents", "-n1", "HEAD"]).split(' ').count(), 3, "merge commit");
    assert!(a.exists("g.txt"));

    b.commit_file("g.txt", "g2\n", "from b 2");
    b.git(&["push", "-q", "origin", "main"]);
    a.commit_file("h.txt", "h2\n", "from a 2");
    a.repo.pull(None, None, true, &c, &mut p).unwrap();
    // Rebasing linearizes: our commits (not the old merge) on top of theirs.
    assert_eq!(a.subjects("-3"), ["from a 2", "from a", "from b 2"]);
    assert_eq!(a.git(&["rev-list", "--merges", "--count", "origin/main..HEAD"]), "0");
    assert_eq!(refs::load(&a.repo).unwrap().state, gk_core::RepoState::Clean);
}

#[test]
fn pull_conflict_is_ok() {
    let (remote, a) = setup();
    let b = TestRepo::clone_from(&remote.path());
    b.commit_file("f.txt", "theirs\n", "b edits f");
    b.git(&["push", "-q", "origin", "main"]);
    a.commit_file("f.txt", "ours\n", "a edits f");
    let (c, mut p) = net();
    let text = a.repo.pull(Some("origin"), Some("main"), false, &c, &mut p).unwrap();
    assert!(text.contains("CONFLICT"), "{text}");
    assert_eq!(refs::load(&a.repo).unwrap().state, gk_core::RepoState::Merging);
}

#[test]
fn checkout_remote_and_set_upstream() {
    let (_remote, a) = setup();
    a.git(&["push", "-q", "origin", "main:feature"]);
    a.git(&["fetch", "-q", "origin"]);
    a.repo.checkout_remote("origin/feature", "feat").unwrap();
    let r = refs::load(&a.repo).unwrap();
    assert_eq!(r.head, gk_core::Head::Branch(s("feat")));
    assert_eq!(r.branch("feat").unwrap().upstream.as_deref(), Some("origin/feature"));

    a.repo.set_upstream("feat", Some("origin/main")).unwrap();
    assert_eq!(refs::load(&a.repo).unwrap().branch("feat").unwrap().upstream.as_deref(), Some("origin/main"));
    a.repo.set_upstream("feat", None).unwrap();
    assert_eq!(refs::load(&a.repo).unwrap().branch("feat").unwrap().upstream, None);
}

#[test]
fn remote_management() {
    let t = TestRepo::new();
    t.repo.add_remote("up", "https://example.com/a.git").unwrap();
    t.repo.add_remote("other", "C:/path with space/b.git").unwrap();
    assert!(t.repo.add_remote("up", "x").is_err());
    t.repo.rename_remote("up", "upstream").unwrap();
    t.repo.set_remote_url("upstream", "https://example.com/c.git").unwrap();
    let r = refs::load(&t.repo).unwrap();
    let names: Vec<&str> = r.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["other", "upstream"]);
    assert_eq!(r.remotes[1].fetch_url, "https://example.com/c.git");
    assert_eq!(r.remotes[0].fetch_url, "C:/path with space/b.git");
    t.repo.remove_remote("upstream").unwrap();
    t.repo.remove_remote("other").unwrap();
    assert!(refs::load(&t.repo).unwrap().remotes.is_empty());
    assert!(t.repo.remove_remote("other").is_err());
}

#[test]
fn cancelled_fetch() {
    let (_remote, a) = setup();
    let c = gk_cli::CancelToken::new();
    c.cancel();
    let mut p = |_l: gk_cli::Line| {};
    assert!(a.repo.fetch(Some("origin"), &FetchOptions::default(), &c, &mut p).is_err());
}
