//! Rename similarity threshold (settings: diff.rename_similarity) in status
//! (both engines) and in commit file lists. Its own test binary: the
//! threshold is process-wide and would race with the `it` tests.

#[path = "it/common.rs"]
#[allow(dead_code)]
mod common;

use common::TestRepo;
use gk_core::content::{self, ChangeKind};
use gk_status::Untracked;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Ten lines of 40 bytes each.
fn lines(changed: &[usize]) -> String {
    (0..10)
        .map(|i| {
            if changed.contains(&i) {
                format!("replaced line {i:02} with other content.\n")
            } else {
                format!("original line {i:02} of the moved file..\n")
            }
        })
        .collect()
}

/// Staged status as `code:path<-orig`, sorted, from both engines.
fn staged(t: &TestRepo) -> [Vec<String>; 2] {
    let f = |s: gk_status::Status| {
        let mut v: Vec<String> = s
            .staged
            .iter()
            .map(|e| match &e.orig_path {
                Some(o) => format!("{}:{}<-{}", e.kind.code(), e.path, o),
                None => format!("{}:{}", e.kind.code(), e.path),
            })
            .collect();
        v.sort();
        v
    };
    let cli = gk_status::compute_cli(t.repo.git(), &[], Untracked::All).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let gix = gk_status::compute_gix(t.repo.workdir(), &[], Untracked::All, &stop).unwrap();
    [f(cli), f(gix)]
}

fn commit_files(t: &TestRepo, a: &str, b: &str) -> Vec<(String, ChangeKind, Option<String>)> {
    // Opened afresh so config changes made through the CLI are seen.
    let g = gix::open(t.repo.workdir()).unwrap();
    let tree = |c: &str| g.find_commit(t.oid(c)).unwrap().tree_id().unwrap().detach();
    content::tree_changes(&g, Some(tree(a)), tree(b))
        .unwrap()
        .into_iter()
        .map(|f| (f.path, f.kind, f.old_path))
        .collect()
}

#[test]
fn rename_similarity_threshold() {
    let t = TestRepo::new();
    let c1 = t.commit_file("old.txt", &lines(&[]), "add");
    t.git(&["mv", "old.txt", "new.txt"]);
    // 4 of 10 equally long lines replaced: 60% similar.
    t.write("new.txt", lines(&[1, 4, 6, 9]));
    t.git(&["add", "new.txt"]);

    let renamed = vec!["R:new.txt<-old.txt".to_string()];
    let split = vec!["A:new.txt".to_string(), "D:old.txt".to_string()];

    content::set_rename_similarity(50);
    assert_eq!(gk_status::rename_similarity(), 50);
    assert_eq!(staged(&t), [renamed.clone(), renamed.clone()]);
    content::set_rename_similarity(80);
    assert_eq!(staged(&t), [split.clone(), split.clone()]);

    let c2 = t.commit("move and edit");
    content::set_rename_similarity(50);
    assert_eq!(
        commit_files(&t, &c1, &c2),
        [(
            "new.txt".to_string(),
            ChangeKind::Renamed,
            Some("old.txt".to_string())
        )]
    );
    content::set_rename_similarity(80);
    assert_eq!(
        commit_files(&t, &c1, &c2),
        [
            ("new.txt".to_string(), ChangeKind::Added, None),
            ("old.txt".to_string(), ChangeKind::Deleted, None),
        ]
    );

    // Turning detection off wins over the threshold; out-of-range values clamp.
    content::set_rename_similarity(50);
    content::set_detect_renames(false);
    assert_eq!(commit_files(&t, &c1, &c2).len(), 2);
    content::set_detect_renames(true);
    content::set_rename_similarity(0);
    assert_eq!(gk_status::rename_similarity(), 1);
    content::set_rename_similarity(200);
    assert_eq!(gk_status::rename_similarity(), 100);
    content::set_rename_similarity(50);

    // git's diff.renames=false still turns detection off in commit file lists.
    t.git(&["config", "diff.renames", "false"]);
    assert_eq!(commit_files(&t, &c1, &c2).len(), 2);
    t.git(&["config", "diff.renames", "true"]);
    assert_eq!(commit_files(&t, &c1, &c2).len(), 1);
}
