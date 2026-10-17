use crate::common::*;
use gk_core::refs::{self, Head, Label, RepoState};

#[test]
fn unborn_head() {
    let t = TestRepo::new();
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.head, Head::Unborn(s("main")));
    assert_eq!(r.head_id, None);
    assert!(r.branches.is_empty());
    assert_eq!(r.state, RepoState::Clean);
    assert!(r.tips().is_empty());
}

#[test]
fn branch_and_detached_head() {
    let t = TestRepo::new();
    let c1 = t.commit_file("a.txt", "1\n", "one");
    let c2 = t.commit_file("a.txt", "2\n", "two");
    t.git(&["branch", "feature", &c1]);
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.head, Head::Branch(s("main")));
    assert_eq!(r.head_id.as_deref(), Some(c2.as_str()));
    assert_eq!(r.current_branch().unwrap().name, "main");
    assert_eq!(r.branch("feature").unwrap().target, c1);
    assert!(!r.branch("feature").unwrap().is_head);
    assert!(r.labels[&c2].contains(&Label::Branch {
        name: s("main"),
        is_head: true
    }));

    t.git(&["checkout", "-q", "--detach", &c1]);
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.head, Head::Detached(c1.clone()));
    assert_eq!(r.head.branch(), None);
    assert!(r.current_branch().is_none());
    assert!(r.labels[&c1].contains(&Label::Head));
}

#[test]
fn tags_are_peeled() {
    let t = TestRepo::new();
    let c1 = t.commit_file("a.txt", "1\n", "one");
    let c2 = t.commit_file("a.txt", "2\n", "two");
    t.git(&["tag", "light", &c1]);
    t.git(&["tag", "-a", "-m", "annotated", "anno", &c2]);
    // A tag of a tag must still resolve to the commit.
    t.git(&["tag", "-a", "-m", "nested", "nested", "anno"]);
    let r = refs::load(&t.repo).unwrap();
    let tag = |n: &str| r.tags.iter().find(|t| t.name == n).unwrap().clone();
    assert_eq!(tag("light").target, c1);
    assert!(!tag("light").annotated);
    assert_eq!(tag("anno").target, c2);
    assert!(tag("anno").annotated);
    assert_eq!(tag("nested").target, c2);
    assert!(r.labels[&c2].contains(&Label::Tag(s("anno"))));
    assert!(r.tips().contains(&c1));
}

#[test]
fn stashes_listed() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1\n", "one");
    t.write("a.txt", "dirty\n");
    t.git(&["stash", "push", "-m", "first"]);
    t.write("a.txt", "dirty again\n");
    t.git(&["stash", "push", "-m", "second"]);
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.stashes.len(), 2);
    assert_eq!(r.stashes[0].index, 0);
    assert!(r.stashes[0].message.contains("second"), "{:?}", r.stashes);
    assert!(r.stashes[1].message.contains("first"));
    assert_eq!(r.stashes[0].id, t.rev("stash@{0}"));
    assert!(r.labels[&r.stashes[0].id].contains(&Label::Stash));
}

#[test]
fn upstream_ahead_behind_and_remote_branches() {
    let remote = Remote::new();
    let a = TestRepo::new();
    a.commit_file("f.txt", "base\n", "base");
    a.git(&["remote", "add", "origin", &remote.url()]);
    a.git(&["push", "-q", "-u", "origin", "main"]);
    a.git(&["push", "-q", "origin", "main:side"]);

    let b = TestRepo::clone_from(&remote.path());
    b.commit_file("g.txt", "from b\n", "b1");
    b.git(&["push", "-q", "origin", "main"]);

    a.commit_file("h.txt", "a1\n", "a1");
    a.commit_file("h.txt", "a2\n", "a2");
    a.git(&["fetch", "-q", "origin"]);

    let r = refs::load(&a.repo).unwrap();
    let main = r.branch("main").unwrap();
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (2, 1));
    assert!(!main.upstream_gone);
    let names: Vec<String> = r.remote_branches.iter().map(|b| b.full()).collect();
    assert!(
        names.contains(&s("origin/main")) && names.contains(&s("origin/side")),
        "{names:?}"
    );
    assert_eq!(r.remotes.len(), 1);
    assert_eq!(r.remotes[0].name, "origin");
    assert_eq!(r.remotes[0].fetch_url, remote.url());
    assert_eq!(r.remotes[0].push_url, remote.url());
    let om = r
        .remote_branches
        .iter()
        .find(|b| b.full() == "origin/main")
        .unwrap();
    assert!(r.labels[&om.target].contains(&Label::Remote(s("origin/main"))));

    // Upstream deleted on the remote.
    a.git(&["branch", "-q", "--track", "side", "origin/side"]);
    b.git(&["push", "-q", "origin", ":side"]);
    a.git(&["fetch", "-q", "--prune", "origin"]);
    let r = refs::load(&a.repo).unwrap();
    assert!(r.branch("side").unwrap().upstream_gone);
}

#[test]
fn remote_url_with_spaces() {
    let t = TestRepo::new();
    t.git(&[
        "remote",
        "add",
        "spaced",
        "C:/some dir/with spaces/repo.git",
    ]);
    t.git(&["remote", "set-url", "--push", "spaced", "D:/push dir/x.git"]);
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.remotes.len(), 1);
    assert_eq!(r.remotes[0].fetch_url, "C:/some dir/with spaces/repo.git");
    assert_eq!(r.remotes[0].push_url, "D:/push dir/x.git");
}

/// main and topic both change line 1 of c.txt.
pub fn conflicting_branches(t: &TestRepo) {
    t.commit_file("c.txt", "base\n", "base");
    t.git(&["checkout", "-q", "-b", "topic"]);
    t.commit_file("c.txt", "topic\n", "topic change");
    t.git(&["checkout", "-q", "main"]);
    t.commit_file("c.txt", "main\n", "main change");
}

#[test]
fn state_merge_conflict() {
    let t = TestRepo::new();
    conflicting_branches(&t);
    assert!(!t.git_raw(&["merge", "topic"]).success());
    assert_eq!(refs::load(&t.repo).unwrap().state, RepoState::Merging);
    assert_eq!(RepoState::Merging.label(), "MERGING");
}

#[test]
fn state_rebase_conflict() {
    let t = TestRepo::new();
    conflicting_branches(&t);
    t.git(&["checkout", "-q", "topic"]);
    assert!(!t.git_raw(&["rebase", "main"]).success());
    assert_eq!(
        refs::load(&t.repo).unwrap().state,
        RepoState::Rebasing { interactive: false }
    );
    t.git(&["rebase", "--abort"]);
    assert!(!t.git_raw(&["rebase", "-i", "main"]).success());
    assert_eq!(
        refs::load(&t.repo).unwrap().state,
        RepoState::Rebasing { interactive: true }
    );
}

#[test]
fn state_cherry_pick_and_revert() {
    let t = TestRepo::new();
    conflicting_branches(&t);
    assert!(!t.git_raw(&["cherry-pick", "topic"]).success());
    assert_eq!(refs::load(&t.repo).unwrap().state, RepoState::CherryPicking);
    t.git(&["cherry-pick", "--abort"]);

    // Reverting "base" conflicts with "main change".
    let base = t.rev("main~1");
    assert!(!t.git_raw(&["revert", "--no-edit", &base]).success());
    assert_eq!(refs::load(&t.repo).unwrap().state, RepoState::Reverting);
}

#[test]
fn remote_names_with_slashes() {
    let remote = Remote::new();
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.git(&["remote", "add", "team/upstream", &remote.url()]);
    t.git(&["push", "-q", "team/upstream", "main:feature/x"]);
    t.git(&["fetch", "-q", "team/upstream"]);
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.remote_branches.len(), 1);
    assert_eq!(r.remote_branches[0].remote, "team/upstream");
    assert_eq!(r.remote_branches[0].name, "feature/x");
    assert_eq!(r.remote_branches[0].full(), "team/upstream/feature/x");
}
