use crate::common::*;
use gk_core::RepoState;
use gk_core::ops::*;
use gk_core::refs;
use gk_diff::{Line, LineKind};

/// `git diff --cached` without the `index <ids>` line.
fn cached_patch(t: &TestRepo) -> String {
    t.staged_diff()
        .lines()
        .filter(|l| !l.starts_with("index "))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn added(n: u32) -> impl Fn(&Line) -> bool {
    move |l: &Line| l.kind == LineKind::Added && l.new_no == n
}

fn state(t: &TestRepo) -> RepoState {
    refs::load(&t.repo).unwrap().state
}

// ------------------------------------------------------------------ staging

#[test]
fn stage_unstage_unborn() {
    let t = TestRepo::new();
    t.write("a.txt", "a\n");
    t.write("b c.txt", "spaced\n");
    t.repo.stage(&[s("a.txt"), s("b c.txt")]).unwrap();
    assert_eq!(t.status(), "A  a.txt\nA  \"b c.txt\"");
    t.repo.unstage(&[s("a.txt")]).unwrap();
    assert_eq!(t.status(), "A  \"b c.txt\"\n?? a.txt");
    t.repo.stage_all().unwrap();
    assert_eq!(t.status(), "A  a.txt\nA  \"b c.txt\"");
    t.repo.unstage_all().unwrap();
    assert_eq!(t.status(), "?? a.txt\n?? \"b c.txt\"");
    t.repo.stage(&[]).unwrap();
    t.repo.unstage(&[]).unwrap();
}

#[test]
fn stage_unstage_with_head() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.commit_file("d.txt", "d\n", "init2");
    t.write("a.txt", "changed\n");
    std::fs::remove_file(t.repo.abs("d.txt")).unwrap();
    t.write("dir/n.txt", "new\n");
    t.repo
        .stage(&[s("a.txt"), s("d.txt"), s("dir/n.txt")])
        .unwrap();
    assert_eq!(t.status(), "M  a.txt\nD  d.txt\nA  dir/n.txt");
    t.repo.unstage(&[s("d.txt"), s("dir/n.txt")]).unwrap();
    assert_eq!(t.status(), "M  a.txt\n D d.txt\n?? dir/n.txt");
    t.repo.stage_all().unwrap();
    assert_eq!(t.status(), "M  a.txt\nD  d.txt\nA  dir/n.txt");
    t.repo.unstage_all().unwrap();
    assert_eq!(t.status(), " M a.txt\n D d.txt\n?? dir/n.txt");
}

#[test]
fn discard_tracked_and_untracked() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.commit_file("b.txt", "b\n", "init2");
    t.write("a.txt", "changed\n");
    std::fs::remove_file(t.repo.abs("b.txt")).unwrap();
    t.write("u.txt", "u\n");
    t.write("udir/x/y.txt", "y\n");
    t.repo
        .discard(&[s("a.txt"), s("b.txt")], &[s("u.txt"), s("udir/")])
        .unwrap();
    assert_eq!(t.read("a.txt"), "a\n");
    assert_eq!(t.read("b.txt"), "b\n");
    assert!(!t.exists("u.txt") && !t.exists("udir"));
    assert_eq!(t.status(), "");
    // Missing paths are fine.
    t.repo.discard(&[], &[s("not-there.txt")]).unwrap();
}

#[test]
fn stage_lines_subset() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "a\nb\nc\nd\n", "init");
    t.write("f.txt", "a\nB\nc\nd\nE\n");
    // Stage only the b -> B change.
    t.repo
        .stage_lines("f.txt", false, &|l: &Line| match l.kind {
            LineKind::Removed => l.old_no == 2,
            LineKind::Added => l.new_no == 2,
            LineKind::Context => false,
        })
        .unwrap();
    assert_eq!(
        cached_patch(&t),
        "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n"
    );
    assert_eq!(t.read("f.txt"), "a\nB\nc\nd\nE\n", "worktree untouched");

    // Stage the rest, then unstage just the added E again.
    t.repo.stage_lines("f.txt", false, &|_| true).unwrap();
    assert_eq!(t.index_content("f.txt"), b"a\nB\nc\nd\nE\n");
    t.repo.stage_lines("f.txt", true, &added(5)).unwrap();
    assert_eq!(t.index_content("f.txt"), b"a\nB\nc\nd\n");
    assert_eq!(t.read("f.txt"), "a\nB\nc\nd\nE\n");

    // Unstage the removal only: b comes back next to B.
    t.repo
        .stage_lines("f.txt", true, &|l: &Line| l.kind == LineKind::Removed)
        .unwrap();
    assert_eq!(t.index_content("f.txt"), b"a\nb\nB\nc\nd\n");
}

#[test]
fn stage_lines_no_eol_and_executable_mode_kept() {
    let t = TestRepo::new();
    t.write("x.sh", "one\ntwo");
    t.git(&["add", "x.sh"]);
    t.git(&["update-index", "--chmod=+x", "x.sh"]);
    t.commit("init");
    t.write("x.sh", "one\ntwo\nthree\nfour");
    t.repo.stage_lines("x.sh", false, &added(3)).unwrap();
    assert_eq!(t.index_content("x.sh"), b"one\ntwo\nthree\n");
    assert!(t.git(&["ls-files", "-s", "x.sh"]).starts_with("100755 "));
}

#[test]
fn discard_lines_in_worktree() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "a\nb\nc\nd\n", "init");
    t.write("f.txt", "a\nB\nc\nd\nE\n");
    t.repo
        .discard_lines("f.txt", &|l: &Line| match l.kind {
            LineKind::Removed => l.old_no == 2,
            LineKind::Added => l.new_no == 2,
            LineKind::Context => false,
        })
        .unwrap();
    assert_eq!(t.read("f.txt"), "a\nb\nc\nd\nE\n");
    assert_eq!(t.staged_diff(), "");
}

#[test]
fn stage_lines_untracked_file() {
    let t = TestRepo::new();
    t.commit_file("other.txt", "o\n", "init");
    t.write("new.txt", "x\ny\nz\n");
    t.repo
        .stage_lines("new.txt", false, &|l: &Line| {
            l.kind == LineKind::Added && l.new_no != 2
        })
        .unwrap();
    assert_eq!(t.index_content("new.txt"), b"x\nz\n");
    assert_eq!(t.status(), "AM new.txt");
    assert_eq!(t.read("new.txt"), "x\ny\nz\n");
}

#[test]
fn stage_lines_untracked_file_in_unborn_repo() {
    let t = TestRepo::new();
    t.write("new.txt", "x\ny\n");
    t.repo.stage_lines("new.txt", false, &added(1)).unwrap();
    assert_eq!(t.index_content("new.txt"), b"x\n");
    // Unstaging every line of a new file makes it untracked again.
    t.repo.stage_lines("new.txt", true, &|_| true).unwrap();
    assert_eq!(t.status(), "?? new.txt");
    // Selecting nothing changes nothing.
    t.repo.stage_lines("new.txt", false, &|_| false).unwrap();
    assert_eq!(t.status(), "?? new.txt");
}

#[test]
fn stage_lines_deleted_file() {
    let t = TestRepo::new();
    t.commit_file("d.txt", "1\n2\n", "init");
    std::fs::remove_file(t.repo.abs("d.txt")).unwrap();
    t.repo
        .stage_lines("d.txt", false, &|l: &Line| l.old_no == 1)
        .unwrap();
    assert_eq!(t.index_content("d.txt"), b"2\n");
    t.repo.stage_lines("d.txt", false, &|_| true).unwrap();
    assert_eq!(
        t.status(),
        "D  d.txt",
        "staging every removed line stages the deletion"
    );
}

#[test]
fn stage_lines_conflicted_rejected() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    t.repo
        .merge(
            "topic",
            &MergeOptions {
                commit: true,
                ..Default::default()
            },
        )
        .unwrap();
    let err = t.repo.stage_lines("c.txt", false, &|_| true).unwrap_err();
    assert!(err.to_string().contains("conflicts"), "{err}");
    assert!(t.status().starts_with("UU c.txt"));
}

#[test]
fn stage_lines_crlf_autocrlf() {
    let t = TestRepo::with_autocrlf(true);
    t.write("w.txt", "a\r\nb\r\nc\r\n");
    t.git(&["add", "w.txt"]);
    t.commit("init");
    assert_eq!(t.index_content("w.txt"), b"a\nb\nc\n");
    let wt = b"a\r\nB\r\nc\r\nD\r\n";
    t.write("w.txt", wt);
    t.repo
        .stage_lines("w.txt", false, &|l: &Line| match l.kind {
            LineKind::Removed => l.old_no == 2,
            LineKind::Added => l.new_no == 2,
            LineKind::Context => false,
        })
        .unwrap();
    assert_eq!(t.index_content("w.txt"), b"a\nB\nc\n", "index stays LF");
    assert_eq!(t.read_bytes("w.txt"), wt, "worktree untouched");
    assert_eq!(
        cached_patch(&t),
        "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
    );
    // Unstage it again, then discard the D line in the worktree.
    t.repo.stage_lines("w.txt", true, &|_| true).unwrap();
    assert_eq!(t.index_content("w.txt"), b"a\nb\nc\n");
    t.repo.discard_lines("w.txt", &added(4)).unwrap();
    assert_eq!(t.read_bytes("w.txt"), b"a\r\nB\r\nc\r\n");
    // Untracked CRLF file.
    t.write("n.txt", "1\r\n2\r\n");
    t.repo.stage_lines("n.txt", false, &added(2)).unwrap();
    assert_eq!(t.index_content("n.txt"), b"2\n");
}

#[test]
fn stage_lines_binary_rejected() {
    let t = TestRepo::new();
    t.commit_file("b.bin", "x\n", "init");
    t.write("b.bin", b"\0\x01\x02");
    assert!(t.repo.stage_lines("b.bin", false, &|_| true).is_err());
    assert!(t.repo.discard_lines("b.bin", &|_| true).is_err());
}

// ------------------------------------------------------------------ commit

#[test]
fn commit_amend_signoff() {
    let t = TestRepo::new();
    t.write("a.txt", "a\n");
    t.repo.stage_all().unwrap();
    let err = t
        .repo
        .commit(&CommitOptions {
            message: s("  \n"),
            ..Default::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");

    t.repo
        .commit(&CommitOptions {
            message: s("First\n\nBody\n# kept? no, stripped"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t.repo.head_message().unwrap(), "First\n\nBody\n");
    let first = t.head();

    t.write("b.txt", "b\n");
    t.repo.stage_all().unwrap();
    t.repo
        .commit(&CommitOptions {
            message: s("Amended"),
            amend: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t.subjects("HEAD"), ["Amended"]);
    assert_ne!(t.head(), first);
    assert_eq!(t.git(&["ls-tree", "--name-only", "HEAD"]), "a.txt\nb.txt");

    // Amending with an empty message keeps the old one.
    t.write("c.txt", "c\n");
    t.repo.stage_all().unwrap();
    t.repo
        .commit(&CommitOptions {
            amend: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t.repo.head_message().unwrap(), "Amended\n");
    assert_eq!(t.git(&["rev-list", "--count", "HEAD"]), "1");

    t.repo
        .commit(&CommitOptions {
            message: s("Signed"),
            signoff: true,
            allow_empty: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.repo.head_message().unwrap(),
        "Signed\n\nSigned-off-by: Test User <test@example.com>\n"
    );

    t.repo
        .commit(&CommitOptions {
            message: s("Other author"),
            allow_empty: true,
            author: Some(s("Someone Else <else@example.com>")),
            sign: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.git(&["log", "-1", "--format=%an <%ae>"]),
        "Someone Else <else@example.com>"
    );

    // Nothing staged.
    assert!(
        t.repo
            .commit(&CommitOptions {
                message: s("nothing"),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn head_message_unborn_fails() {
    let t = TestRepo::new();
    assert!(t.repo.head_message().is_err());
}

// ------------------------------------------------------------------ branches and tags

#[test]
fn branches() {
    let t = TestRepo::new();
    let c1 = t.commit_file("a.txt", "1\n", "one");
    t.commit_file("a.txt", "2\n", "two");
    t.repo.create_branch("b1", Some(&c1), false).unwrap();
    assert_eq!(t.rev("b1"), c1);
    assert_eq!(t.git(&["branch", "--show-current"]), "main");
    t.repo.create_branch("b2", None, true).unwrap();
    assert_eq!(t.git(&["branch", "--show-current"]), "b2");
    t.repo.checkout("b1").unwrap();
    assert_eq!(t.read("a.txt"), "1\n");
    t.repo.rename_branch("b1", "renamed").unwrap();
    assert_eq!(t.git(&["branch", "--show-current"]), "renamed");
    t.repo.checkout("main").unwrap();
    t.repo
        .create_branch("unmerged", Some("renamed"), true)
        .unwrap();
    t.commit_file("u.txt", "u\n", "unmerged work");
    t.repo.checkout("main").unwrap();
    assert!(t.repo.delete_branch("unmerged", false).is_err());
    t.repo.delete_branch("unmerged", true).unwrap();
    t.repo.delete_branch("b2", false).unwrap();
    let r = refs::load(&t.repo).unwrap();
    let names: Vec<&str> = r.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main", "renamed"]);
    assert!(t.repo.checkout("no-such-branch").is_err());
}

#[test]
fn tags() {
    let t = TestRepo::new();
    let c1 = t.commit_file("a.txt", "1\n", "one");
    let c2 = t.commit_file("a.txt", "2\n", "two");
    t.repo.create_tag("light", &c1, None, false).unwrap();
    t.repo.create_tag("blank", &c1, Some("  "), false).unwrap();
    t.repo
        .create_tag("anno", "HEAD", Some("Release\n\nnotes"), false)
        .unwrap();
    let r = refs::load(&t.repo).unwrap();
    let get = |n: &str| r.tags.iter().find(|t| t.name == n).unwrap().clone();
    assert!(!get("light").annotated && get("light").target == c1);
    assert!(!get("blank").annotated);
    assert!(get("anno").annotated && get("anno").target == c2);
    assert_eq!(
        t.git(&["tag", "-l", "--format=%(contents)", "anno"]),
        "Release\n\nnotes"
    );
    t.repo.delete_tag("light").unwrap();
    t.repo.delete_tag("anno").unwrap();
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.tags.len(), 1);
    assert!(t.repo.delete_tag("light").is_err());
}

// ------------------------------------------------------------------ merge

fn merge_opts(ff: FastForward) -> MergeOptions {
    MergeOptions {
        ff,
        commit: true,
        ..Default::default()
    }
}

#[test]
fn merge_fast_forward_and_no_ff() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    t.git(&["checkout", "-q", "-b", "topic"]);
    let tip = t.commit_file("b.txt", "b\n", "topic work");
    t.git(&["checkout", "-q", "main"]);
    t.git(&["branch", "copy"]);

    t.repo
        .merge("topic", &merge_opts(FastForward::Only))
        .unwrap();
    assert_eq!(t.head(), tip);

    t.git(&["checkout", "-q", "copy"]);
    t.repo
        .merge(
            "topic",
            &MergeOptions {
                message: Some(s("Merge it")),
                ..merge_opts(FastForward::Never)
            },
        )
        .unwrap();
    assert_eq!(
        t.git(&["rev-list", "--parents", "-n1", "HEAD"])
            .split(' ')
            .count(),
        3
    );
    assert_eq!(t.subjects("-1"), ["Merge it"]);

    // ff-only on diverged history fails.
    t.git(&["checkout", "-q", "-b", "diverged", "main~1"]);
    t.commit_file("c.txt", "c\n", "diverge");
    assert!(
        t.repo
            .merge("topic", &merge_opts(FastForward::Only))
            .is_err()
    );
    assert_eq!(state(&t), RepoState::Clean);
}

#[test]
fn merge_squash_and_no_commit() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    t.git(&["checkout", "-q", "-b", "topic"]);
    t.commit_file("b.txt", "b\n", "t1");
    t.commit_file("c.txt", "c\n", "t2");
    t.git(&["checkout", "-q", "main"]);
    t.commit_file("m.txt", "m\n", "main work");
    let head = t.head();
    t.repo
        .merge(
            "topic",
            &MergeOptions {
                squash: true,
                commit: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(t.head(), head);
    assert_eq!(t.status(), "A  b.txt\nA  c.txt");
    assert_eq!(state(&t), RepoState::Clean);
    t.repo
        .commit(&CommitOptions {
            message: s("squashed"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.git(&["rev-list", "--parents", "-n1", "HEAD"])
            .split(' ')
            .count(),
        2
    );

    t.git(&["reset", "-q", "--hard", &head]);
    t.repo
        .merge(
            "topic",
            &MergeOptions {
                commit: false,
                ff: FastForward::Never,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(t.head(), head);
    assert_eq!(state(&t), RepoState::Merging);
    t.repo.continue_op(RepoState::Merging).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(
        t.git(&["rev-list", "--parents", "-n1", "HEAD"])
            .split(' ')
            .count(),
        3
    );
}

#[test]
fn merge_conflict_resolve_continue() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    t.commit_file("other.txt", "o\n", "unrelated");
    let text = t
        .repo
        .merge("topic", &merge_opts(FastForward::Allow))
        .unwrap();
    assert!(text.contains("CONFLICT"), "{text}");
    assert_eq!(state(&t), RepoState::Merging);
    // Continuing with unresolved conflicts is an error.
    assert!(t.repo.continue_op(RepoState::Merging).is_err());
    t.repo.resolve_with(&[s("c.txt")], Side::Theirs).unwrap();
    assert_eq!(t.read("c.txt"), "topic\n");
    assert_eq!(t.status(), "M  c.txt");
    t.repo.continue_op(RepoState::Merging).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(
        t.git(&["rev-list", "--parents", "-n1", "HEAD"])
            .split(' ')
            .count(),
        3
    );
    assert_eq!(t.git(&["show", "HEAD:c.txt"]), "topic");
}

#[test]
fn merge_conflict_ours_mark_resolved_abort() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    let head = t.head();
    t.repo
        .merge("topic", &merge_opts(FastForward::Allow))
        .unwrap();
    t.repo.resolve_with(&[s("c.txt")], Side::Ours).unwrap();
    assert_eq!(t.read("c.txt"), "main\n");
    t.write("c.txt", "hand merged\n");
    t.repo.mark_resolved(&[s("c.txt")]).unwrap();
    assert_eq!(t.index_content("c.txt"), b"hand merged\n");
    t.repo.abort_op(RepoState::Merging).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.head(), head);
    assert_eq!(t.read("c.txt"), "main\n");
    assert!(t.repo.abort_op(RepoState::Clean).is_err());
    assert!(t.repo.continue_op(RepoState::Clean).is_err());
    assert!(t.repo.skip_op(RepoState::Merging).is_err());
}

// ------------------------------------------------------------------ cherry-pick, revert, reset

#[test]
fn cherry_pick_clean_and_conflict() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    t.git(&["checkout", "-q", "topic"]);
    let extra = t.commit_file("x.txt", "x\n", "extra");
    t.git(&["checkout", "-q", "main"]);
    t.repo
        .cherry_pick(std::slice::from_ref(&extra), false)
        .unwrap();
    assert_eq!(t.subjects("-1"), ["extra"]);
    assert_eq!(t.read("x.txt"), "x\n");

    let text = t.repo.cherry_pick(&[t.rev("topic~1")], false).unwrap();
    assert!(text.to_lowercase().contains("conflict"), "{text}");
    assert_eq!(state(&t), RepoState::CherryPicking);
    t.repo.save_resolution("c.txt", b"merged\n").unwrap();
    t.repo.continue_op(RepoState::CherryPicking).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.subjects("-1"), ["topic change"]);
    assert_eq!(t.git(&["show", "HEAD:c.txt"]), "merged");

    // --no-commit leaves the change staged.
    t.git(&["reset", "-q", "--hard", "HEAD~2"]);
    t.repo.cherry_pick(&[extra], true).unwrap();
    assert_eq!(t.status(), "A  x.txt");
}

#[test]
fn cherry_pick_conflict_skip_and_abort() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    let head = t.head();
    t.repo.cherry_pick(&[s("topic")], false).unwrap();
    t.repo.skip_op(RepoState::CherryPicking).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.head(), head);
    t.repo.cherry_pick(&[s("topic")], false).unwrap();
    t.repo.abort_op(RepoState::CherryPicking).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.head(), head);
    assert!(t.repo.cherry_pick(&[s("no-such-rev")], false).is_err());
}

#[test]
fn revert_clean_and_conflict() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    let add = t.commit_file("b.txt", "b\n", "add b");
    t.repo.revert(&[add], false).unwrap();
    assert_eq!(t.subjects("-1"), ["Revert \"add b\""]);
    assert!(!t.exists("b.txt"));

    let modify = t.commit_file("a.txt", "a2\n", "modify a");
    t.commit_file("a.txt", "a3\n", "modify a again");
    let text = t.repo.revert(&[modify], false).unwrap();
    assert!(text.to_lowercase().contains("conflict"), "{text}");
    assert_eq!(state(&t), RepoState::Reverting);
    t.repo.abort_op(RepoState::Reverting).unwrap();
    assert_eq!(state(&t), RepoState::Clean);

    t.repo.revert(&[t.rev("HEAD")], true).unwrap();
    assert_eq!(t.status(), "M  a.txt");
    assert_eq!(t.read("a.txt"), "a2\n");
}

#[test]
fn revert_conflict_continue() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    let modify = t.commit_file("a.txt", "a2\n", "modify a");
    t.commit_file("a.txt", "a3\n", "modify a again");
    t.repo.revert(&[modify], false).unwrap();
    t.repo.save_resolution("a.txt", b"resolved\n").unwrap();
    t.repo.continue_op(RepoState::Reverting).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.subjects("-1"), ["Revert \"modify a\""]);
}

#[test]
fn reset_modes() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1\n", "one");
    let c2 = t.commit_file("a.txt", "2\n", "two");
    t.repo.reset("HEAD~1", ResetMode::Soft).unwrap();
    assert_eq!(t.status(), "M  a.txt");
    t.repo.reset(&c2, ResetMode::Hard).unwrap();
    t.repo.reset("HEAD~1", ResetMode::Mixed).unwrap();
    assert_eq!(t.status(), " M a.txt");
    assert_eq!(t.read("a.txt"), "2\n");
    t.repo.reset("HEAD", ResetMode::Hard).unwrap();
    assert_eq!(t.status(), "");
    assert_eq!(t.read("a.txt"), "1\n");
}

// ------------------------------------------------------------------ stash

#[test]
fn stash_push_apply_pop_drop() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.write("a.txt", "a2\n");
    t.repo
        .stash_push("my message", &StashOptions::default(), &[])
        .unwrap();
    assert_eq!(t.status(), "");
    let r = refs::load(&t.repo).unwrap();
    assert_eq!(r.stashes.len(), 1);
    assert!(r.stashes[0].message.contains("my message"));

    t.repo.stash_apply(0, false, false).unwrap();
    assert_eq!(t.read("a.txt"), "a2\n");
    assert_eq!(refs::load(&t.repo).unwrap().stashes.len(), 1);
    t.git(&["checkout", "--", "a.txt"]);
    t.repo.stash_apply(0, true, false).unwrap();
    assert_eq!(t.read("a.txt"), "a2\n");
    assert!(refs::load(&t.repo).unwrap().stashes.is_empty());

    t.repo
        .stash_push("", &StashOptions::default(), &[])
        .unwrap();
    t.repo.stash_drop(0).unwrap();
    assert!(refs::load(&t.repo).unwrap().stashes.is_empty());
    assert!(t.repo.stash_drop(0).is_err());
}

#[test]
fn stash_keep_index_and_restore_index() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.commit_file("b.txt", "b\n", "init2");
    t.write("a.txt", "staged\n");
    t.git(&["add", "a.txt"]);
    t.write("b.txt", "unstaged\n");
    t.repo
        .stash_push(
            "ki",
            &StashOptions {
                keep_index: true,
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(t.status(), "M  a.txt");
    t.git(&["reset", "-q", "--hard"]);
    t.repo.stash_apply(0, true, true).unwrap();
    assert_eq!(t.status(), "M  a.txt\n M b.txt");

    // Without --index the staged change comes back unstaged.
    t.repo
        .stash_push("again", &StashOptions::default(), &[])
        .unwrap();
    t.repo.stash_apply(0, true, false).unwrap();
    assert_eq!(t.status(), " M a.txt\n M b.txt");
}

#[test]
fn stash_untracked_and_paths() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.commit_file("b.txt", "b\n", "init2");
    t.write("u.txt", "u\n");
    t.write("a.txt", "a2\n");
    t.repo
        .stash_push(
            "with untracked",
            &StashOptions {
                include_untracked: true,
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    assert!(!t.exists("u.txt"));
    assert_eq!(t.status(), "");
    t.repo.stash_apply(0, true, false).unwrap();
    assert!(t.exists("u.txt"));

    t.write("b.txt", "b2\n");
    t.repo
        .stash_push("only a", &StashOptions::default(), &[s("a.txt")])
        .unwrap();
    assert_eq!(t.status(), " M b.txt\n?? u.txt");
    assert_eq!(t.read("a.txt"), "a\n");
    t.repo.stash_apply(0, true, false).unwrap();
    assert_eq!(t.read("a.txt"), "a2\n");

    t.git(&["add", "b.txt"]);
    t.repo
        .stash_push(
            "staged only",
            &StashOptions {
                staged_only: true,
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(t.status(), " M a.txt\n?? u.txt");
}

#[test]
fn stash_apply_conflict_and_branch() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.write("a.txt", "stashed\n");
    t.repo
        .stash_push("s", &StashOptions::default(), &[])
        .unwrap();
    let base = t.head();
    t.commit_file("a.txt", "committed\n", "conflicting");
    let text = t.repo.stash_apply(0, true, false).unwrap();
    assert!(text.to_lowercase().contains("conflict"), "{text}");
    // A conflicting pop keeps the stash.
    assert_eq!(refs::load(&t.repo).unwrap().stashes.len(), 1);
    t.git(&["reset", "-q", "--hard"]);

    t.repo.stash_branch(0, "from-stash").unwrap();
    assert_eq!(t.git(&["branch", "--show-current"]), "from-stash");
    assert_eq!(t.head(), base);
    assert_eq!(t.read("a.txt"), "stashed\n");
    assert!(refs::load(&t.repo).unwrap().stashes.is_empty());
}

// ------------------------------------------------------------------ misc

#[test]
fn ignore_appends() {
    let t = TestRepo::new();
    t.repo.ignore(&[s("*.log")]).unwrap();
    assert_eq!(t.read(".gitignore"), "*.log\n");
    t.write(".gitignore", "*.log");
    t.repo.ignore(&[s("build/"), s("tmp")]).unwrap();
    assert_eq!(t.read(".gitignore"), "*.log\nbuild/\ntmp\n");
    t.write("x.log", "x");
    assert_eq!(t.status(), "?? .gitignore");
}

#[test]
fn config_local() {
    let t = TestRepo::new();
    assert_eq!(t.repo.config_get("gitkwaken.test-key"), None);
    t.repo
        .config_set("gitkwaken.test-key", "some value", false)
        .unwrap();
    assert_eq!(
        t.repo.config_get("gitkwaken.test-key").as_deref(),
        Some("some value")
    );
    assert_eq!(
        t.git(&["config", "--local", "gitkwaken.test-key"]),
        "some value"
    );
    assert_eq!(t.repo.config_get("user.name").as_deref(), Some("Test User"));
}

#[test]
fn format_and_apply_patch() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    let c1 = t.commit_file("a.txt", "a\nb\n", "add b");
    let c2 = t.commit_file("n.txt", "n\n", "add n");
    let patch = t.repo.format_patch(&[c1, c2]).unwrap();
    let text = String::from_utf8(patch.clone()).unwrap();
    assert!(
        text.contains("Subject: [PATCH] add b") && text.contains("Subject: [PATCH] add n"),
        "{text}"
    );

    t.git(&["reset", "-q", "--hard", "HEAD~2"]);
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.patch");
    std::fs::write(&file, &patch).unwrap();
    t.repo.apply_patch(&file, false).unwrap();
    assert_eq!(t.read("a.txt"), "a\nb\n");
    assert_eq!(t.read("n.txt"), "n\n");

    t.git(&["reset", "-q", "--hard"]);
    t.git(&["clean", "-fdq"]);
    t.repo.apply_patch(&file, true).unwrap();
    assert_eq!(t.index_content("a.txt"), b"a\nb\n");
    assert_eq!(t.read("a.txt"), "a\n", "worktree untouched with --cached");

    // Conflicting content: the three-way fallback leaves conflicts and reports failure.
    t.git(&["reset", "-q", "--hard"]);
    t.commit_file("a.txt", "a\nsomething else\n", "diverge");
    assert!(t.repo.apply_patch(&file, false).is_err());
    let garbage = out.path().join("garbage.patch");
    std::fs::write(&garbage, "not a patch").unwrap();
    assert!(t.repo.apply_patch(&garbage, false).is_err());
}

#[test]
fn archive_formats() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    let out = tempfile::tempdir().unwrap();
    let zip = out.path().join("out.zip");
    t.repo.archive("HEAD", &zip).unwrap();
    assert_eq!(&std::fs::read(&zip).unwrap()[..2], b"PK");
    let tar = out.path().join("out.tar");
    t.repo.archive("main", &tar).unwrap();
    assert!(std::fs::metadata(&tar).unwrap().len() >= 1024);
    assert!(t.repo.archive("nope", &out.path().join("x.zip")).is_err());
}

#[test]
fn stage_treats_paths_literally() {
    // As a glob, "a[b].txt" would also match "ab.txt".
    let t = TestRepo::new();
    t.write("a[b].txt", "1\n");
    t.write("ab.txt", "2\n");
    t.repo.stage(&["a[b].txt".to_string()]).unwrap();
    let staged = t.git(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "a[b].txt");
}

#[test]
fn option_like_names_are_rejected() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    assert!(t.repo.create_branch("--force", None, false).is_err());
    assert!(t.repo.checkout("-q").is_err());
    assert!(t.repo.add_remote("origin", "--upload-pack=evil").is_err());
}

#[test]
fn stage_single_added_line_after_blank_line() {
    let t = TestRepo::new();
    t.commit_file("README.md", "# Demo\n\nA demo repository.\nUsage: run it.\n", "init");
    t.write("README.md", "# Demo\n\nA demo repository.\nUsage: run it.\n\nMore docs.\n");
    // select only new line 6 ("More docs.")
    t.repo
        .stage_lines("README.md", false, &|l: &gk_diff::Line| l.kind == gk_diff::LineKind::Added && l.new_no == 6)
        .unwrap();
    let cached = t.git(&["diff", "--cached", "--", "README.md"]);
    assert!(cached.contains("+More docs."), "{cached}");
    assert!(!cached.contains("\n+\n"), "blank line must stay unstaged:\n{cached}");
}
