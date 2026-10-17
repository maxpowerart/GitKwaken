#![allow(clippy::field_reassign_with_default)]

use super::*;
use std::fs;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Points GITKWAKEN_CONFIG_DIR at a fresh tempdir; holds the lock for the test.
struct TestEnv {
    dir: tempfile::TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl TestEnv {
    fn new() -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: all env access in tests is serialized by ENV_LOCK.
        unsafe { std::env::set_var(CONFIG_DIR_ENV, dir.path()) };
        Self { dir, _guard: guard }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
}

#[test]
fn config_dir_env_override() {
    let env = TestEnv::new();
    assert_eq!(Settings::config_dir(), env.path());
    assert_eq!(Settings::path(), env.path().join("settings.toml"));
}

#[test]
fn defaults_roundtrip() {
    let env = TestEnv::new();
    assert!(!Settings::path().exists());
    assert_eq!(Settings::load(), Settings::default());

    let d = Settings::default();
    assert_eq!(d.window.size, [1400.0, 900.0]);
    assert_eq!(d.theme, Theme::Dark);
    assert_eq!(d.diff.max_file_kb, 20480);
    assert_eq!(d.history.date_format, "%Y-%m-%d %H:%M");
    assert_eq!(d.language, "en");
    d.save().unwrap();
    assert!(env.path().join("settings.toml").exists());
    assert_eq!(Settings::load(), d);

    // Fully populated settings roundtrip too.
    let mut s = Settings::default();
    s.theme = Theme::System;
    s.status_engine = StatusEngine::Gix;
    s.window.position = Some([10.0, 20.5]);
    s.git_path = Some(PathBuf::from(r"C:\Program Files\Git\bin\git.exe"));
    s.external_diff_tool = Some(ExternalTool {
        name: "Beyond Compare".into(),
        command: "bcomp".into(),
        args: "$LOCAL $REMOTE".into(),
    });
    s.commit.sign = Some(true);
    s.keymap.insert("commit".into(), "Ctrl+Enter".into());
    s.open_tabs = vec!["a".into(), "b".into()];
    s.active_tab = 1;
    s.add_bookmark("repo", "C:/x/repo", Some("Work".into()));
    s.add_recent("C:/x/repo");
    s.save().unwrap();
    assert_eq!(Settings::load(), s);
}

#[test]
fn partial_file_loads_with_defaults() {
    let env = TestEnv::new();
    fs::write(
        env.path().join("settings.toml"),
        r#"
theme = "light"
font_size = 15.0
unknown_top_level = 42

[diff]
context_lines = 7

[history]
row_height = 30.0
some_future_field = "x"
"#,
    )
    .unwrap();
    let s = Settings::load();
    assert_eq!(s.theme, Theme::Light);
    assert_eq!(s.font_size, 15.0);
    assert_eq!(s.diff.context_lines, 7);
    assert!(s.diff.word_diff);
    assert_eq!(s.diff.tab_width, 4);
    assert_eq!(s.history.row_height, 30.0);
    assert_eq!(s.history.graph_lane_width, 14.0);
    assert_eq!(s.ui_scale, 1.0);
    assert!(s.confirmations.force_push);
    assert_eq!(s.auto_fetch_minutes, 10);
    assert_eq!(s.mono_font_family, "Consolas");
    assert!(s.compact);
    assert!(s.diff.renames);
}

#[test]
fn appearance_and_keymap_fields_roundtrip() {
    let env = TestEnv::new();
    fs::write(
        env.path().join("settings.toml"),
        "mono_font_family = \"  \"\ncompact = false\n[diff]\nmax_file_kb = 0\nrenames = false\n[keymap]\nfetch = \"Ctrl+Shift+F\"\n",
    )
    .unwrap();
    let mut s = Settings::load();
    assert_eq!(s.mono_font_family, "Consolas", "blank family falls back");
    assert!(!s.compact);
    assert!(!s.diff.renames);
    assert_eq!(s.diff.max_file_kb, 20480, "0 KB falls back");
    assert_eq!(
        s.keymap.get("fetch").map(String::as_str),
        Some("Ctrl+Shift+F")
    );
    s.mono_font_family = "Cascadia Mono".into();
    s.save().unwrap();
    assert_eq!(Settings::load().mono_font_family, "Cascadia Mono");
}

#[test]
fn out_of_range_values_are_clamped() {
    let env = TestEnv::new();
    fs::write(
        env.path().join("settings.toml"),
        "ui_scale = 100.0\nactive_tab = 5\nopen_tabs = [\"a\"]\n",
    )
    .unwrap();
    let s = Settings::load();
    assert_eq!(s.ui_scale, 1.0);
    assert_eq!(s.active_tab, 0);
}

#[test]
fn corrupt_file_backed_up_and_defaults() {
    let env = TestEnv::new();
    let p = env.path().join("settings.toml");
    fs::write(&p, "this is = = not toml [[[").unwrap();
    let s = Settings::load();
    assert_eq!(s, Settings::default());
    assert!(!p.exists(), "broken file should be moved away");
    let backups: Vec<_> = fs::read_dir(env.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("settings.toml.bak-"))
        .collect();
    assert_eq!(backups.len(), 1);
    let content = fs::read_to_string(env.path().join(&backups[0])).unwrap();
    assert_eq!(content, "this is = = not toml [[[");

    // Wrong type for a field is also treated as corrupt.
    fs::write(&p, "font_size = \"big\"\n").unwrap();
    assert_eq!(Settings::load(), Settings::default());
}

#[test]
fn atomic_save_creates_dirs_and_leaves_no_temp() {
    let env = TestEnv::new();
    let nested = env.path().join("a").join("b").join("settings.toml");
    let mut s = Settings::default();
    s.font_size = 20.0;
    s.save_to(&nested).unwrap();
    assert_eq!(Settings::load_from(&nested).font_size, 20.0);

    // Overwrite existing file.
    s.font_size = 21.0;
    s.save_to(&nested).unwrap();
    assert_eq!(Settings::load_from(&nested).font_size, 21.0);

    let names: Vec<_> = fs::read_dir(nested.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["settings.toml".to_string()]);
}

#[test]
fn recent_cap_and_dedup() {
    let mut s = Settings::default();
    for i in 0..30 {
        s.add_recent(format!("C:/repos/r{i}"));
    }
    assert_eq!(s.recent_repos.len(), MAX_RECENT_REPOS);
    assert_eq!(s.recent_repos[0], PathBuf::from("C:/repos/r29"));
    assert_eq!(s.recent_repos[19], PathBuf::from("C:/repos/r10"));

    s.add_recent("C:/repos/r15");
    assert_eq!(s.recent_repos.len(), MAX_RECENT_REPOS);
    assert_eq!(s.recent_repos[0], PathBuf::from("C:/repos/r15"));
    assert_eq!(
        s.recent_repos
            .iter()
            .filter(|p| *p == Path::new("C:/repos/r15"))
            .count(),
        1
    );

    // Trailing separator is treated as the same path.
    s.add_recent("C:/repos/r20/");
    assert_eq!(s.recent_repos.len(), MAX_RECENT_REPOS);
    assert_eq!(s.recent_repos[0], PathBuf::from("C:/repos/r20/"));

    s.remove_recent(Path::new("C:/repos/r20"));
    assert_eq!(s.recent_repos.len(), MAX_RECENT_REPOS - 1);
}

#[test]
fn bookmark_helpers() {
    let mut s = Settings::default();
    assert!(s.add_bookmark("", "C:/code/alpha", None));
    assert_eq!(s.bookmarks[0].name, "alpha");
    assert!(!s.add_bookmark("dup", "C:/code/alpha", None));
    assert!(s.add_bookmark("Beta", "C:/code/beta", Some("Work".into())));
    assert_eq!(s.groups, vec!["Work".to_string()]);

    assert!(s.rename_bookmark(Path::new("C:/code/alpha"), "Alpha!"));
    assert!(!s.rename_bookmark(Path::new("C:/code/alpha"), "  "));
    assert!(!s.rename_bookmark(Path::new("C:/nope"), "x"));
    assert_eq!(
        s.find_bookmark(Path::new("C:/code/alpha")).unwrap().name,
        "Alpha!"
    );

    assert!(s.move_bookmark_to_group(Path::new("C:/code/alpha"), Some("Personal".into())));
    assert_eq!(s.groups, vec!["Work".to_string(), "Personal".to_string()]);
    assert_eq!(s.bookmarks[0].group.as_deref(), Some("Personal"));
    assert!(s.move_bookmark_to_group(Path::new("C:/code/alpha"), None));
    assert_eq!(s.bookmarks[0].group, None);
    assert!(!s.move_bookmark_to_group(Path::new("C:/nope"), None));

    assert!(s.rename_group("Work", "Job"));
    assert_eq!(s.bookmarks[1].group.as_deref(), Some("Job"));
    assert!(s.remove_group("Job"));
    assert_eq!(s.bookmarks[1].group, None);

    assert!(s.remove_bookmark(Path::new("C:/code/beta")));
    assert!(!s.remove_bookmark(Path::new("C:/code/beta")));
    assert_eq!(s.bookmarks.len(), 1);
}

#[test]
fn external_tool_args() {
    let t = ExternalTool {
        name: "kdiff3".into(),
        command: "kdiff3".into(),
        args: "$BASE $LOCAL $REMOTE -o $MERGED".into(),
    };
    assert_eq!(
        t.expand_args("l", "r", "b", "m"),
        "\"b\" \"l\" \"r\" -o \"m\""
    );
}

#[test]
fn repo_state_roundtrip_and_path() {
    let env = TestEnv::new();
    let wd = env.path().join("work").join("repo");
    fs::create_dir_all(&wd).unwrap();

    assert_eq!(RepoState::load(&wd), RepoState::default());

    let mut st = RepoState::default();
    st.commit_message_draft = "wip: thing\n\nbody".into();
    st.push_recent_message("first");
    st.sidebar_collapsed.insert("tags".into());
    st.history_scope = "current".into();
    st.layout.sidebar_width = 300.0;
    st.last_view = "changes".into();
    st.commit_graph_offer_dismissed = true;
    st.file_view_tree = true;
    st.save(&wd).unwrap();
    assert_eq!(RepoState::load(&wd), st);

    // Path: <config>/repos/<16 hex>.toml, stable across calls and spellings.
    let p = RepoState::path_for(&wd);
    assert!(p.exists());
    assert_eq!(p.parent().unwrap(), env.path().join("repos"));
    let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
    assert_eq!(stem.len(), 16);
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(RepoState::key_for(&wd), stem);
    assert_eq!(RepoState::key_for(&wd.join("..").join("repo")), stem);
    assert_eq!(RepoState::key_for(&wd.join(".")), stem);

    let other = env.path().join("work").join("other");
    fs::create_dir_all(&other).unwrap();
    assert_ne!(RepoState::key_for(&other), stem);

    // Hash algorithm is fixed (FNV-1a over the normalized path).
    assert_eq!(
        format!("{:016x}", crate::io::fnv1a64(b"hello")),
        "a430d84680aabd0b"
    );
}

#[test]
fn repo_state_partial_and_corrupt() {
    let env = TestEnv::new();
    let wd = env.path().join("r");
    fs::create_dir_all(&wd).unwrap();
    let p = RepoState::path_for(&wd);
    fs::create_dir_all(p.parent().unwrap()).unwrap();

    fs::write(
        &p,
        "last_view = \"stash\"\nextra = 1\n[layout]\nlist_split = 0.7\n",
    )
    .unwrap();
    let st = RepoState::load(&wd);
    assert_eq!(st.last_view, "stash");
    assert_eq!(st.layout.list_split, 0.7);
    assert_eq!(st.layout.sidebar_width, Layout::default().sidebar_width);
    assert_eq!(st.history_scope, "all");

    fs::write(&p, "[[[").unwrap();
    assert_eq!(RepoState::load(&wd), RepoState::default());
    assert!(!p.exists());
}

#[test]
fn recent_messages_cap_dedup() {
    let mut st = RepoState::default();
    for i in 0..40 {
        st.push_recent_message(format!("msg {i}"));
    }
    assert_eq!(st.recent_messages.len(), MAX_RECENT_MESSAGES);
    assert_eq!(st.recent_messages[0], "msg 39");
    st.push_recent_message("  msg 20 \n");
    assert_eq!(st.recent_messages[0], "msg 20");
    assert_eq!(st.recent_messages.len(), MAX_RECENT_MESSAGES);
    assert_eq!(
        st.recent_messages.iter().filter(|m| *m == "msg 20").count(),
        1
    );
    st.push_recent_message("   ");
    assert_eq!(st.recent_messages[0], "msg 20");
}

#[test]
fn autosave_debounce() {
    let mut s = Settings::default();
    let mut saver = Autosave::new(&s);
    let mut saves = 0;
    let t0 = Instant::now();
    let mut count = |_: &Settings| -> anyhow::Result<()> {
        saves += 1;
        Ok(())
    };

    // Unchanged -> no save.
    assert!(!saver.maybe_save_at(t0, &s, &mut count));
    assert!(!saver.is_dirty(&s));

    // Changed -> first save immediate.
    s.font_size = 14.0;
    assert!(saver.is_dirty(&s));
    assert!(saver.maybe_save_at(t0, &s, &mut count));

    // Changed again within 2s -> deferred.
    s.font_size = 15.0;
    assert!(!saver.maybe_save_at(t0 + Duration::from_millis(500), &s, &mut count));
    assert!(!saver.maybe_save_at(t0 + Duration::from_millis(1999), &s, &mut count));
    // After 2s -> saved.
    assert!(saver.maybe_save_at(t0 + Duration::from_secs(2), &s, &mut count));
    // Unchanged after interval -> nothing.
    assert!(!saver.maybe_save_at(t0 + Duration::from_secs(10), &s, &mut count));

    // Failed save keeps it dirty and retries after the interval.
    s.font_size = 16.0;
    let t1 = t0 + Duration::from_secs(20);
    assert!(!saver.maybe_save_at(t1, &s, |_| anyhow::bail!("disk full")));
    assert!(saver.is_dirty(&s));
    assert!(!saver.maybe_save_at(t1 + Duration::from_secs(1), &s, &mut count));
    assert!(saver.maybe_save_at(t1 + Duration::from_secs(2), &s, &mut count));

    // flush ignores debounce.
    s.font_size = 17.0;
    assert!(saver.flush(&s, &mut count));
    assert!(!saver.flush(&s, &mut count));

    drop(count);
    assert_eq!(saves, 4);
}

#[test]
fn autosave_with_real_files() {
    let _env = TestEnv::new();
    let mut s = Settings::default();
    let mut saver = Autosave::default().with_interval(Duration::ZERO);
    assert!(saver.maybe_save(&s, |v| v.save()));
    s.theme = Theme::Light;
    assert!(saver.maybe_save(&s, |v| v.save()));
    assert_eq!(Settings::load().theme, Theme::Light);
}
