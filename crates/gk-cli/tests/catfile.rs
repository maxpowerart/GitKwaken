//! `CatFile` against a real temporary repository.

use gk_cli::{CatFile, CatFilePool, CommandLog, Git, ObjectKind};

struct Fixture {
    _dir: tempfile::TempDir,
    git: Git,
    big: Vec<u8>,
    binary: Vec<u8>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let git = Git::new(dir.path(), CommandLog::default());
    git.run(&["init", "-q", "-b", "main"]).unwrap();
    git.run(&["config", "user.name", "T"]).unwrap();
    git.run(&["config", "user.email", "t@e"]).unwrap();
    git.run(&["config", "core.autocrlf", "false"]).unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    // > 1 MB, not a multiple of any buffer size, every byte value.
    let big: Vec<u8> = (0..1_500_007u32).map(|i| (i * 31 % 251) as u8).collect();
    std::fs::write(dir.path().join("big.bin"), &big).unwrap();
    let binary = b"\0\r\n\r\nline\r\n\0\0\x01\xff\n missing\n".to_vec();
    std::fs::write(dir.path().join("bin.dat"), &binary).unwrap();
    std::fs::create_dir(dir.path().join("sub dir")).unwrap();
    std::fs::write(dir.path().join("sub dir").join("b c.txt"), "spaced\n").unwrap();
    std::fs::write(dir.path().join(".gitattributes"), "*.dat -text\n").unwrap();
    git.run(&["add", "-A"]).unwrap();
    git.run(&["commit", "-q", "-m", "first\n\nbody"]).unwrap();
    Fixture {
        _dir: dir,
        git,
        big,
        binary,
    }
}

#[test]
fn reads_blob_tree_commit_and_missing() {
    let f = fixture();
    let mut cf = CatFile::new(&f.git);
    assert!(!cf.is_running(), "lazy start");

    let (kind, data) = cf.read("HEAD:a.txt").unwrap().unwrap();
    assert_eq!((kind, data.as_slice()), (ObjectKind::Blob, &b"hello\n"[..]));
    assert!(cf.is_running());

    let commit = f.git.read(&["rev-parse", "HEAD"]).unwrap();
    let (kind, data) = cf.read(&commit).unwrap().unwrap();
    assert_eq!(kind, ObjectKind::Commit);
    assert_eq!(
        data,
        f.git.run(&["cat-file", "commit", "HEAD"]).unwrap().stdout
    );
    assert!(data.ends_with(b"first\n\nbody\n"));

    let tree = f.git.read(&["rev-parse", "HEAD^{tree}"]).unwrap();
    let (kind, data) = cf.read(&tree).unwrap().unwrap();
    assert_eq!(kind, ObjectKind::Tree);
    assert_eq!(
        data,
        f.git.run(&["cat-file", "tree", &tree]).unwrap().stdout
    );

    // Paths with spaces resolve; info reports id, type and size.
    let (_, spaced) = cf.read("HEAD:sub dir/b c.txt").unwrap().unwrap();
    assert_eq!(spaced, b"spaced\n");
    let info = cf.info("HEAD:a.txt").unwrap().unwrap();
    assert_eq!(info.kind, ObjectKind::Blob);
    assert_eq!(info.size, 6);
    assert_eq!(info.oid, f.git.read(&["rev-parse", "HEAD:a.txt"]).unwrap());

    // Missing objects are None, and the stream stays in sync afterwards.
    assert_eq!(cf.read(&"0".repeat(40)).unwrap(), None);
    assert_eq!(cf.read("HEAD:nope").unwrap(), None);
    assert_eq!(cf.info("no-such-branch").unwrap(), None);
    assert_eq!(cf.read("HEAD:a.txt").unwrap().unwrap().1, b"hello\n");
    assert!(cf.read("bad\nname").is_err());
    assert_eq!(cf.starts(), 1, "one process served everything");
}

#[test]
fn large_and_binary_objects_are_exact() {
    let f = fixture();
    let mut cf = CatFile::new(&f.git);
    let (_, big) = cf.read("HEAD:big.bin").unwrap().unwrap();
    assert_eq!(big.len(), f.big.len());
    assert!(big == f.big);
    let (_, bin) = cf.read("HEAD:bin.dat").unwrap().unwrap();
    assert_eq!(bin, f.binary);

    // Over the limit: header only, content skipped, stream still in sync.
    let o = cf.read_limited("HEAD:big.bin", 1024).unwrap().unwrap();
    assert_eq!(o.info.size, f.big.len() as u64);
    assert_eq!(o.data, None);
    let o = cf.read_limited("HEAD:bin.dat", 1024).unwrap().unwrap();
    assert_eq!(o.data.as_deref(), Some(f.binary.as_slice()));
    assert_eq!(cf.starts(), 1);
}

#[test]
fn restarts_after_the_process_died() {
    let f = fixture();
    let log = f.git.log().clone();
    let mut cf = CatFile::new(&f.git);
    assert!(cf.read("HEAD:a.txt").unwrap().is_some());
    let pid = cf.pid().unwrap();
    cf.kill();
    assert_eq!(cf.read("HEAD:a.txt").unwrap().unwrap().1, b"hello\n");
    assert_eq!(cf.starts(), 2);
    assert_ne!(cf.pid(), Some(pid));
    // The dead process's lifetime was logged.
    assert!(
        log.entries()
            .iter()
            .any(|e| e.command.contains("cat-file --batch-command"))
    );
    drop(cf);
}

#[test]
fn pool_is_shared_between_threads() {
    let f = fixture();
    let pool = std::sync::Arc::new(CatFilePool::new(f.git.clone(), 2));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let (_, d) = pool.read("HEAD:a.txt").unwrap().unwrap();
                    assert_eq!(d, b"hello\n");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(pool.starts() <= 2, "bounded: {}", pool.starts());
    assert!(pool.running() >= 1);
}
