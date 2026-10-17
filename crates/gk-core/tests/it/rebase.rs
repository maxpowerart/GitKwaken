use crate::common::*;
use gk_core::RepoState;
use gk_core::ops::{TodoAction, TodoItem};
use gk_core::refs;

fn state(t: &TestRepo) -> RepoState {
    refs::load(&t.repo).unwrap().state
}

#[test]
fn rebase_clean() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    t.git(&["checkout", "-q", "-b", "topic"]);
    t.commit_file("t.txt", "t\n", "topic 1");
    t.commit_file("t.txt", "t2\n", "topic 2");
    t.git(&["checkout", "-q", "main"]);
    t.commit_file("m.txt", "m\n", "main 1");
    t.git(&["checkout", "-q", "topic"]);
    t.repo.rebase("main").unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.subjects("HEAD"), ["topic 2", "topic 1", "main 1", "base"]);
    assert!(t.repo.rebase("no-such-branch").is_err());
}

#[test]
fn rebase_conflict_continue_skip_abort() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    t.git(&["checkout", "-q", "topic"]);
    t.commit_file("t.txt", "t\n", "topic extra");
    let topic = t.head();

    let text = t.repo.rebase("main").unwrap();
    assert!(text.contains("CONFLICT"), "{text}");
    assert_eq!(state(&t), RepoState::Rebasing { interactive: false });
    t.repo.abort_op(state(&t)).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.head(), topic);

    t.repo.rebase("main").unwrap();
    t.repo.skip_op(state(&t)).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.subjects("HEAD"), ["topic extra", "main change", "base"]);

    t.git(&["reset", "-q", "--hard", &topic]);
    t.repo.rebase("main").unwrap();
    t.repo.save_resolution("c.txt", b"both\n").unwrap();
    t.repo.continue_op(state(&t)).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(
        t.subjects("HEAD"),
        ["topic extra", "topic change", "main change", "base"]
    );
    assert_eq!(t.git(&["show", "HEAD~1:c.txt"]), "both");
}

/// base + five commits, each adding its own file.
fn five(t: &TestRepo) -> Vec<String> {
    t.commit_file("base.txt", "b\n", "base");
    ["one", "two", "three", "four", "five"]
        .iter()
        .map(|n| t.commit_file(&format!("{n}.txt"), "x\n", n))
        .collect()
}

fn item(action: TodoAction, id: &str, summary: &str) -> TodoItem {
    TodoItem {
        action,
        id: id.to_string(),
        summary: summary.to_string(),
        new_message: None,
    }
}

#[test]
fn interactive_reword_squash_fixup_drop() {
    let t = TestRepo::new();
    let c = five(&t);
    let todo = vec![
        item(TodoAction::Pick, &c[0], "one"),
        TodoItem {
            new_message: Some(s("Two reworded\n\nWith a body line\nand a second one\n")),
            ..item(TodoAction::Reword, &c[1], "two")
        },
        item(TodoAction::Squash, &c[2], "three"),
        item(TodoAction::Fixup, &c[3], "four"),
        item(TodoAction::Drop, &c[4], "five"),
    ];
    t.repo.rebase_interactive("HEAD~5", &todo, false).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(t.subjects("HEAD"), ["Two reworded", "one", "base"]);
    assert_eq!(
        t.git(&["log", "-1", "--format=%B"]),
        "Two reworded\n\nWith a body line\nand a second one\n\nthree"
    );
    assert_eq!(
        t.git(&["ls-tree", "--name-only", "HEAD"]),
        "base.txt\nfour.txt\none.txt\nthree.txt\ntwo.txt"
    );
    assert_eq!(t.status(), "");
}

#[test]
fn interactive_reorder_and_plain_reword() {
    let t = TestRepo::new();
    let c = five(&t);
    let todo = vec![
        item(TodoAction::Pick, &c[2], "three"),
        item(TodoAction::Pick, &c[0], "one"),
        item(TodoAction::Reword, &c[1], "two"),
        item(TodoAction::Pick, &c[4], "five"),
        item(TodoAction::Pick, &c[3], "four"),
    ];
    t.repo
        .rebase_interactive(&format!("{}~1", c[0]), &todo, true)
        .unwrap();
    assert_eq!(
        t.subjects("HEAD"),
        ["four", "five", "two", "one", "three", "base"]
    );
}

#[test]
fn interactive_reword_quotes_in_message() {
    let t = TestRepo::new();
    let c = five(&t);
    let msg = "It's \"quoted\" $HOME `x`\n";
    let todo = vec![TodoItem {
        new_message: Some(s(msg)),
        ..item(TodoAction::Reword, &c[4], "five")
    }];
    t.repo.rebase_interactive("HEAD~1", &todo, false).unwrap();
    assert_eq!(t.git(&["log", "-1", "--format=%B"]), msg.trim_end());
}

#[test]
fn interactive_edit_stops() {
    let t = TestRepo::new();
    let c = five(&t);
    let todo = vec![
        item(TodoAction::Pick, &c[2], "three"),
        item(TodoAction::Edit, &c[3], "four"),
        item(TodoAction::Pick, &c[4], "five"),
    ];
    t.repo.rebase_interactive(&c[1], &todo, false).unwrap();
    assert_eq!(state(&t), RepoState::Rebasing { interactive: true });
    assert_eq!(t.subjects("-1"), ["four"]);
    t.commit_file("extra.txt", "e\n", "inserted while editing");
    t.repo.continue_op(state(&t)).unwrap();
    assert_eq!(state(&t), RepoState::Clean);
    assert_eq!(
        t.subjects("HEAD"),
        [
            "five",
            "inserted while editing",
            "four",
            "three",
            "two",
            "one",
            "base"
        ]
    );
}

#[test]
fn interactive_conflict_then_abort() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "0\n", "base");
    let a = t.commit_file("f.txt", "1\n", "a");
    let b = t.commit_file("f.txt", "2\n", "b");
    let head = t.head();
    // Swapping two commits that touch the same line conflicts.
    let todo = vec![
        item(TodoAction::Pick, &b, "b"),
        item(TodoAction::Pick, &a, "a"),
    ];
    let text = t.repo.rebase_interactive("HEAD~2", &todo, false).unwrap();
    assert!(text.to_lowercase().contains("conflict"), "{text}");
    assert_eq!(state(&t), RepoState::Rebasing { interactive: true });
    t.repo.abort_op(state(&t)).unwrap();
    assert_eq!(t.head(), head);
}

#[test]
fn interactive_runs_concurrently() {
    // Two rebases in the same process must not share todo files.
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let t = TestRepo::new();
                let c = five(&t);
                let todo = vec![
                    item(TodoAction::Pick, &c[4], "five"),
                    item(TodoAction::Pick, &c[3], "four"),
                ];
                t.repo.rebase_interactive("HEAD~2", &todo, false).unwrap();
                assert_eq!(t.subjects("HEAD")[..2], ["four", "five"]);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
