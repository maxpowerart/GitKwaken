//! Discard backups: every discard is saved under `refs/gitkwaken/discarded`
//! and can be restored.

use crate::common::*;
use gk_cli::CommandLog;

fn s(x: &str) -> String {
    x.to_string()
}

#[test]
fn discard_then_restore_modified_deleted_and_untracked() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    t.commit_file("b.txt", "b\n", "b");
    t.commit_file("c.txt", "c\n", "c");
    t.write("a.txt", "a changed\r\n");
    std::fs::remove_file(t.repo.abs("b.txt")).unwrap();
    t.write("u.txt", "untracked\n");
    t.write("udir/x/y.bin", [0u8, 1, 2, 13, 10]);
    let before = t.status();

    t.repo
        .discard(
            &[s("a.txt"), s("b.txt"), s("c.txt")],
            &[s("u.txt"), s("udir/")],
        )
        .unwrap();
    assert_eq!(t.status(), "");
    assert!(!t.exists("udir"));

    let list = t.repo.discard_backups(10).unwrap();
    assert_eq!(list.len(), 1);
    let b = &list[0];
    assert_eq!(b.files, vec!["a.txt", "c.txt", "u.txt", "udir/x/y.bin"]);
    assert_eq!(b.deleted, vec!["b.txt"]);
    assert!(
        b.message
            .starts_with("gitkwaken discard: 5 paths (a.txt, b.txt, c.txt, …)"),
        "{}",
        b.message
    );
    assert!(b.time > 0);
    assert_eq!(t.git(&["rev-parse", gk_core::DISCARD_REF]), b.id);
    // Hidden: not a branch, not in the history.
    assert_eq!(t.git(&["branch", "--list"]), "* main");

    let out = t.repo.restore_discarded(&b.id, false).unwrap();
    assert_eq!((out.restored, out.unchanged), (4, 1), "{out:?}");
    assert_eq!(out.overwritten_backup, None);
    assert_eq!(t.read("a.txt"), "a changed\r\n");
    assert!(!t.exists("b.txt"));
    assert_eq!(t.read("u.txt"), "untracked\n");
    assert_eq!(t.read_bytes("udir/x/y.bin"), vec![0, 1, 2, 13, 10]);
    assert_eq!(t.status(), before);

    // Restoring again is a no-op.
    let again = t.repo.restore_discarded(&b.id, false).unwrap();
    assert_eq!((again.restored, again.unchanged), (0, 5));
}

#[test]
fn restore_refuses_to_overwrite_local_changes() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    t.write("a.txt", "mine 1\n");
    t.write("u.txt", "u 1\n");
    t.repo.discard(&[s("a.txt")], &[s("u.txt")]).unwrap();
    let first = t.repo.discard_backups(10).unwrap()[0].id.clone();

    // New local work on both paths after the discard.
    t.write("a.txt", "mine 2\n");
    t.write("u.txt", "u 2\n");
    let err = t.repo.restore_discarded(&first, false).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("2 files changed") && msg.contains("a.txt") && msg.contains("u.txt"),
        "{msg}"
    );
    assert_eq!(t.read("a.txt"), "mine 2\n");
    assert_eq!(t.read("u.txt"), "u 2\n");

    // Overwriting keeps the replaced content as a backup of its own.
    let out = t.repo.restore_discarded(&first, true).unwrap();
    assert_eq!(t.read("a.txt"), "mine 1\n");
    assert_eq!(t.read("u.txt"), "u 1\n");
    let second = out
        .overwritten_backup
        .expect("backup of the overwritten files");
    let list = t.repo.discard_backups(10).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, second);
    assert!(
        list[0].message.starts_with("gitkwaken restore: 2 paths"),
        "{}",
        list[0].message
    );
    t.repo.restore_discarded(&second, true).unwrap();
    assert_eq!(t.read("a.txt"), "mine 2\n");
    assert_eq!(t.read("u.txt"), "u 2\n");
}

#[test]
fn discard_all_saves_staged_content_too() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    t.write("a.txt", "staged\n");
    t.write("n.txt", "new\n");
    t.git(&["add", "a.txt", "n.txt"]);
    t.write("a.txt", "worktree\n");
    t.repo
        .discard_changes(&[s("a.txt"), s("n.txt")], &[s("a.txt")], &[s("n.txt")])
        .unwrap();
    assert_eq!(t.status(), "");
    let b = &t.repo.discard_backups(1).unwrap()[0];
    assert_eq!(
        t.git(&["show", &format!("{}:files/a.txt", b.id)]),
        "worktree"
    );
    assert_eq!(t.git(&["show", &format!("{}:index/a.txt", b.id)]), "staged");
    assert_eq!(t.git(&["show", &format!("{}:index/n.txt", b.id)]), "new");
    t.repo.restore_discarded(&b.id, false).unwrap();
    assert_eq!(t.read("a.txt"), "worktree\n");
    assert_eq!(t.read("n.txt"), "new\n");
    // The worktree comes back; the index stays as the discard left it.
    assert_eq!(t.status(), " M a.txt\n?? n.txt");
}

#[test]
fn discard_lines_is_backed_up() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "a\nb\n", "init");
    t.write("f.txt", "a\nB\nc\n");
    t.repo.discard_lines("f.txt", &|_| true).unwrap();
    assert_eq!(t.read("f.txt"), "a\nb\n");
    let b = &t.repo.discard_backups(1).unwrap()[0];
    assert_eq!(b.files, vec!["f.txt"]);
    t.repo.restore_discarded(&b.id, false).unwrap();
    assert_eq!(t.read("f.txt"), "a\nB\nc\n");
}

#[test]
fn discard_of_many_files_and_restore() {
    let t = TestRepo::new();
    for i in 0..200 {
        t.write(&format!("t/{}/f{i}.txt", i % 10), format!("{i}\n"));
    }
    t.git(&["add", "-A"]);
    t.commit("init");
    let mut tracked = Vec::new();
    for i in 0..200 {
        let p = format!("t/{}/f{i}.txt", i % 10);
        if i % 4 == 0 {
            std::fs::remove_file(t.repo.abs(&p)).unwrap();
        } else {
            t.write(&p, format!("{i} changed\n"));
        }
        tracked.push(p);
    }
    let mut untracked = vec![s("gen/")];
    for i in 0..300 {
        t.write(&format!("gen/{}/g{i}.out", i % 7), format!("gen {i}\n"));
    }
    for i in 0..100 {
        let p = format!("top{i}.tmp");
        t.write(&p, format!("top {i}\n"));
        untracked.push(p);
    }
    let before = t.status();
    let start = std::time::Instant::now();
    t.repo.discard(&tracked, &untracked).unwrap();
    eprintln!("discard of 600 paths with backup: {:?}", start.elapsed());
    assert_eq!(t.status(), "");

    let b = &t.repo.discard_backups(10).unwrap()[0];
    assert_eq!(b.files.len(), 150 + 300 + 100);
    assert_eq!(b.deleted.len(), 50);
    let out = t.repo.restore_discarded(&b.id, false).unwrap();
    assert_eq!(out.restored, 600);
    assert_eq!(t.status(), before);
    // The blobs went into a pack: the repository must still be sound.
    t.git(&["fsck", "--no-dangling"]);
}

#[test]
fn backups_keep_a_reflog_even_without_log_all_ref_updates() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    t.git(&["config", "core.logAllRefUpdates", "false"]);
    let repo = gk_core::Repo::open(t.repo.workdir(), CommandLog::default()).unwrap();
    for i in 0..3 {
        t.write("a.txt", format!("change {i}\n"));
        repo.discard(&[s("a.txt")], &[]).unwrap();
    }
    let list = repo.discard_backups(10).unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(
        t.git(&["show", &format!("{}:files/a.txt", list[2].id)]),
        "change 0"
    );
}

#[test]
fn discard_refuses_to_delete_a_nested_repository() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    t.write("a.txt", "keep me\n");
    t.write("nested/file.txt", "x\n");
    git_at(&t.repo.abs("nested")).run(&["init", "-q"]).unwrap();
    let err = t.repo.discard(&[s("a.txt")], &[s("nested/")]).unwrap_err();
    assert!(format!("{err:#}").contains("git repository"), "{err:#}");
    // Nothing was discarded.
    assert_eq!(t.read("a.txt"), "keep me\n");
    assert!(t.exists("nested/file.txt"));
    assert!(t.repo.discard_backups(10).unwrap().is_empty());
}

#[test]
fn single_file_discard_overhead() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "a");
    // Warm up (first gix open, first reflog).
    t.write("a.txt", "warm\n");
    t.repo.discard(&[s("a.txt")], &[]).unwrap();
    let mut best = std::time::Duration::MAX;
    for i in 0..5 {
        t.write("a.txt", format!("x {i}\n"));
        let start = std::time::Instant::now();
        t.repo.backup_discard(&[s("a.txt")], &[], &[]).unwrap();
        best = best.min(start.elapsed());
    }
    eprintln!("backup of one file: {best:?}");
    // Generous bound for slow CI machines; measured ~1-5 ms locally.
    assert!(best < std::time::Duration::from_millis(500), "{best:?}");
}

#[test]
fn discard_of_a_submodule_path_is_not_blocked() {
    let t = TestRepo::new();
    let head = t.commit_file("a.txt", "a\n", "a");
    // A gitlink entry with a nested repository at its path.
    t.write("sub/inner.txt", "x\n");
    git_at(&t.repo.abs("sub")).run(&["init", "-q"]).unwrap();
    t.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},sub"),
    ]);
    t.commit("add gitlink");
    t.write("a.txt", "changed\n");
    t.repo.discard(&[s("a.txt"), s("sub")], &[]).unwrap();
    assert_eq!(t.read("a.txt"), "a\n");
    assert!(t.exists("sub/inner.txt"));
    assert_eq!(t.repo.discard_backups(10).unwrap()[0].files, vec!["a.txt"]);
}
