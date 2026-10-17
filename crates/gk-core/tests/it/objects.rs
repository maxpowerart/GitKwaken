//! Object reader: gitoxide vs the `git cat-file --batch` reader, and the
//! fallback when gitoxide cannot read an object.

use crate::common::*;
use gk_core::content::{self, ChangeKind, DiffTarget};
use gk_core::history::{self, MetaCache};
use gk_core::{ObjectReader, Repo};

/// Root, rename + modify + delete + binary, a side branch and a merge;
/// a staged and an unstaged change.
fn sample() -> (TestRepo, Vec<String>) {
    let t = TestRepo::new();
    t.write("a.txt", "one\ntwo\nthree\n");
    t.write("ren_a.txt", "rename me\nline\nline\nline\n");
    t.write("del.txt", "gone\n");
    t.write("dir/x.txt", "x\n");
    t.git(&["add", "-A"]);
    let c1 = t.commit("root\n\nbody line");
    t.write("a.txt", "one\n2\nthree\n");
    t.git(&["mv", "ren_a.txt", "ren_b.txt"]);
    t.git(&["rm", "-q", "del.txt"]);
    t.write("bin.dat", b"\0\r\n\x01\xff\r\n".as_slice());
    let big: Vec<u8> = (0..1_200_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    t.write("big.txt", &big);
    t.git(&["add", "-A"]);
    let c2 = t.commit("second");
    t.git(&["checkout", "-q", "-b", "side"]);
    let c3 = t.commit_file("side.txt", "side\n", "side");
    t.git(&["checkout", "-q", "main"]);
    let c4 = t.commit_file("dir/x.txt", "x2\n", "main change");
    t.git_dated(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let c5 = t.head();
    t.write("a.txt", "staged\n");
    t.git(&["add", "a.txt"]);
    t.write("a.txt", "unstaged\n");
    (t, vec![c1, c2, c3, c4, c5])
}

fn targets(t: &TestRepo, commits: &[String]) -> Vec<DiffTarget> {
    let mut out = vec![
        DiffTarget::Unstaged { path: s("a.txt") },
        DiffTarget::Staged {
            path: s("a.txt"),
            old_path: None,
        },
        DiffTarget::Staged {
            path: s("nope.txt"),
            old_path: None,
        },
    ];
    for c in commits {
        for f in content::commit_details(&t.repo, t.oid(c)).unwrap().files {
            out.push(DiffTarget::Blobs {
                path: f.path,
                old: f.old_id,
                new: f.new_id,
            });
        }
    }
    out
}

#[test]
fn cli_reader_matches_gix() {
    let (t, commits) = sample();
    let gix = t.repo.clone();
    let cli = t.repo.clone().with_object_reader(ObjectReader::Cli);
    assert_eq!(gix.object_reader(), ObjectReader::Gix);
    assert_eq!(cli.object_reader(), ObjectReader::Cli);

    for c in &commits {
        let id = t.oid(c);
        let a = content::commit_details(&gix, id).unwrap();
        let b = content::commit_details(&cli, id).unwrap();
        assert_eq!(a, b, "commit details of {c}");
    }
    let second = content::commit_details(&gix, t.oid(&commits[1])).unwrap();
    let kinds: Vec<_> = second
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect();
    assert!(
        kinds.contains(&("ren_b.txt", ChangeKind::Renamed)),
        "{kinds:?}"
    );
    assert_eq!(
        content::compare_commits(&gix, t.oid(&commits[0]), t.oid(&commits[4])).unwrap(),
        content::compare_commits(&cli, t.oid(&commits[0]), t.oid(&commits[4])).unwrap()
    );

    let all = targets(&t, &commits);
    assert!(all.len() > 8);
    for target in &all {
        let a = content::load_sides(&gix, target).unwrap();
        let b = content::load_sides(&cli, target).unwrap();
        assert_eq!(a, b, "{target:?}");
    }

    // tree_blob: file, directory, missing path.
    let g = t.repo.gix();
    let tree = t.oid("HEAD^{tree}");
    for path in ["dir/x.txt", "dir", "missing/file", "bin.dat", "big.txt"] {
        assert_eq!(
            content::tree_blob(&gix, &g, tree, path).unwrap(),
            content::tree_blob(&cli, &g, tree, path).unwrap(),
            "{path}"
        );
    }
    assert_eq!(
        content::tree_blob(&cli, &g, tree, "bin.dat")
            .unwrap()
            .unwrap(),
        b"\0\r\n\x01\xff\r\n"
    );
    // Missing objects fail on both readers.
    let zero = gk_core::ObjectId::from_hex(&[b'0'; 40]).unwrap();
    assert!(content::blob(&gix, &g, zero).is_err());
    assert!(content::blob(&cli, &g, zero).is_err());

    // History row metadata and search go through the same reader.
    let ids: Vec<_> = commits.iter().map(|c| t.oid(c)).collect();
    let metas = |repo: &Repo| {
        let cache = MetaCache::new(repo.clone());
        ids.iter()
            .map(|id| {
                let m = cache.get_sync(*id).unwrap();
                (m.summary.clone(), m.author.as_str().to_string(), m.time)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(metas(&gix), metas(&cli));
    assert_eq!(metas(&cli)[4].0, "merge side");

    // The CLI reader really ran git; gitoxide never needed it.
    assert!(cli.cat_file().starts() >= 1);
    assert_eq!(gix.object_fallbacks(), 0);
    assert!(cli.object_reader_status().contains("cat-file"));
    assert!(gix.object_reader_status().contains("gitoxide"));
    let _ = history::commit_meta(&g, ids[0]).unwrap();
}

/// Objects that only git can see (an alternate object directory passed to
/// git through the environment) are served by the cat-file fallback.
#[test]
fn fallback_reads_objects_gitoxide_cannot() {
    let store = TestRepo::new();
    store.write("a.txt", "from the store\n");
    store.write("b.txt", "b\n");
    store.git(&["add", "-A"]);
    let first = store.commit("store root");
    store.write("a.txt", "changed in store\n");
    store.git(&["add", "-A"]);
    let second = store.commit("store change");
    let objects = store.repo.git_dir().join("objects");

    let t = TestRepo::new();
    t.commit_file("own.txt", "own\n", "own");
    let repo = t.repo.clone().with_env([(
        "GIT_ALTERNATE_OBJECT_DIRECTORIES".to_string(),
        objects.to_string_lossy().into_owned(),
    )]);
    let g = repo.gix();
    let id = store.oid(&second);
    assert!(g.find_object(id).is_err(), "gitoxide cannot see it");

    let d = content::commit_details(&repo, id).unwrap();
    assert_eq!(d.message, "store change\n");
    assert_eq!(d.parents, vec![store.oid(&first)]);
    assert_eq!(
        d.files
            .iter()
            .map(|f| (f.path.as_str(), f.kind))
            .collect::<Vec<_>>(),
        [("a.txt", ChangeKind::Modified)]
    );
    let f = &d.files[0];
    let (old, new) = content::load_sides(
        &repo,
        &DiffTarget::Blobs {
            path: f.path.clone(),
            old: f.old_id,
            new: f.new_id,
        },
    )
    .unwrap();
    assert_eq!(old.unwrap(), b"from the store\n");
    assert_eq!(new.unwrap(), b"changed in store\n");
    assert_eq!(
        content::tree_blob(&repo, &g, d.tree, "b.txt")
            .unwrap()
            .unwrap(),
        b"b\n"
    );
    assert!(repo.object_fallbacks() >= 4, "{}", repo.object_fallbacks());
    assert!(repo.object_reader_status().contains("fallback served"));

    // Without the alternate, git cannot read it either: gitoxide's error stands.
    let plain = Repo::open(t.repo.workdir(), gk_cli::CommandLog::default()).unwrap();
    assert!(content::commit_details(&plain, id).is_err());
}

/// SHA-256 repositories: gitoxide 0.89 (built with SHA-1 only) refuses to
/// open them, so the object fallback cannot apply: opening reports the error.
#[test]
fn sha256_repository_is_rejected_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let git = git_at(dir.path());
    git.run(&["init", "-q", "--object-format=sha256", "-b", "main"])
        .unwrap();
    configure(&git, false);
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    git.run(&["add", "a.txt"]).unwrap();
    git.run(&["commit", "-q", "-m", "sha256"]).unwrap();
    let err = Repo::open(dir.path(), gk_cli::CommandLog::default())
        .err()
        .expect("gitoxide 0.89 without the sha256 feature cannot open it");
    eprintln!("sha256 open error: {err:#}");

    // The cat-file reader itself handles 64-hex ids.
    let mut cf = gk_cli::CatFile::new(&git);
    let info = cf.info("HEAD").unwrap().unwrap();
    assert_eq!(info.oid.len(), 64);
    assert_eq!(cf.read("HEAD:a.txt").unwrap().unwrap().1, b"a\n");
}
