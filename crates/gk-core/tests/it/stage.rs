//! Bulk staging (above 1000 paths, in-process) must give exactly the index
//! `git add` gives.

use crate::common::*;

const N: usize = 1100;

/// The same history and working-tree changes in a fresh repository.
fn build(autocrlf: bool) -> TestRepo {
    let t = TestRepo::with_autocrlf(autocrlf);
    t.git(&["config", "filter.upper.clean", "tr a-z A-Z"]);
    t.git(&["config", "filter.upper.smudge", "cat"]);
    t.write(
        ".gitattributes",
        "*.bin binary\n*.crlf text eol=crlf\n*.lf text eol=lf\n*.auto text=auto\n\
         *.raw -text\n*.up filter=upper\n*.id ident\n",
    );
    for i in 0..N {
        t.write(&format!("d{}/f{i}.txt", i % 7), format!("line {i}\n"));
    }
    t.write("x/exec.sh", "#!/bin/sh\n");
    t.write("x/crlf_in_index.txt", "one\r\ntwo\r\n");
    t.write("x/keep.up", "keep\n");
    t.write("df", "file that becomes a folder\n");
    // CRs reach the index only without conversion.
    t.git(&["-c", "core.autocrlf=false", "add", "-A"]);
    t.git(&["update-index", "--chmod=+x", "x/exec.sh"]);
    t.commit("init");

    for i in 0..N {
        let p = format!("d{}/f{i}.txt", i % 7);
        match i % 9 {
            0 => std::fs::remove_file(t.repo.abs(&p)).unwrap(),
            1 | 4 | 7 => t.write(&p, format!("changed {i}\r\nsecond\r\n")),
            2 => t.write(&p, format!("mixed {i}\r\nlone\rcr\n")),
            _ => t.write(&p, format!("changed {i}\n")),
        }
    }
    for (p, c) in [
        ("new/a.crlf", &b"a\nb\n"[..]),
        ("new/b.lf", b"a\r\nb\r\n"),
        ("new/c.auto", b"a\r\nb\r\n"),
        ("new/d.raw", b"a\r\nb\r\n"),
        ("new/e.bin", b"\0\r\n"),
        ("new/f.up", b"hello\n"),
        ("new/g.id", b"$Id$\r\n"),
        ("new/h.txt", b"bin\0ary\r\n"),
        ("new/empty.txt", b""),
        ("new/deep/er/z.txt", b"z\r\n"),
        ("x/exec.sh", b"#!/bin/sh\r\necho\r\n"),
        ("x/crlf_in_index.txt", b"one\r\ntwo\r\nthree\r\n"),
        ("x/keep.up", b"keep it\n"),
    ] {
        t.write(p, c);
    }
    std::fs::remove_file(t.repo.abs("df")).unwrap();
    t.write("df/inner.txt", "now a folder\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = t.repo.abs("new/run.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    t
}

fn changed_paths(t: &TestRepo) -> Vec<String> {
    let out = t
        .repo
        .git()
        .run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .unwrap()
        .stdout;
    out.split(|b| *b == 0)
        .filter(|r| r.len() > 3)
        .map(|r| String::from_utf8(r[3..].to_vec()).unwrap())
        .collect()
}

fn has_pack(t: &TestRepo) -> bool {
    std::fs::read_dir(t.repo.git_dir().join("objects/pack"))
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with(".pack"))
        })
        .unwrap_or(false)
}

fn compare(autocrlf: bool) {
    let ours = build(autocrlf);
    let theirs = build(autocrlf);
    let paths = changed_paths(&ours);
    assert!(
        paths.len() > 1000,
        "{} paths: bulk path not taken",
        paths.len()
    );
    assert_eq!(paths, changed_paths(&theirs));

    ours.repo.stage(&paths).unwrap();
    theirs.git(&["add", "-A"]);

    let ls = |t: &TestRepo| t.git(&["ls-files", "-s"]);
    let (a, b) = (ls(&ours), ls(&theirs));
    if a != b {
        let diff: Vec<String> = a
            .lines()
            .zip(b.lines())
            .filter(|(x, y)| x != y)
            .take(10)
            .map(|(x, y)| format!("ours   {x}\ntheirs {y}"))
            .collect();
        panic!("index differs (autocrlf={autocrlf}):\n{}", diff.join("\n"));
    }
    assert_eq!(ours.status(), theirs.status());
    // Stat data is valid: nothing shows as modified in the worktree.
    assert_eq!(ours.git(&["diff", "--name-only"]), "");
    // The in-process path ran (it writes a pack; git add writes loose objects).
    assert!(has_pack(&ours) && !has_pack(&theirs));
    ours.git(&["fsck", "--no-dangling"]);
    // The cached trees were invalidated where needed: commits are equal.
    ours.commit("staged");
    theirs.commit("staged");
    assert_eq!(
        ours.git(&["rev-parse", "HEAD^{tree}"]),
        theirs.git(&["rev-parse", "HEAD^{tree}"])
    );
}

#[test]
fn bulk_stage_equals_git_add() {
    compare(false);
}

#[test]
fn bulk_stage_equals_git_add_with_autocrlf() {
    compare(true);
}

#[test]
fn bulk_stage_in_unborn_repository() {
    let mk = || {
        let t = TestRepo::new();
        for i in 0..1200 {
            t.write(&format!("s{}/n{i}.txt", i % 13), format!("new {i}\n"));
        }
        t
    };
    let (ours, theirs) = (mk(), mk());
    let paths = changed_paths(&ours);
    ours.repo.stage(&paths).unwrap();
    theirs.git(&["add", "-A"]);
    assert_eq!(
        ours.git(&["ls-files", "-s"]),
        theirs.git(&["ls-files", "-s"])
    );
    assert_eq!(ours.git(&["diff", "--name-only"]), "");
    ours.git(&["fsck", "--no-dangling"]);
}
