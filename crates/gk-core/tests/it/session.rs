use crate::common::*;
use gk_core::content::DiffTarget;
use gk_core::ops::CommitOptions;
use gk_core::{Head, Load, Session, SessionOptions, Snapshot};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

fn open(t: &TestRepo) -> (Session, Arc<AtomicUsize>) {
    let repaints = Arc::new(AtomicUsize::new(0));
    let r = repaints.clone();
    let ss = Session::open(t.repo.clone(), SessionOptions::default(), move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    (ss, repaints)
}

fn wait_snap(ss: &Session, what: &str, mut cond: impl FnMut(&Snapshot) -> bool) -> Snapshot {
    let mut last = None;
    let ok = wait_until(T, || {
        let snap = ss.snapshot();
        let hit = cond(&snap);
        last = Some(snap);
        hit
    });
    let snap = last.unwrap();
    assert!(ok, "timed out waiting for {what}; messages: {:?}", snap.messages.iter().map(|m| &m.text).collect::<Vec<_>>());
    snap
}

fn ready<T>(what: &str, mut f: impl FnMut() -> Load<T>) -> Arc<T> {
    let mut out = None;
    let ok = wait_until(T, || match f() {
        Load::Ready(v) => {
            out = Some(v);
            true
        }
        Load::Failed(e) => panic!("{what} failed: {e}"),
        Load::Loading => false,
    });
    assert!(ok, "timed out waiting for {what}");
    out.unwrap()
}

fn loaded(snap: &Snapshot, commits: usize) -> bool {
    snap.refs.head == Head::Branch(s("main"))
        && snap.history.len() == commits
        && snap.status_gen > 0
        && !snap.status_loading
        && !snap.history_loading
}

#[test]
fn session_end_to_end() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "first");
    t.commit_file("b.txt", "b\n", "second with NEEDLE");
    let (ss, repaints) = open(&t);
    let snap = wait_snap(&ss, "initial load", |sn| loaded(sn, 2));
    assert!(snap.status.is_clean());
    assert!(repaints.load(Ordering::SeqCst) > 0);
    assert_eq!(ss.repo().workdir(), t.repo.workdir());

    // Watcher-driven refresh.
    let gen0 = snap.status_gen;
    t.write("a.txt", "changed\n");
    t.write("new.txt", "n\n");
    let snap = wait_snap(&ss, "status after edit", |sn| {
        sn.status_gen > gen0 && sn.status.unstaged.iter().any(|e| e.path == "a.txt") && sn.status.unstaged.iter().any(|e| e.path == "new.txt")
    });
    let a = snap.status.unstaged.iter().find(|e| e.path == "a.txt").unwrap();
    assert_eq!(a.kind, gk_status::Kind::Modified);

    // Diff: Loading, then Ready.
    let target = DiffTarget::Unstaged { path: s("a.txt") };
    assert!(matches!(ss.diff(&target, snap.status_gen, false, 3), Load::Loading));
    let d = ready("diff", || ss.diff(&target, snap.status_gen, false, 3));
    assert_eq!((d.additions, d.deletions), (1, 1));

    // Index op then a commit through the queue.
    ss.run_index_op("stage", |r| r.stage(&[s("a.txt"), s("new.txt")]));
    ss.run_op("commit", |r, _c, _p| r.commit(&CommitOptions { message: s("third"), ..Default::default() }));
    let snap = wait_snap(&ss, "history after commit", |sn| loaded(sn, 3) && sn.status.is_clean() && sn.op.is_none());
    let head = t.oid("HEAD");
    assert_eq!(snap.history.ids[0], head);
    assert_eq!(snap.refs.head_id.as_deref(), Some(t.head().as_str()));

    let details = ready("details", || ss.commit_details(head));
    assert_eq!(details.message.trim(), "third");
    assert_eq!(details.files.len(), 2);

    let rows = ready("search", || ss.search("needle"));
    assert_eq!(*rows, vec![snap.history.row(&t.oid("HEAD~1")).unwrap() as u32]);

    let metas = ss.metas(&snap.history.ids);
    assert_eq!(metas.len(), 3);
    assert_eq!(ss.meta().get_sync(head).unwrap().summary, "third");

    // A failing op reports an error message.
    ss.run_op("bad checkout", |r, _c, _p| r.checkout("no-such-branch").map(|_| String::new()));
    let snap = wait_snap(&ss, "error message", |sn| sn.messages.iter().any(|m| m.text.contains("bad checkout failed")));
    assert_eq!(snap.messages.last().unwrap().kind, gk_core::MessageKind::Error);
    ss.dismiss_messages();
    assert!(ss.snapshot().messages.is_empty());
}

#[test]
fn session_external_ref_change() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "first");
    let (ss, _) = open(&t);
    wait_snap(&ss, "initial load", |sn| loaded(sn, 1));
    // Changes made by another tool are picked up through the .git watcher.
    t.commit_file("b.txt", "b\n", "external");
    t.git(&["tag", "v1"]);
    wait_snap(&ss, "external commit", |sn| loaded(sn, 2) && sn.refs.tags.len() == 1);
}

#[test]
fn session_without_watcher_and_scope() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "first");
    t.git(&["checkout", "-q", "-b", "side"]);
    t.commit_file("b.txt", "b\n", "side work");
    t.git(&["checkout", "-q", "main"]);
    let opts = SessionOptions { watch: false, ..Default::default() };
    let ss = Session::open(t.repo.clone(), opts.clone(), || {});
    wait_snap(&ss, "all branches", |sn| loaded(sn, 2));
    ss.set_options(SessionOptions { scope: gk_core::history::Scope::Current, ..opts });
    wait_snap(&ss, "current branch only", |sn| loaded(sn, 1));
    t.write("a.txt", "x\n");
    ss.refresh_status();
    wait_snap(&ss, "manual status refresh", |sn| sn.status.unstaged.len() == 1);
}

#[test]
fn session_conflict_message() {
    let t = TestRepo::new();
    crate::refs::conflicting_branches(&t);
    let (ss, _) = open(&t);
    wait_snap(&ss, "initial load", |sn| loaded(sn, 3));
    ss.run_op("merge", |r, _c, _p| {
        r.merge("topic", &gk_core::ops::MergeOptions { commit: true, ..Default::default() })
    });
    let snap = wait_snap(&ss, "merge state", |sn| sn.refs.state == gk_core::RepoState::Merging && sn.status.conflicts().count() == 1);
    assert!(snap.messages.iter().any(|m| m.text.contains("conflicts")));
}

#[test]
fn session_drop_releases_everything() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "first");
    let token = Arc::new(());
    let tk = token.clone();
    let ss = Session::open(t.repo.clone(), SessionOptions::default(), move || {
        let _ = &tk;
    });
    wait_snap(&ss, "initial load", |sn| loaded(sn, 1));
    let s2 = ss.clone();
    drop(ss);
    assert_eq!(s2.snapshot().history.len(), 1, "clones stay usable");
    drop(s2);
    assert!(wait_until(T, || Arc::strong_count(&token) == 1), "session threads did not shut down");
}
