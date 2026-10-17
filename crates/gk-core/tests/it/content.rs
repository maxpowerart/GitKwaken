use crate::common::*;
use gk_core::content::{self, ChangeKind, DiffTarget};

fn unstaged(t: &TestRepo, path: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    content::load_sides(&t.repo, &DiffTarget::Unstaged { path: s(path) }).unwrap()
}

fn staged(t: &TestRepo, path: &str, old: Option<&str>) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    content::load_sides(
        &t.repo,
        &DiffTarget::Staged {
            path: s(path),
            old_path: old.map(s),
        },
    )
    .unwrap()
}

fn b(v: &str) -> Option<Vec<u8>> {
    Some(v.as_bytes().to_vec())
}

#[test]
fn unstaged_sides() {
    let t = TestRepo::new();
    t.commit_file("m.txt", "old\n", "init");
    t.commit_file("d.txt", "gone\n", "init2");
    t.write("m.txt", "new\n");
    std::fs::remove_file(t.repo.abs("d.txt")).unwrap();
    t.write("dir/u.txt", "untracked\n");

    assert_eq!(unstaged(&t, "m.txt"), (b("old\n"), b("new\n")));
    assert_eq!(unstaged(&t, "d.txt"), (b("gone\n"), None));
    assert_eq!(unstaged(&t, "dir/u.txt"), (None, b("untracked\n")));
    assert_eq!(
        unstaged(&t, "dir"),
        (None, None),
        "directories have no content"
    );

    // Intent-to-add: index holds the empty blob.
    t.git(&["add", "-N", "dir/u.txt"]);
    assert_eq!(unstaged(&t, "dir/u.txt"), (b(""), b("untracked\n")));
}

#[test]
fn unstaged_conflicted_uses_ours() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    assert!(!t.git_raw(&["merge", "topic"]).success());
    let (old, new) = unstaged(&t, "c.txt");
    assert_eq!(old, b("main\n"));
    let new = String::from_utf8(new.unwrap()).unwrap();
    assert!(new.contains("<<<<<<<") && new.contains("topic"), "{new}");
    assert_eq!(
        content::index_blob(&t.repo.gix(), "c.txt", 3).unwrap(),
        b("topic\n")
    );
    assert_eq!(
        content::index_blob(&t.repo.gix(), "c.txt", 1).unwrap(),
        b("base\n")
    );
}

#[test]
fn staged_sides() {
    let t = TestRepo::new();
    t.commit_file("m.txt", "old\n", "init");
    t.commit_file("r_old.txt", "rename me\nline 2\nline 3\n", "init2");
    t.write("m.txt", "staged\n");
    t.write("a.txt", "added\n");
    t.git(&["add", "m.txt", "a.txt"]);
    t.git(&["mv", "r_old.txt", "r_new.txt"]);
    t.write("m.txt", "worktree\n");

    assert_eq!(staged(&t, "m.txt", None), (b("old\n"), b("staged\n")));
    assert_eq!(staged(&t, "a.txt", None), (None, b("added\n")));
    assert_eq!(
        staged(&t, "r_new.txt", Some("r_old.txt")),
        (
            b("rename me\nline 2\nline 3\n"),
            b("rename me\nline 2\nline 3\n")
        )
    );
    assert_eq!(staged(&t, "r_new.txt", None).0, None);
}

#[test]
fn staged_in_unborn_repo() {
    let t = TestRepo::new();
    t.write("a.txt", "x\n");
    t.git(&["add", "a.txt"]);
    assert_eq!(staged(&t, "a.txt", None), (None, b("x\n")));
    assert_eq!(unstaged(&t, "a.txt"), (b("x\n"), b("x\n")));
}

#[test]
fn blobs_and_tree_changes() {
    let t = TestRepo::new();
    t.write("keep.txt", "k\n");
    t.write("mod.txt", "v1\n");
    t.write("del.txt", "d\n");
    t.write(
        "ren_a.txt",
        "some content\nthat is long enough\nfor rename detection\n",
    );
    t.git(&["add", "."]);
    let c1 = t.commit("one");
    t.write("mod.txt", "v2\n");
    t.write("sub/add.txt", "new\n");
    std::fs::remove_file(t.repo.abs("del.txt")).unwrap();
    t.git(&["rm", "-q", "--cached", "del.txt"]);
    t.git(&["mv", "ren_a.txt", "ren_b.txt"]);
    t.git(&["add", "."]);
    let c2 = t.commit("two");

    let g = t.repo.gix();
    let tree = |c: &str| g.find_commit(t.oid(c)).unwrap().tree_id().unwrap().detach();
    let files = content::tree_changes(&g, Some(tree(&c1)), tree(&c2)).unwrap();
    let summary: Vec<(String, ChangeKind, Option<String>)> = files
        .iter()
        .map(|f| (f.path.clone(), f.kind, f.old_path.clone()))
        .collect();
    assert_eq!(
        summary,
        vec![
            (s("del.txt"), ChangeKind::Deleted, None),
            (s("mod.txt"), ChangeKind::Modified, None),
            (s("ren_b.txt"), ChangeKind::Renamed, Some(s("ren_a.txt"))),
            (s("sub/add.txt"), ChangeKind::Added, None),
        ]
    );
    for f in &files {
        let (o, n) = content::load_sides(
            &t.repo,
            &DiffTarget::Blobs {
                path: f.path.clone(),
                old: f.old_id,
                new: f.new_id,
            },
        )
        .unwrap();
        match f.kind {
            ChangeKind::Added => assert!(o.is_none() && n == b("new\n")),
            ChangeKind::Deleted => assert!(o == b("d\n") && n.is_none()),
            ChangeKind::Modified => assert!(o == b("v1\n") && n == b("v2\n")),
            _ => assert_eq!(o, n),
        }
    }

    // Root commit: everything added.
    let root = content::tree_changes(&g, None, tree(&c1)).unwrap();
    assert_eq!(root.len(), 4);
    assert!(root.iter().all(|f| f.kind == ChangeKind::Added));

    assert_eq!(
        content::compare_commits(&t.repo, t.oid(&c1), t.oid(&c2)).unwrap(),
        files
    );
    let back = content::compare_commits(&t.repo, t.oid(&c2), t.oid(&c1)).unwrap();
    assert_eq!(back.len(), 4);
    assert!(
        back.iter()
            .any(|f| f.path == "del.txt" && f.kind == ChangeKind::Added)
    );
}

#[test]
fn commit_details_root_normal_merge() {
    let t = TestRepo::new();
    let root = t.commit_file("a.txt", "a\n", "root commit\n\nwith body");
    t.git(&["checkout", "-q", "-b", "side"]);
    t.commit_file("b.txt", "b\n", "side");
    t.git(&["checkout", "-q", "main"]);
    let normal = t.commit_file("a.txt", "a2\n", "normal");
    t.git_dated(&["merge", "-q", "--no-ff", "-m", "the merge", "side"]);
    let merge = t.head();

    let d = content::commit_details(&t.repo, t.oid(&root)).unwrap();
    assert!(d.parents.is_empty());
    assert_eq!(d.message.trim_end(), "root commit\n\nwith body");
    assert_eq!(d.author, "Test User");
    assert_eq!(d.author_email, "test@example.com");
    assert_eq!(d.committer, "Test User");
    assert!(d.author_time > 0 && d.committer_time > 0);
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].kind, ChangeKind::Added);

    let d = content::commit_details(&t.repo, t.oid(&normal)).unwrap();
    assert_eq!(d.parents, vec![t.oid(&root)]);
    assert_eq!(d.files.len(), 1);
    assert_eq!(
        (d.files[0].path.as_str(), d.files[0].kind),
        ("a.txt", ChangeKind::Modified)
    );

    let d = content::commit_details(&t.repo, t.oid(&merge)).unwrap();
    assert_eq!(d.parents, vec![t.oid(&normal), t.oid("side")]);
    assert_eq!(d.tree, t.oid("HEAD^{tree}"));
    // Against the first parent: only what the side branch brought in.
    assert_eq!(d.files.len(), 1);
    assert_eq!(
        (d.files[0].path.as_str(), d.files[0].kind),
        ("b.txt", ChangeKind::Added)
    );
}

#[test]
fn worktree_roundtrip_with_autocrlf() {
    let t = TestRepo::with_autocrlf(true);
    t.write("w.txt", "a\r\nb\r\n");
    t.git(&["add", "w.txt"]);
    t.commit("crlf");
    assert_eq!(t.index_content("w.txt"), b"a\nb\n");
    let g = t.repo.gix();
    assert_eq!(
        content::worktree_clean(&t.repo, &g, "w.txt").unwrap(),
        b("a\nb\n")
    );
    content::write_worktree(&t.repo, &g, "w.txt", b"x\ny\n").unwrap();
    assert_eq!(t.read("w.txt"), "x\r\ny\r\n");
}
