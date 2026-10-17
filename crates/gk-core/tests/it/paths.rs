//! Path edge cases: special characters, Unicode (Cyrillic, emoji, NFC vs NFD),
//! long paths (> 260 characters) and case-only renames. Every operation goes
//! through the gk-core / gk-status APIs the UI uses: stage, unstage, status
//! (both engines and the watcher fast path), diff, discard and stage_lines.

use crate::common::*;
use bstr::BString;
use gk_core::content::{DiffTarget, load_sides};
use gk_diff::{Line, LineKind, Options, diff};
use gk_status::{Engine, Kind, Status, Untracked};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// (path, code) pairs of one status side.
fn summary(entries: &[gk_status::Entry]) -> Vec<(String, char)> {
    entries
        .iter()
        .map(|e| (e.path.to_string(), e.kind.code()))
        .collect()
}

/// Status from both engines; asserts they agree and returns it.
fn status(t: &TestRepo) -> Status {
    let stop = Arc::new(AtomicBool::new(false));
    let run = |engine| {
        gk_status::compute(
            t.repo.workdir(),
            t.repo.git(),
            &[],
            &gk_status::Options {
                engine,
                untracked: Untracked::All,
            },
            &stop,
        )
        .unwrap()
    };
    let cli = run(Engine::Cli);
    let gix = run(Engine::Gix);
    assert_eq!(
        summary(&gix.staged),
        summary(&cli.staged),
        "staged: gix vs cli"
    );
    assert_eq!(
        summary(&gix.unstaged),
        summary(&cli.unstaged),
        "unstaged: gix vs cli"
    );
    cli
}

/// Fast (watcher) path for `paths` must report what the full scan reports.
/// An error or `Fallback` is fine (the session then runs a full scan), e.g.
/// before the first `git add` there is no index file yet.
fn fast_matches(t: &TestRepo, paths: &[&str], full: &Status) {
    let mut fast = gk_status::fast::FastStatus::new(t.repo.workdir()).unwrap();
    let scope: Vec<BString> = paths.iter().map(|p| BString::from(*p)).collect();
    if let Ok(gk_status::fast::Outcome::Updated(mut v)) = fast.refresh(&scope, Untracked::All) {
        v.sort_by(|a, b| a.path.cmp(&b.path));
        let mut expected: Vec<_> = full
            .unstaged
            .iter()
            .filter(|e| scope.iter().any(|s| s == &e.path))
            .cloned()
            .collect();
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(summary(&v), summary(&expected), "fast path vs full scan");
    }
}

fn kind_of(entries: &[gk_status::Entry], path: &str) -> Option<Kind> {
    entries
        .iter()
        .find(|e| e.path == path.as_bytes())
        .map(|e| e.kind)
}

fn unstaged_diff(t: &TestRepo, path: &str) -> gk_diff::FileDiff {
    let (old, new) = load_sides(
        &t.repo,
        &DiffTarget::Unstaged {
            path: path.to_string(),
        },
    )
    .unwrap();
    diff(
        old.as_deref().unwrap_or_default(),
        new.as_deref().unwrap_or_default(),
        Options::default(),
    )
}

fn staged_diff_of(t: &TestRepo, path: &str) -> gk_diff::FileDiff {
    let (old, new) = load_sides(
        &t.repo,
        &DiffTarget::Staged {
            path: path.to_string(),
            old_path: None,
        },
    )
    .unwrap();
    diff(
        old.as_deref().unwrap_or_default(),
        new.as_deref().unwrap_or_default(),
        Options::default(),
    )
}

fn added_line(n: u32) -> impl Fn(&Line) -> bool {
    move |l: &Line| l.kind == LineKind::Added && l.new_no == n
}

/// Runs the full cycle on each path: commit, modify, status, diff, stage_lines,
/// stage, unstage, discard; plus untracked add/discard of a sibling.
fn exercise(t: &TestRepo, paths: &[&str]) {
    for p in paths {
        t.write(p, "one\ntwo\n");
    }
    // Untracked: listed individually, staged through `stage`.
    let st = status(t);
    for p in paths {
        assert_eq!(kind_of(&st.unstaged, p), Some(Kind::Untracked), "{p}");
    }
    fast_matches(t, paths, &st);
    let owned: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    t.repo.stage(&owned).unwrap();
    let st = status(t);
    for p in paths {
        assert_eq!(kind_of(&st.staged, p), Some(Kind::Added), "{p}");
    }
    t.commit("add edge-case paths");
    assert!(status(t).is_clean(), "{:?}", status(t));

    for p in paths {
        t.write(p, "one\ntwo\nthree\nfour\n");
    }
    let st = status(t);
    for p in paths {
        assert_eq!(kind_of(&st.unstaged, p), Some(Kind::Modified), "{p}");
        let d = unstaged_diff(t, p);
        assert_eq!((d.additions, d.deletions), (2, 0), "{p}");
    }
    fast_matches(t, paths, &st);

    // Lines: stage only line 3 of each file.
    for p in paths {
        t.repo.stage_lines(p, false, &added_line(3)).unwrap();
        assert_eq!(t.index_content(p), b"one\ntwo\nthree\n", "{p}");
        let d = staged_diff_of(t, p);
        assert_eq!((d.additions, d.deletions), (1, 0), "{p}");
    }
    let st = status(t);
    for p in paths {
        assert_eq!(kind_of(&st.staged, p), Some(Kind::Modified), "{p}");
        assert_eq!(kind_of(&st.unstaged, p), Some(Kind::Modified), "{p}");
    }
    // Unstage the staged line again.
    for p in paths {
        t.repo.stage_lines(p, true, &|_| true).unwrap();
        assert_eq!(t.index_content(p), b"one\ntwo\n", "{p}");
    }

    // Whole files: stage, unstage.
    t.repo.stage(&owned).unwrap();
    let st = status(t);
    for p in paths {
        assert_eq!(kind_of(&st.staged, p), Some(Kind::Modified), "{p}");
        assert_eq!(kind_of(&st.unstaged, p), None, "{p}");
    }
    t.repo.unstage(&owned).unwrap();
    let st = status(t);
    assert!(st.staged.is_empty(), "{:?}", st.staged);
    assert_eq!(st.unstaged.len(), paths.len());

    // discard_lines on the first file, discard on all.
    t.repo.discard_lines(paths[0], &added_line(4)).unwrap();
    assert_eq!(t.read(paths[0]), "one\ntwo\nthree\n");
    t.repo.discard(&owned, &[]).unwrap();
    for p in paths {
        assert_eq!(t.read(p), "one\ntwo\n", "{p}");
    }
    assert!(status(t).is_clean());

    // Deletion staged through `stage`, then restored.
    std::fs::remove_file(t.repo.abs(paths[0])).unwrap();
    let st = status(t);
    assert_eq!(kind_of(&st.unstaged, paths[0]), Some(Kind::Deleted));
    t.repo.stage(&owned[..1]).unwrap();
    assert_eq!(kind_of(&status(t).staged, paths[0]), Some(Kind::Deleted));
    t.repo.unstage(&owned[..1]).unwrap();
    t.repo.discard(&owned[..1], &[]).unwrap();
    assert!(status(t).is_clean());

    // Untracked siblings discarded by path.
    let extra: Vec<String> = paths.iter().map(|p| format!("{p}.new")).collect();
    for p in &extra {
        t.write(p, "x");
    }
    assert_eq!(status(t).unstaged.len(), extra.len());
    t.repo.discard(&[], &extra).unwrap();
    assert!(status(t).is_clean());
}

#[test]
fn special_characters_in_names() {
    let names: Vec<String> = [
        "with space.txt",
        "br[ac]ket.txt",
        "star[*]glob?.txt",
        "hash#1.txt",
        "pct%20encoded.txt",
        "semi;colon,comma.txt",
        "quote'apos.txt",
        "-leading-dash.txt",
        ":colon-start.txt",
        "dir with space/nested [x]/f#%.txt",
        "(paren) {brace} @at !bang &amp $dollar ~tilde.txt",
    ]
    .iter()
    // `*`, `?` and `:` are not valid in Windows file names.
    .map(|p| {
        if cfg!(windows) {
            p.replace(['*', '?', ':'], "_")
        } else {
            p.to_string()
        }
    })
    .collect();
    let t = TestRepo::new();
    exercise(&t, &names.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn unicode_names() {
    let t = TestRepo::new();
    exercise(
        &t,
        &[
            "Привет мир.txt",
            "папка/файл.txt",
            "😀 smile.txt",
            "emoji 🚀/ракета 🔥.md",
            "日本語/ファイル.txt",
            "Ünïcödé ß.txt",
        ],
    );
}

/// "é" precomposed (NFC) and decomposed (NFD) are different byte sequences and,
/// on NTFS and ext4, different files. Both must round-trip unchanged.
#[cfg(not(target_os = "macos"))]
#[test]
fn nfc_and_nfd_names_are_distinct() {
    let nfc = "caf\u{e9}.txt";
    let nfd = "cafe\u{301}.txt";
    assert_ne!(nfc.as_bytes(), nfd.as_bytes());
    let t = TestRepo::new();
    t.git(&["config", "core.precomposeunicode", "false"]);
    exercise(&t, &[nfc, nfd]);
    let files = t.git(&["-c", "core.quotepath=false", "ls-files"]);
    assert!(files.lines().any(|l| l == nfc), "{files}");
    assert!(files.lines().any(|l| l == nfd), "{files}");
}

/// Repository-relative path of `depth` directories, each 40 characters.
fn long_path(depth: usize) -> String {
    let mut p = String::new();
    for i in 0..depth {
        p.push_str(&format!("dir{i:02}_{}/", "x".repeat(34)));
    }
    p.push_str("deep file [long].txt");
    p
}

#[test]
fn long_paths_over_260_chars() {
    let t = TestRepo::new();
    t.git(&["config", "core.longpaths", "true"]);
    let a = long_path(8);
    let b = format!("{}/second.txt", long_path(7).trim_end_matches(".txt"));
    let abs = t.repo.abs(&a);
    assert!(
        abs.to_string_lossy().len() > 300,
        "absolute path is {} chars",
        abs.to_string_lossy().len()
    );
    assert!(a.len() > 260, "relative path alone is over MAX_PATH");
    exercise(&t, &[&a, &b]);
    assert_eq!(t.git(&["ls-files"]).lines().count(), 2);
}

#[test]
fn many_edge_case_paths_staged_in_one_call() {
    // `stage` passes paths NUL-separated on stdin, so a large, mixed batch
    // must not trip over quoting or the 32k command-line limit.
    let t = TestRepo::new();
    let paths: Vec<String> = (0..2000)
        .map(|i| match i % 4 {
            0 => format!("sp ace/{i} file.txt"),
            1 => format!("кир/{i}.txt"),
            2 => format!("br[{i}]/x#{i}%.txt"),
            _ => format!("emoji😀/{i}.txt"),
        })
        .collect();
    for p in &paths {
        t.write(p, "x\n");
    }
    t.repo.stage(&paths).unwrap();
    let st = status(&t);
    assert_eq!(st.staged.len(), paths.len());
    assert!(st.unstaged.is_empty());
    t.repo.unstage(&paths).unwrap();
    let st = status(&t);
    assert!(st.staged.is_empty());
    assert_eq!(st.unstaged.len(), paths.len());
}

/// On case-insensitive file systems git sets `core.ignorecase=true` at init.
/// A rename that only changes case must be staged as a rename, and both
/// engines must agree before and after.
#[cfg(windows)]
#[test]
fn case_only_rename_with_ignorecase() {
    let t = TestRepo::new();
    assert_eq!(t.git(&["config", "core.ignorecase"]), "true");
    t.commit_file("Readme.md", "hello\n", "init");
    t.commit_file("Src/Main.rs", "fn main() {}\n", "src");

    // Renaming on disk only: git (ignorecase) still matches the index entry,
    // so nothing is reported; gix must not invent a delete + untracked pair.
    std::fs::rename(t.repo.abs("Readme.md"), t.repo.abs("README.md")).unwrap();
    let st = status(&t);
    assert!(st.is_clean(), "{st:?}");

    // Staging the new spelling through gk-core must not drop or duplicate the file.
    t.repo.stage(&[s("README.md")]).unwrap();
    let files = t.git(&["ls-files"]);
    assert_eq!(
        files
            .lines()
            .filter(|l| l.eq_ignore_ascii_case("readme.md"))
            .count(),
        1,
        "{files}"
    );

    // The explicit case rename (what "Rename" in a client should run).
    let old = files
        .lines()
        .find(|l| l.eq_ignore_ascii_case("readme.md"))
        .unwrap()
        .to_string();
    t.git(&["mv", "-f", &old, "README.md"]);
    let st = status(&t);
    let staged = summary(&st.staged);
    if old != "README.md" {
        assert_eq!(staged, [(s("README.md"), 'R')], "{st:?}");
        assert_eq!(
            st.staged[0].orig_path.as_ref().map(|p| p.to_string()),
            Some(old.clone())
        );
        // Diff of a pure rename is empty.
        let (o, n) = load_sides(
            &t.repo,
            &DiffTarget::Staged {
                path: s("README.md"),
                old_path: Some(old.clone()),
            },
        )
        .unwrap();
        assert_eq!(o, n);
        // Unstage both sides of the rename restores the old spelling in the index.
        t.repo.unstage(&[s("README.md"), old.clone()]).unwrap();
        let files = t.git(&["ls-files"]);
        assert!(files.lines().any(|l| l == old), "{files}");
        t.git(&["mv", "-f", &old, "README.md"]);
    }
    t.commit("case rename");
    assert_eq!(
        t.git(&["ls-tree", "--name-only", "HEAD", "README.md"]),
        "README.md"
    );
    assert!(status(&t).is_clean());

    // Directory case rename: modify a file below it and stage via the new spelling.
    std::fs::rename(t.repo.abs("Src"), t.repo.abs("src")).unwrap();
    t.write("src/Main.rs", "fn main() { println!(); }\n");
    let st = status(&t);
    assert_eq!(st.unstaged.len(), 1, "{st:?}");
    let p = st.unstaged[0].path.to_string();
    assert!(p.eq_ignore_ascii_case("src/main.rs"));
    assert_eq!(st.unstaged[0].kind, Kind::Modified);
    let d = unstaged_diff(&t, &p);
    assert_eq!((d.additions, d.deletions), (1, 1));
    let one = std::slice::from_ref(&p);
    t.repo.stage(one).unwrap();
    assert_eq!(kind_of(&status(&t).staged, &p), Some(Kind::Modified));
    t.repo.unstage(one).unwrap();
    t.repo.discard(one, &[]).unwrap();
    assert!(status(&t).is_clean());
}

/// BUG (gk-status, reported): after an on-disk case-only rename the watcher
/// fast path reports the new spelling as untracked although `git status` and
/// both full engines report a clean tree under `core.ignorecase=true`, so the
/// UI shows a phantom untracked file until the next full refresh.
#[cfg(windows)]
#[test]
fn fast_path_case_only_rename_with_ignorecase() {
    let t = TestRepo::new();
    t.commit_file("Readme.md", "hello\n", "init");
    std::fs::rename(t.repo.abs("Readme.md"), t.repo.abs("README.md")).unwrap();
    let st = status(&t);
    assert!(st.is_clean(), "{st:?}");
    fast_matches(&t, &["README.md", "Readme.md"], &st);
}
