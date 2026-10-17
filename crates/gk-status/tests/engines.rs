//! Differential test: the gix engine must agree with `git status`.

use gk_cli::{CommandLog, Git};
use gk_status::{Engine, Options, Status, Untracked, compute};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn summarize(s: &Status) -> (Vec<String>, Vec<String>) {
    let f = |v: &Vec<gk_status::Entry>| {
        v.iter().map(|e| format!("{}:{}", e.kind.code(), e.path)).collect::<Vec<_>>()
    };
    (f(&s.staged), f(&s.unstaged))
}

fn both(dir: &std::path::Path, git: &Git, untracked: Untracked) -> (Status, Status) {
    let stop = Arc::new(AtomicBool::new(false));
    let a = compute(dir, git, &[], &Options { engine: Engine::Gix, untracked }, &stop).unwrap();
    let b = compute(dir, git, &[], &Options { engine: Engine::Cli, untracked }, &stop).unwrap();
    (a, b)
}

#[test]
fn gix_matches_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let git = Git::new(dir, CommandLog::default());
    git.run(&["init", "-q", "-b", "main"]).unwrap();
    git.run(&["config", "user.email", "t@t"]).unwrap();
    git.run(&["config", "user.name", "t"]).unwrap();
    git.run(&["config", "core.autocrlf", "false"]).unwrap();

    fs::create_dir_all(dir.join("src/deep")).unwrap();
    for i in 0..50 {
        fs::write(dir.join(format!("src/f{i}.txt")), format!("line {i}\n")).unwrap();
    }
    fs::write(dir.join("src/deep/keep.txt"), "keep\n").unwrap();
    fs::write(dir.join("del.txt"), "bye\n").unwrap();
    fs::write(dir.join(".gitignore"), "ignored/\n*.log\n").unwrap();
    git.run(&["add", "-A"]).unwrap();
    git.run(&["commit", "-q", "-m", "init"]).unwrap();

    // unstaged modification, deletion, untracked, ignored
    fs::write(dir.join("src/f1.txt"), "changed\n").unwrap();
    fs::remove_file(dir.join("del.txt")).unwrap();
    fs::create_dir_all(dir.join("newdir/sub")).unwrap();
    fs::write(dir.join("newdir/sub/a.txt"), "a").unwrap();
    fs::write(dir.join("top.txt"), "t").unwrap();
    fs::create_dir_all(dir.join("ignored")).unwrap();
    fs::write(dir.join("ignored/x.txt"), "x").unwrap();
    fs::write(dir.join("debug.log"), "x").unwrap();
    // staged: modification, addition, deletion
    fs::write(dir.join("src/f2.txt"), "staged\n").unwrap();
    fs::write(dir.join("added.txt"), "new\n").unwrap();
    git.run(&["add", "src/f2.txt", "added.txt"]).unwrap();
    git.run(&["rm", "-q", "src/f3.txt"]).unwrap();
    // both staged and unstaged
    fs::write(dir.join("src/f2.txt"), "staged then changed\n").unwrap();

    for mode in [Untracked::All, Untracked::Normal] {
        let (a, b) = both(dir, &git, mode);
        assert_eq!(summarize(&a), summarize(&b), "mode {mode:?}");
    }
    let (a, _) = both(dir, &git, Untracked::All);
    let (staged, unstaged) = summarize(&a);
    assert_eq!(staged, ["A:added.txt", "M:src/f2.txt", "D:src/f3.txt"]);
    assert_eq!(
        unstaged,
        ["D:del.txt", "?:newdir/sub/a.txt", "M:src/f1.txt", "M:src/f2.txt", "?:top.txt"]
    );

    // pathspec-limited computation
    let stop = Arc::new(AtomicBool::new(false));
    for engine in [Engine::Gix, Engine::Cli] {
        let s = compute(dir, &git, &["src".into()], &Options { engine, untracked: Untracked::All }, &stop)
            .unwrap();
        let (_, u) = summarize(&s);
        assert_eq!(u, ["M:src/f1.txt", "M:src/f2.txt"], "{engine:?}");
    }
}
