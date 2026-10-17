//! Integration tests for the less frequent features in `gk_core::advanced`:
//! bisect, git-flow, sparse checkout, worktrees, submodules, clean, optimize,
//! LFS and blame, all against real temporary repositories.

use crate::common::*;
use gk_cli::CommandLog;
use gk_core::Repo;
use gk_core::advanced::*;
use std::collections::HashMap;
use std::path::Path;

/// `repo` with extra `-c` style config passed through `GIT_CONFIG_*` env vars.
fn with_config(repo: &Repo, pairs: &[(&str, &str)]) -> Repo {
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), pairs.len().to_string())];
    for (i, (k, v)) in pairs.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k.to_string()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), v.to_string()));
    }
    repo.clone().with_env(env)
}

/// Local file URLs are blocked for submodules since git 2.38.1.
fn allow_file(repo: &Repo) -> Repo {
    // autocrlf off so submodule clones do not depend on the user's global config.
    with_config(
        repo,
        &[
            ("protocol.file.allow", "always"),
            ("core.autocrlf", "false"),
        ],
    )
}

fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn last_component(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().to_string()
}

fn current_branch(t: &TestRepo) -> String {
    t.git(&["branch", "--show-current"])
}

// ------------------------------------------------------------------ bisect

#[test]
fn bisect_finds_first_bad_commit() {
    let t = TestRepo::new();
    let mut ids = Vec::new();
    for i in 0..12 {
        let state = if i >= 7 { "bug" } else { "ok" };
        ids.push(t.commit_file("state.txt", &format!("{state} {i}\n"), &format!("c{i}")));
    }
    let first_bad = ids[7].clone();

    let log = t.repo.bisect_start(&ids[11], &ids[0]).unwrap();
    assert!(log.contains("git bisect start"), "{log}");
    assert!(t.exists(".git/BISECT_LOG"));

    let mut found = None;
    let mut transcript = String::new();
    for _ in 0..12 {
        let state = t.read("state.txt");
        let mark = if state.starts_with("bug") {
            BisectMark::Bad
        } else {
            BisectMark::Good
        };
        let out = t.repo.bisect_mark(mark, None).unwrap();
        transcript.push_str(&format!("--- {mark:?} at {:?}:\n{out}\n", state.trim()));
        if out.contains("is the first bad commit") {
            found = out.split_whitespace().next().map(str::to_string);
            break;
        }
    }
    assert_eq!(
        found.as_deref(),
        Some(first_bad.as_str()),
        "bisect transcript:\n{transcript}"
    );

    t.repo.bisect_reset().unwrap();
    assert!(!t.exists(".git/BISECT_LOG"));
    assert_eq!(current_branch(&t), "main");
    assert_eq!(t.head(), ids[11]);
}

#[test]
fn bisect_skip_and_explicit_revs() {
    let t = TestRepo::new();
    let ids: Vec<String> = (0..6)
        .map(|i| t.commit_file("n.txt", &format!("{i}\n"), &format!("c{i}")))
        .collect();
    t.repo.bisect_start("HEAD", &ids[0]).unwrap();
    // Skipping the current candidate moves to another commit.
    let before = t.head();
    t.repo.bisect_mark(BisectMark::Skip, None).unwrap();
    assert_ne!(t.head(), before);
    // Marking explicit revisions narrows to the single bad commit.
    t.repo.bisect_mark(BisectMark::Good, Some(&ids[3])).unwrap();
    let out = t.repo.bisect_mark(BisectMark::Bad, Some(&ids[4])).unwrap();
    assert!(out.starts_with(&ids[4]), "{out}");
    t.repo.bisect_reset().unwrap();
    assert_eq!(current_branch(&t), "main");
}

#[test]
fn bisect_rejects_option_like_revisions() {
    let t = TestRepo::new();
    t.commit_file("a", "a\n", "a");
    assert!(t.repo.bisect_start("--foo", "HEAD").is_err());
    assert!(!t.exists(".git/BISECT_LOG"));
}

// ------------------------------------------------------------------ git-flow

fn parents(t: &TestRepo, rev: &str) -> usize {
    t.git(&["rev-list", "--parents", "-n", "1", rev])
        .split_whitespace()
        .count()
        - 1
}

#[test]
fn flow_feature_and_release_cycle() {
    let t = TestRepo::new();
    t.commit_file("app.txt", "v0\n", "initial");
    assert!(t.repo.flow_config().is_none());

    let cfg = FlowConfig {
        version_tag: "v".into(),
        ..FlowConfig::default()
    };
    t.repo.flow_init(&cfg).unwrap();
    assert_eq!(t.repo.flow_config(), Some(cfg.clone()));
    assert_eq!(t.rev("develop"), t.rev("main"));
    // Init is idempotent and keeps an existing develop.
    t.repo.flow_init(&cfg).unwrap();

    // Feature: branches from develop, finishes with a --no-ff merge into develop.
    t.repo
        .flow_start(&cfg, FlowKind::Feature, "login", None)
        .unwrap();
    assert_eq!(current_branch(&t), "feature/login");
    let feat = t.commit_file("login.txt", "login\n", "add login");
    t.repo
        .flow_finish(&cfg, FlowKind::Feature, "login", None, false)
        .unwrap();
    assert_eq!(current_branch(&t), "develop");
    assert_eq!(parents(&t, "develop"), 2, "no-ff merge commit");
    assert_eq!(t.rev("develop^2"), feat);
    assert!(t.git(&["branch", "--list", "feature/login"]).is_empty());
    assert_ne!(t.rev("main"), t.rev("develop"), "main untouched");

    // Release: from develop, merged into main and develop, tagged on main.
    t.repo
        .flow_start(&cfg, FlowKind::Release, "1.0", None)
        .unwrap();
    assert_eq!(current_branch(&t), "release/1.0");
    let bump = t.commit_file("app.txt", "v1.0\n", "bump version");
    t.repo
        .flow_finish(&cfg, FlowKind::Release, "1.0", Some("Release 1.0"), false)
        .unwrap();
    assert_eq!(parents(&t, "main"), 2);
    assert_eq!(t.rev("main^2"), bump);
    assert_eq!(t.git(&["cat-file", "-t", "v1.0"]), "tag", "annotated tag");
    assert_eq!(t.rev("v1.0^{commit}"), t.rev("main"));
    assert_eq!(
        t.git(&["tag", "-l", "--format=%(contents:subject)", "v1.0"]),
        "Release 1.0"
    );
    assert_eq!(
        t.git(&["merge-base", "--is-ancestor", &bump, "develop"]),
        "",
        "release merged back into develop"
    );
    assert_eq!(current_branch(&t), "develop");
    assert!(t.git(&["branch", "--list", "release/1.0"]).is_empty());
    assert_eq!(t.git(&["show", "main:login.txt"]), "login");

    // Hotfix: from main, keep the branch when asked.
    t.repo
        .flow_start(&cfg, FlowKind::Hotfix, "1.0.1", None)
        .unwrap();
    t.commit_file("app.txt", "v1.0.1\n", "hotfix");
    t.repo
        .flow_finish(&cfg, FlowKind::Hotfix, "1.0.1", None, true)
        .unwrap();
    assert_eq!(t.rev("v1.0.1^{commit}"), t.rev("main"));
    assert!(!t.git(&["branch", "--list", "hotfix/1.0.1"]).is_empty());
    assert_eq!(t.git(&["show", "develop:app.txt"]), "v1.0.1");
}

#[test]
fn flow_finish_stops_on_conflict() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "base\n", "base");
    let cfg = FlowConfig::default();
    t.repo.flow_init(&cfg).unwrap();
    t.repo
        .flow_start(&cfg, FlowKind::Feature, "x", None)
        .unwrap();
    t.commit_file("f.txt", "feature\n", "feature edit");
    t.git(&["checkout", "-q", "develop"]);
    t.commit_file("f.txt", "develop\n", "develop edit");
    let log = t
        .repo
        .flow_finish(&cfg, FlowKind::Feature, "x", None, false)
        .unwrap();
    assert!(log.contains("CONFLICT"), "{log}");
    assert!(t.exists(".git/MERGE_HEAD"));
    assert!(
        !t.git(&["branch", "--list", "feature/x"]).is_empty(),
        "branch kept until the merge is resolved"
    );
}

#[test]
fn flow_init_without_master_fails() {
    let t = TestRepo::new();
    t.commit_file("a", "a\n", "a");
    let cfg = FlowConfig {
        master: "production".into(),
        ..FlowConfig::default()
    };
    let err = t.repo.flow_init(&cfg).unwrap_err().to_string();
    assert!(err.contains("production"), "{err}");
}

// ------------------------------------------------------------------ sparse checkout

#[test]
fn sparse_set_list_disable() {
    let t = TestRepo::new();
    t.write("root.txt", "r\n");
    t.write("a/one.txt", "1\n");
    t.write("a/deep/two.txt", "2\n");
    t.write("b/three.txt", "3\n");
    t.write("c/four.txt", "4\n");
    t.git(&["add", "-A"]);
    t.commit("init");
    assert_eq!(t.repo.sparse_list().unwrap(), None);

    t.repo.sparse_set(&[s("a"), s("c")]).unwrap();
    assert_eq!(t.repo.sparse_list().unwrap(), Some(vec![s("a"), s("c")]));
    assert!(t.exists("root.txt"), "cone mode keeps top-level files");
    assert!(t.exists("a/deep/two.txt"));
    assert!(t.exists("c/four.txt"));
    assert!(!t.exists("b/three.txt"));
    assert_eq!(t.status(), "", "sparse files are not reported as deleted");

    t.repo.sparse_set(&[s("b")]).unwrap();
    assert!(t.exists("b/three.txt"));
    assert!(!t.exists("a/one.txt"));

    t.repo.sparse_disable().unwrap();
    assert_eq!(t.repo.sparse_list().unwrap(), None);
    for f in ["a/one.txt", "a/deep/two.txt", "b/three.txt", "c/four.txt"] {
        assert!(t.exists(f), "{f} restored");
    }
}

// ------------------------------------------------------------------ worktrees

#[test]
fn worktrees_add_list_remove_prune() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "init");
    t.git(&["branch", "existing"]);
    let dir = tempfile::tempdir().unwrap();
    let wt1 = dir.path().join("wt-new");
    let wt2 = dir.path().join("wt-existing");

    let list = t.repo.worktrees().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].branch.as_deref(), Some("main"));

    t.repo.worktree_add(&wt1, "topic", true).unwrap();
    t.repo.worktree_add(&wt2, "existing", false).unwrap();
    assert!(wt1.join("a.txt").exists());
    let list = t.repo.worktrees().unwrap();
    assert_eq!(list.len(), 3);
    let by_name: HashMap<String, &Worktree> =
        list.iter().map(|w| (last_component(&w.path), w)).collect();
    assert_eq!(by_name["wt-new"].branch.as_deref(), Some("topic"));
    assert_eq!(by_name["wt-existing"].branch.as_deref(), Some("existing"));
    assert_eq!(by_name["wt-new"].head, t.head());
    assert!(!by_name["wt-new"].prunable);

    // A checked-out branch cannot be added twice.
    assert!(
        t.repo
            .worktree_add(&dir.path().join("dup"), "topic", false)
            .is_err()
    );
    assert!(
        t.repo
            .worktree_add(&dir.path().join("x"), "-b", true)
            .is_err()
    );

    // Dirty worktree: plain remove fails, --force succeeds.
    std::fs::write(wt1.join("dirty.txt"), "x").unwrap();
    assert!(t.repo.worktree_remove(&wt1, false).is_err());
    t.repo.worktree_remove(&wt1, true).unwrap();
    assert!(!wt1.exists());
    assert_eq!(t.repo.worktrees().unwrap().len(), 2);

    // Deleted behind git's back: listed as prunable until pruned.
    std::fs::remove_dir_all(&wt2).unwrap();
    let list = t.repo.worktrees().unwrap();
    let gone = list
        .iter()
        .find(|w| last_component(&w.path) == "wt-existing")
        .unwrap();
    assert!(gone.prunable);
    t.repo.worktree_prune().unwrap();
    assert_eq!(t.repo.worktrees().unwrap().len(), 1);
    // The branch is free again.
    t.git(&["checkout", "-q", "existing"]);
}

// ------------------------------------------------------------------ submodules

#[test]
fn submodules_add_update_status_sync() {
    let lib = TestRepo::new();
    let lib_v1 = lib.commit_file("lib.txt", "v1\n", "lib v1");
    let lib_url = fwd(lib.repo.workdir());

    let t = TestRepo::new();
    let sup = allow_file(&t.repo);
    t.commit_file("readme.txt", "super\n", "init");
    assert!(sup.submodules().unwrap().is_empty(), "no .gitmodules");

    let (cancel, mut p) = net();
    sup.submodule_add(&lib_url, "libs/lib", &cancel, &mut p)
        .unwrap();
    assert!(t.exists(".gitmodules"));
    assert_eq!(t.read("libs/lib/lib.txt"), "v1\n");
    t.git(&["commit", "-q", "-m", "add submodule"]);
    let subs = sup.submodules().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].path, "libs/lib");
    assert_eq!(subs[0].commit, lib_v1);
    assert_eq!(subs[0].state, ' ');
    assert!(
        sup.submodule_add(&lib_url, "--evil", &cancel, &mut p)
            .is_err()
    );

    // A fresh clone has the submodule uninitialized until update --init.
    let c = TestRepo::clone_from(t.repo.workdir());
    let csup = allow_file(&c.repo);
    let subs = csup.submodules().unwrap();
    assert_eq!(subs[0].state, '-');
    assert!(!c.exists("libs/lib/lib.txt"));
    csup.submodule_update(&[], &cancel, &mut p).unwrap();
    let subs = csup.submodules().unwrap();
    assert_eq!(subs[0].state, ' ');
    assert_eq!(c.read("libs/lib/lib.txt"), "v1\n");

    // Moving the submodule checkout shows '+', update restores the recorded commit.
    let lib_v2 = lib.commit_file("lib.txt", "v2\n", "lib v2");
    let in_sub = git_at(&c.repo.abs("libs/lib"));
    configure(&in_sub, false);
    in_sub.run(&["fetch", "-q", "origin"]).unwrap();
    in_sub.run(&["checkout", "-q", &lib_v2]).unwrap();
    let subs = csup.submodules().unwrap();
    assert_eq!(subs[0].state, '+');
    assert_eq!(subs[0].commit, lib_v2);
    csup.submodule_update(&[s("libs/lib")], &cancel, &mut p)
        .unwrap();
    assert_eq!(csup.submodules().unwrap()[0].commit, lib_v1);

    // sync copies a changed URL from .gitmodules into .git/config.
    let moved = tempfile::tempdir().unwrap();
    let new_url = fwd(moved.path());
    c.git(&[
        "config",
        "-f",
        ".gitmodules",
        "submodule.libs/lib.url",
        &new_url,
    ]);
    assert_ne!(
        c.git(&["config", "submodule.libs/lib.url"]),
        new_url,
        "not synced yet"
    );
    csup.submodule_sync().unwrap();
    assert_eq!(c.git(&["config", "submodule.libs/lib.url"]), new_url);
    assert_eq!(
        git_at(&c.repo.abs("libs/lib"))
            .read(&["remote", "get-url", "origin"])
            .unwrap(),
        new_url
    );
}

// ------------------------------------------------------------------ clean

// BUG (gk-core, reported): `Repo::clean` passes `--pathspec-from-file`, which
// `git clean` does not support ("unknown option"), so cleaning from the UI
// always fails. Remove the `ignore` once fixed.
#[test]
fn clean_preview_and_clean() {
    let t = TestRepo::new();
    t.commit_file(".gitignore", "*.log\n", "ignore");
    t.write("u.txt", "u");
    t.write("dir/x.txt", "x");
    t.write("keep.log", "log");
    t.write("weird name [1].txt", "w");

    let mut files = t.repo.clean_preview(false, false).unwrap();
    files.sort();
    assert_eq!(files, [s("u.txt"), s("weird name [1].txt")]);
    let with_dirs = t.repo.clean_preview(false, true).unwrap();
    assert!(with_dirs.contains(&s("dir/")), "{with_dirs:?}");
    assert!(!with_dirs.contains(&s("keep.log")));
    let all = t.repo.clean_preview(true, true).unwrap();
    assert!(all.contains(&s("keep.log")), "{all:?}");
    assert!(
        !t.exists("dir/x.txt") || t.exists("u.txt"),
        "preview deletes nothing"
    );
    assert!(t.exists("dir/x.txt") && t.exists("keep.log"));

    t.repo.clean(&[]).unwrap();
    t.repo
        .clean(&[s("u.txt"), s("dir/"), s("weird name [1].txt")])
        .unwrap();
    assert!(!t.exists("u.txt"));
    assert!(!t.exists("dir"));
    assert!(!t.exists("weird name [1].txt"));
    assert!(t.exists("keep.log"), "not selected, so kept");
    assert!(t.exists(".gitignore"), "tracked files never touched");
    assert_eq!(t.repo.clean_preview(true, true).unwrap(), [s("keep.log")]);
}

// ------------------------------------------------------------------ optimize

#[test]
fn write_commit_graph_and_offer() {
    let t = TestRepo::new();
    for i in 0..3 {
        t.commit_file("f.txt", &format!("{i}\n"), &format!("c{i}"));
    }
    assert!(!t.repo.has_commit_graph());
    assert!(offer_commit_graph(
        COMMIT_GRAPH_OFFER_MIN_COMMITS + 1,
        false
    ));
    assert!(!offer_commit_graph(COMMIT_GRAPH_OFFER_MIN_COMMITS, false));
    assert!(!offer_commit_graph(1_000_000, true));
    let (cancel, mut p) = net();
    t.repo.write_commit_graph(&cancel, &mut p).unwrap();
    assert!(t.repo.has_commit_graph());
    assert!(t.repo.optimization_status().commit_graph);
    assert!(
        t.repo
            .git()
            .run(&["commit-graph", "verify"])
            .unwrap()
            .success()
    );
}

#[test]
fn optimize_sets_config_and_writes_commit_graph() {
    let t = TestRepo::new();
    for i in 0..5 {
        t.commit_file("f.txt", &format!("{i}\n"), &format!("c{i}"));
    }
    t.git(&["repack", "-adq"]);
    // git records the real (long) path in maintenance.repo, while the temp dir may
    // be spelled with 8.3 short names (C:\Users\RUNNER~1\...): open it by its long path.
    let long = std::fs::canonicalize(t.repo.workdir()).unwrap();
    let long = long.to_string_lossy();
    let long = long.strip_prefix(r"\\?\").unwrap_or(&long).to_string();
    let long_repo = Repo::open(Path::new(&long), CommandLog::default()).unwrap();
    // `maintenance register` writes to the global config: point it at a scratch file.
    let global = tempfile::tempdir().unwrap();
    let global_cfg = global.path().join("gitconfig");
    std::fs::write(&global_cfg, "").unwrap();
    let r = long_repo.with_env([(
        "GIT_CONFIG_GLOBAL".to_string(),
        global_cfg.to_string_lossy().to_string(),
    )]);

    let before = r.optimization_status();
    assert_eq!(before, Optimization::default());

    let (cancel, mut p) = net();
    r.optimize(&cancel, &mut p).unwrap();
    for (k, v) in [
        ("core.fsmonitor", "true"),
        ("core.untrackedCache", "true"),
        ("feature.manyFiles", "true"),
        ("fetch.writeCommitGraph", "true"),
    ] {
        assert_eq!(r.config_get(k).as_deref(), Some(v), "{k}");
    }
    if cfg!(windows) {
        assert_eq!(r.config_get("core.longpaths").as_deref(), Some("true"));
    }
    let info = r.git_dir().join("objects/info");
    assert!(
        info.join("commit-graph").exists() || info.join("commit-graphs").exists(),
        "commit-graph written"
    );
    assert!(
        r.git_dir().join("objects/pack/multi-pack-index").exists(),
        "multi-pack-index written"
    );
    // Changed-path Bloom filters are in the graph.
    let verify = r.git().run(&["commit-graph", "verify"]).unwrap();
    assert!(verify.success());

    let after = r.optimization_status();
    assert!(after.fsmonitor && after.untracked_cache && after.many_files);
    assert!(after.commit_graph);
    assert!(
        after.maintenance,
        "registered in the (scratch) global config"
    );
    let global_text = std::fs::read_to_string(&global_cfg).unwrap();
    assert!(global_text.contains("[maintenance]"), "{global_text}");

    // Do not leave a fsmonitor daemon holding the temp dir open.
    let _ = r.git().run_raw(&["fsmonitor--daemon", "stop"], None);
    // A second run is harmless.
    r.optimize(&cancel, &mut p).unwrap();
    let _ = r.git().run_raw(&["fsmonitor--daemon", "stop"], None);
}

// ------------------------------------------------------------------ LFS

/// `None` (with the reason printed) when git-lfs is not installed.
fn lfs_repo() -> Option<TestRepo> {
    let t = TestRepo::new();
    if !t.repo.lfs_available() {
        eprintln!("SKIP lfs test: `git lfs version` failed (git-lfs not installed)");
        return None;
    }
    // Local filter config so the test does not depend on `git lfs install` globally.
    t.git(&["lfs", "install", "--local", "--skip-repo"]);
    Some(t)
}

#[test]
fn lfs_track_commit_pointer() {
    let Some(t) = lfs_repo() else { return };
    t.repo.lfs_track("*.bin").unwrap();
    assert!(
        t.read(".gitattributes")
            .contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
        "{}",
        t.read(".gitattributes")
    );
    let data: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
    t.write("assets/big.bin", &data);
    t.git(&["add", ".gitattributes", "assets/big.bin"]);
    t.commit("add binary");

    let pointer = String::from_utf8(t.index_content("assets/big.bin")).unwrap();
    assert!(
        pointer.starts_with("version https://git-lfs.github.com/spec/v1"),
        "{pointer}"
    );
    assert!(pointer.contains("size 50000"), "{pointer}");
    assert_eq!(t.read_bytes("assets/big.bin"), data, "smudged in worktree");
    assert!(t.git(&["lfs", "ls-files", "-n"]).contains("assets/big.bin"));
    // Status through gk-status sees the file as clean.
    let st = gk_status::compute(
        t.repo.workdir(),
        t.repo.git(),
        &[],
        &gk_status::Options::default(),
        &Default::default(),
    )
    .unwrap();
    assert!(st.is_clean(), "{st:?}");
    // No lock server for a local repo: listing locks fails cleanly, no panic.
    let _ = t.repo.lfs_locks();
}

// ------------------------------------------------------------------ blame

/// Per final line: (commit id, original line number, text), from `--line-porcelain`.
fn reference_blame(t: &TestRepo, args: &[&str]) -> Vec<(String, u32, String, String, String)> {
    let mut full = vec!["blame", "--line-porcelain", "-w"];
    full.extend_from_slice(args);
    let out = t.git(&full);
    let mut rows = Vec::new();
    let (mut id, mut orig, mut author, mut summary) =
        (String::new(), 0, String::new(), String::new());
    for line in out.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            rows.push((
                id.clone(),
                orig,
                text.to_string(),
                author.clone(),
                summary.clone(),
            ));
        } else if let Some(v) = line.strip_prefix("author ") {
            author = v.to_string();
        } else if let Some(v) = line.strip_prefix("summary ") {
            summary = v.to_string();
        } else {
            let mut it = line.split(' ');
            let first = it.next().unwrap();
            if first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) {
                id = first.to_string();
                orig = it.next().unwrap().parse().unwrap();
            }
        }
    }
    rows
}

fn assert_blame_matches(t: &TestRepo, b: &Blame, args: &[&str]) {
    let reference = reference_blame(t, args);
    assert_eq!(b.lines.len(), reference.len());
    assert_eq!(b.text.len(), reference.len());
    for (i, (id, orig, text, author, summary)) in reference.iter().enumerate() {
        let c = &b.commits[b.lines[i].commit];
        assert_eq!(&c.id, id, "line {}", i + 1);
        assert_eq!(b.lines[i].orig_line, *orig, "line {}", i + 1);
        assert_eq!(&b.text[i], text, "line {}", i + 1);
        assert_eq!(&c.author, author, "line {}", i + 1);
        assert_eq!(&c.summary, summary, "line {}", i + 1);
    }
}

#[test]
fn blame_matches_git_blame() {
    let t = TestRepo::new();
    let c1 = t.commit_file("src/f.rs", "fn a() {}\nfn b() {}\nfn c() {}\n", "first");
    t.git(&["config", "user.name", "Second Author"]);
    t.commit_file(
        "src/f.rs",
        "fn a() {}\nfn b2() {}\nfn c() {}\nfn d() {}\n",
        "second",
    );
    t.git(&["config", "user.name", "Test User"]);
    // Whitespace-only change is ignored (-w).
    t.commit_file(
        "src/f.rs",
        "fn a() {}\nfn b2() {}\n  fn c() {}\nfn d() {}\n\tfn e() {}\n",
        "third",
    );
    // Rename keeps attribution and records `previous`.
    t.git(&["mv", "src/f.rs", "src/g.rs"]);
    t.commit("rename");
    t.write("src/g.rs", format!("{}fn f() {{}}\n", t.read("src/g.rs")));

    let b = t.repo.blame("src/g.rs", None).unwrap();
    assert_blame_matches(&t, &b, &["--", "src/g.rs"]);
    assert_eq!(b.lines.len(), 6);
    let first = &b.commits[b.lines[0].commit];
    assert_eq!(first.id, c1);
    assert_eq!(first.author, "Test User");
    assert!(first.author_time > 0);
    let b2 = &b.commits[b.lines[1].commit];
    assert_eq!(b2.author, "Second Author");
    assert!(b2.previous.is_some(), "second commit has a previous");
    // Uncommitted line.
    assert_eq!(
        b.commits[b.lines[5].commit].id,
        "0000000000000000000000000000000000000000"
    );

    // At an older revision and path.
    let b = t.repo.blame("src/f.rs", Some("HEAD~2")).unwrap();
    assert_blame_matches(&t, &b, &["HEAD~2", "--", "src/f.rs"]);
    assert_eq!(
        b.text,
        ["fn a() {}", "fn b2() {}", "fn c() {}", "fn d() {}"]
    );

    assert!(t.repo.blame("missing.rs", None).is_err());
}

#[test]
fn blame_on_larger_generated_file() {
    let t = TestRepo::new();
    let mut lines: Vec<String> = (0..400).map(|i| format!("line {i}")).collect();
    t.commit_file("big.txt", &(lines.join("\n") + "\n"), "c0");
    for round in 1..8 {
        for (i, l) in lines.iter_mut().enumerate() {
            if i % (round + 3) == 0 {
                *l = format!("line {i} edited in round {round}");
            }
        }
        t.commit_file("big.txt", &(lines.join("\n") + "\n"), &format!("c{round}"));
    }
    let b = t.repo.blame("big.txt", None).unwrap();
    assert_eq!(b.commits.len(), 8);
    assert_blame_matches(&t, &b, &["--", "big.txt"]);
}

/// Opens a second handle to the same workdir (sanity check for `with_env` clones).
#[test]
fn with_config_handles_share_the_repository() {
    let t = TestRepo::new();
    t.commit_file("a", "a\n", "a");
    let r = with_config(&t.repo, &[("core.abbrev", "12")]);
    assert_eq!(
        r.git()
            .read(&["rev-parse", "--short", "HEAD"])
            .unwrap()
            .len(),
        12
    );
    let again = Repo::open(r.workdir(), CommandLog::default()).unwrap();
    assert_eq!(again.workdir(), t.repo.workdir());
}
