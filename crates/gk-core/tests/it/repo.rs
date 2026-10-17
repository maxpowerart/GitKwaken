use crate::common::*;
use gk_cli::CommandLog;
use gk_core::Repo;

#[test]
fn open_from_subdir_and_failures() {
    let t = TestRepo::new();
    t.write("sub/dir/f.txt", "x\n");
    let r = Repo::open(&t.repo.abs("sub/dir"), CommandLog::default()).unwrap();
    assert_eq!(r.workdir(), t.repo.workdir());
    assert!(r.git_dir().ends_with(".git"));
    assert_eq!(r.abs("sub/dir/f.txt"), t.repo.abs("sub/dir/f.txt"));
    assert!(!r.name().is_empty());

    let plain = tempfile::tempdir().unwrap();
    assert!(Repo::open(plain.path(), CommandLog::default()).is_err());
    let bare = Remote::new();
    assert!(Repo::open(&bare.path(), CommandLog::default()).is_err());
}

#[test]
fn init_new_repo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("new");
    gk_core::init(&path, CommandLog::default()).unwrap();
    let r = Repo::open(&path, CommandLog::default()).unwrap();
    assert!(gk_core::refs::load(&r).unwrap().head.branch().is_some());
}

#[test]
fn env_and_git_override() {
    let t = TestRepo::new();
    let r = t.repo.clone().with_env([("GIT_AUTHOR_NAME".to_string(), "Env Author".to_string())]);
    t.write("a.txt", "a\n");
    r.stage_all().unwrap();
    r.commit(&gk_core::ops::CommitOptions { message: s("env"), ..Default::default() }).unwrap();
    assert_eq!(t.git(&["log", "-1", "--format=%an"]), "Env Author");

    let exe = gk_cli::find_git();
    let r = Repo::open_with_git(t.repo.workdir(), CommandLog::default(), Some(exe.clone())).unwrap();
    assert_eq!(r.git().exe(), exe.as_path());
    let bad = Repo::open_with_git(t.repo.workdir(), CommandLog::default(), Some("no-such-git-exe".into())).unwrap();
    assert!(bad.stage_all().is_err());
}
