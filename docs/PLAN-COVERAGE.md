# GitKwaken: design-document coverage

Audit date: 2026-10-17. Source: workspace at `D:\Projects\GitKwaken` (code read only, nothing changed).

Status legend:

- **Done**: implemented and reachable from the UI (the entry point is named in Evidence).
- **Partial**: implemented in part, not reachable from the UI, or a budget that was not measured or not met.
- **Missing**: not implemented.
- **Deviation**: implemented differently from the design.

Evidence uses `impl:` for the code, `UI:` for the entry point, `test:` for unit/integration tests (`crate/path.rs::fn`), `E2E:` for UI scenarios (`scripts/e2e/*.script`, run by `scripts/run-e2e*.sh`). Paths are relative to the repo root. `it/` = `crates/gk-core/tests/it/`.

Measurements made for this audit with `target/lead/release/gk-bench.exe` and the release `gitkwaken.exe`:

- `gk-bench history bench-data/gitgit`: refs::load 40.3 ms, history::build 97.2 ms (85,620 commits), layout 1.7 MB, search "fix" 75.6 ms.
- `gk-bench status bench-data/r1m 3`: git status 407 ms; gix engine (the default) 898 ms full; CLI engine 396 ms full; fast path 0.2 ms with a cached index (110.8 ms when the index has to be loaded).
- Peak working set 8 s after opening the History view: demo 107 MB, git.git 133 MB, r1m 368 MB.

## Functionality

### Repositories

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Bookmarks with groups | Partial | impl: `crates/gk-app/src/dialogs/global.rs:413` (Bookmark dialog with group combo), `crates/gk-settings/src/lib.rs:29,96`; UI: Welcome page, right-click a Recent repo, "Bookmark" (`crates/gk-app/src/views/welcome.rs:91`); test: `crates/gk-settings/src/tests.rs::bookmark_helpers` | Groups are saved but the Welcome list is flat (`welcome.rs:55`), with no group headings. Bookmarks are visible only when no tab is open. There is no bookmarks sidebar or menu. |
| Tabs | Done | impl: `crates/gk-app/src/app.rs:180` (tab bar), `app.rs:278` (Ctrl+W, Ctrl+Tab), `app.rs:174` (open tabs persisted); UI: tab strip, "+", middle-click | No automated test. |
| Init | Done | impl: `crates/gk-app/src/dialogs/global.rs:335`, `crates/gk-core/src/repo.rs:98`; UI: Welcome "Create new…", palette "Create repository…"; test: `it/repo.rs::init_new_repo` | |
| Clone (shallow, partial `--filter=blob:none`, sparse) | Partial | impl: `crates/gk-app/src/dialogs/global.rs:74,113,116`; UI: tab bar "Clone…", Welcome, palette; E2E: `r-clone.script` (plain clone only) | Shallow and partial are present. There is no sparse clone (`--sparse` plus cone setup). Shallow and partial clones are not tested. |
| Open folder | Done | impl: `crates/gk-app/src/app.rs:128`; UI: Ctrl+O, "+" in the tab bar, Welcome "Open repository…"; test: `it/repo.rs::open_from_subdir_and_failures` | |
| Scan disk for repositories | Missing | (none) | No folder scan anywhere. |

### Working copy

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Staged / unstaged lists | Done | impl: `crates/gk-app/src/views/file_status.rs:408`; UI: File Status view; test: `file_status.rs::renders_headless`; E2E: `commit.script`, `drag.script` | |
| Tree and flat view | Done | impl: `crates/gk-app/src/views/file_status.rs:279` ("Tree" toggle), `file_status/tree.rs`; test: `tree.rs::builds_nested_tree_dirs_first`, `file_status.rs::renders_headless` | |
| Stage / unstage / discard file | Done | impl: `crates/gk-core/src/ops.rs:16,32,74`; UI: row checkbox, context menu (`file_status/list.rs:780-833`), toolbar Discard; test: `it/ops.rs::stage_unstage_with_head`, `discard_tracked_and_untracked`; E2E: `commit.script`, `r-discard-file.script`, `r-discard-untracked.script`, `r-discard-all.script` | |
| Stage / unstage / discard hunk | Done | impl: `crates/gk-app/src/views/diff.rs:933-939`; UI: hunk header buttons; test: `diff.rs::hunk_actions_on_a_real_repository` | |
| Stage / unstage / discard line | Done | impl: `crates/gk-app/src/views/diff.rs:600-640`, `crates/gk-core/src/ops.rs:99,171`; UI: line selection, context menu; test: `it/ops.rs::stage_lines_subset`, `discard_lines_in_worktree`; E2E: `stage-line.script` | |
| Ignore by mask | Partial | impl: `crates/gk-app/src/views/file_status/list.rs:836-855`, `crates/gk-core/src/ops.rs:704`; UI: context menu, "Ignore" submenu; test: `it/ops.rs::ignore_appends` | Only preset patterns are offered (`/file`, `*.ext`, `/dir/`). You cannot type your own mask. |
| Filters by status | Partial | impl: `crates/gk-app/src/views/file_status.rs:287` (path text filter); Settings "Show untracked files" | No filter by status (modified, added, untracked, conflicted). |

### Commit

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Amend | Done | impl: `crates/gk-app/src/views/file_status/commit.rs:262`; test: `it/ops.rs::commit_amend_signoff`; E2E: `r-amend.script` | |
| Sign-off | Done | impl: `commit.rs:276`, default in Settings (`dialogs/settings.rs:287`); test: `it/ops.rs::commit_amend_signoff` | |
| GPG/SSH signing | Done | impl: `crates/gk-core/src/ops.rs:212-216` (`-S`), `dialogs/settings.rs:289` (Always / Never / Follow config) | Global setting only, no per-commit toggle. SSH signing works through `gpg.format`. No test. |
| Commit templates (`commit.template`) | Missing | (none) | The box is not prefilled. `commit -F -` bypasses the template. |
| History of messages | Done | impl: `commit.rs:187` ("Recent ▾"), `crates/gk-settings/src/repo_state.rs`; test: `crates/gk-settings/src/tests.rs::recent_messages_cap_dedup` | |
| Author per commit (override) | Partial | impl: `crates/gk-core/src/ops.rs:828` (`CommitOptions.author`) | Exists in core only. The commit box never sets it (`commit.rs:317`). No test. |
| Hooks with visible output | Partial | impl: commit through the git CLI (`ops.rs:225-227`, blocking `run`) | Hooks run, but successful output is dropped (`crates/gk-core/src/session.rs:688` reports only conflicts). Output shows only on failure or in the Output panel. Nothing streams live. No UI for `--no-verify`. |
| Commit and push | Done | impl: `commit.rs:282,324-340`; test: `commit.rs::push_target_uses_upstream_or_first_remote`; E2E: `r-commit-push.script` | |

### Stash

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Push (incl. partial, keep-index) | Done | impl: `crates/gk-app/src/dialogs/worktree.rs:217,273,277`; UI: toolbar Stash, STASHES header, file context menu "Stash…" (`list.rs:832`, partial); test: `it/ops.rs::stash_keep_index_and_restore_index`, `stash_untracked_and_paths`; E2E: `r-stash.script` | |
| Apply / pop / drop | Done | impl: `worktree.rs:367,408,428`; UI: sidebar stash context menu (`views/sidebar.rs:1106-1112`); test: `it/ops.rs::stash_push_apply_pop_drop`; E2E: `r-stash-apply.script`, `r-stash-pop.script`, `r-stash-drop.script` | |
| View stash diff | Missing | `crates/gk-app/src/views/sidebar.rs:1099` (click only selects) | Stash commits are not history tips (`crates/gk-core/src/refs.rs:138`), so the diff cannot be shown. |
| Branch from stash | Done | impl: `worktree.rs:479`; UI: `sidebar.rs:1113`; test: `it/ops.rs::stash_apply_conflict_and_branch` | No E2E. |

### History

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Graph | Done | impl: `crates/gk-core/src/history.rs:46`, `crates/gk-graph/src/layout.rs`, `views/history/graph.rs`; test: `history/ui_tests.rs::history_view_selection_keyboard_and_worktree_row`, `layout.rs::*`; E2E: all `h-*.script` | |
| Filter: branches | Partial | impl: `crates/gk-app/src/views/history.rs:247` (All / Current toggle) | `Scope::Revs` (`history.rs:138`) exists but the UI never uses it. You cannot pick specific branches. |
| Filter: author | Partial | impl: `crates/gk-core/src/history.rs:261` (one search box matches message, author, email and SHA) | No author-only filter. |
| Filter: date | Missing | (none) | |
| Filter: path | Partial | impl: file history for one file (`views/search.rs:113`) | No path filter on the graph and no directory or pathspec filter. |
| Filter: text | Done | impl: `views/search.rs:155`, `history.rs:261`; UI: Search view, Ctrl+3; test: `it/history.rs::search_message_author_id`; E2E: `h-search.script` | |
| Filter: pickaxe `-S` / `-G` | Missing | (none) | |
| Compare two commits | Done | impl: `crates/gk-app/src/views/details.rs:266`; UI: select two commits in History; E2E: `h-keyboard.script` (Shift+Down, then compare) | |
| File history | Done | impl: `views/search.rs:113,220` (`git log --follow`); UI: file context menu "File history" (`list.rs:877`); test: `search.rs::file_log_follows_renames`; E2E: `h-file-history.script` | |
| Blame | Done | impl: `crates/gk-app/src/views/blame.rs:56`, `crates/gk-core/src/advanced.rs:250`; UI: file context menu "Blame" (`list.rs:880`), palette "Blame <path>"; test: `blame.rs::runs_and_heat`, `advanced.rs::blame_porcelain`; E2E: `r-blame.script` | |
| Reflog | Missing | (none) | Only reachable through the git console. |

### Branches and tags

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Create / rename / delete | Done | impl: `crates/gk-app/src/dialogs/branch.rs`, `crates/gk-core/src/ops.rs:256-277`; UI: toolbar Branch, sidebar branch menu (`sidebar.rs:1335`), History "Create branch here…"; test: `it/ops.rs::branches`; E2E: `branch.script`, `h-create-branch.script`, `h-sidebar-rename.script`, `h-sidebar-delete.script` | |
| Checkout (incl. detached) | Done | impl: `branch.rs` Checkout; UI: double-click in History or sidebar, menus, palette "Checkout <b>"; test: `it/refs.rs::branch_and_detached_head`; E2E: `h-checkout-detached.script`, `h-sidebar-checkout.script`, `h-select-checkout.script` | |
| Tracking | Done | impl: `ops.rs:249,283`; UI: "Track remote branch…" (`sidebar.rs:1359`), "Checkout as tracking branch…", Push dialog "Track" column; test: `it/remote.rs::checkout_remote_and_set_upstream`; E2E: `r-push-branch.script` | |
| Ahead / behind | Done | impl: `crates/gk-core/src/refs.rs:176`, badges at `sidebar.rs:919`; test: `it/refs.rs::upstream_ahead_behind_and_remote_branches`; E2E: `r-fetch.script` ("↓ 1") | |
| Tags: lightweight / annotated / signed | Done | impl: `crates/gk-app/src/dialogs/tag.rs:74-99`, `ops.rs:310`; UI: toolbar Tag, History "Create tag here…", sidebar tag menu; test: `it/ops.rs::tags`, `it/remote.rs::push_tags_and_delete`; E2E: `h-create-tag.script`, `r-tag-push.script` | Signed tags are not tested. |
| git-flow | Done | impl: `crates/gk-app/src/dialogs/flow.rs:45`, `advanced.rs:496-538`; UI: toolbar "Git Flow", palette; test: `advanced.rs::flow_classify`, `flow.rs::master_choice`, `bases` | Unit tests only. No integration test or E2E. |

### History rewriting

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Merge (ff / no-ff / squash) | Done | impl: `crates/gk-app/src/dialogs/rewrite.rs:122`, `ops.rs:339`; UI: toolbar Merge, History "Merge into current…", sidebar; test: `it/ops.rs::merge_fast_forward_and_no_ff`, `merge_squash_and_no_commit`; E2E: `h-merge.script` | |
| Rebase onto | Done | impl: `rewrite.rs` Rebase, `ops.rs:363`; UI: History or sidebar "Rebase current onto…"; test: `it/rebase.rs::rebase_clean`; E2E: `h-rebase.script` | |
| Interactive rebase by drag-and-drop | Done | impl: `rewrite.rs:276,478` (drag ☰, ↑/↓), `ops.rs:373`; UI: History "Interactive rebase children of this commit…" (`history.rs:560`); test: `it/rebase.rs::interactive_reorder_and_plain_reword`, `interactive_reword_squash_fixup_drop`; E2E: `h-interactive-rebase.script` | |
| Cherry-pick | Done | impl: `rewrite.rs:618`; UI: `history.rs:571`; test: `it/ops.rs::cherry_pick_clean_and_conflict`; E2E: `h-cherry-pick.script` | |
| Revert | Done | impl: `rewrite.rs:618`; UI: `history.rs:574`; test: `it/ops.rs::revert_clean_and_conflict`; E2E: `h-revert.script` | |
| Reset soft / mixed / hard | Done | impl: `rewrite.rs:717`; UI: `history.rs:577`; test: `it/ops.rs::reset_modes`; E2E: `h-reset-mixed.script`, `h-reset-hard.script` | |
| Undo last operation via reflog | Missing | (none) | |

### Conflicts

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Conflict list | Done | impl: `Kind::Conflict` in `crates/gk-status/src/lib.rs`, File Status banner (`file_status.rs:222`); test: `it/refs.rs::state_merge_conflict`, `it/session.rs::session_conflict_message`; E2E: `conflict-guard.script` | |
| Ours / theirs per file | Done | impl: `list.rs:798-802`, `ops.rs:504`; UI: context menu "Resolve using mine / theirs"; test: `it/ops.rs::merge_conflict_ours_mark_resolved_abort` | |
| Built-in 3-way merge | Done | impl: `crates/gk-app/src/views/merge.rs:78`, `views/merge/diff3.rs`; UI: double-click a conflicted file, "Resolve in merge editor" (`list.rs:808`), palette; test: `diff3.rs::*` (15 tests); E2E: `merge.script`, `r-merge-ours.script`, `r-merge-both.script` | |
| External merge tool | Done | impl: `views/file_status/merge_tool.rs`, `file_status.rs:519`; UI: context menu (`list.rs:818`), Settings tools; test: `merge_tool.rs::splits_quoted_args` | No E2E. |
| Continue / skip / abort | Done | impl: `file_status.rs:654-716`, `ops.rs:429-458`; test: `it/ops.rs::merge_conflict_resolve_continue`, `cherry_pick_conflict_skip_and_abort`, `it/rebase.rs::rebase_conflict_continue_skip_abort` | |

### Remotes

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Fetch (all, prune) | Done | impl: `crates/gk-app/src/dialogs/remote.rs:506,509`, `ops.rs:575`; UI: toolbar Fetch, sidebar "Fetch from X"; test: `it/remote.rs::push_set_upstream_and_fetch`, `cancelled_fetch`; E2E: `r-fetch.script` | |
| Pull (merge / rebase) | Done | impl: `remote.rs:149`, `ops.rs:597`; UI: toolbar Pull; test: `it/remote.rs::pull_merge_and_rebase`; E2E: `r-pull-merge.script`, `r-pull-rebase.script` | |
| Push (force-with-lease, tags, to another branch) | Done | impl: `remote.rs:300-460` (editable remote branch per row, 372, 373); test: `it/remote.rs::push_rejected_then_force_with_lease`, `push_tags_and_delete`; E2E: `r-push.script`, `r-force-push.script`, `r-push-branch.script`, `r-tag-push.script` | |
| Manage remotes | Done | impl: `remote.rs:533,629` (Add, Edit, Remove); UI: REMOTES "+", "Edit…", "Remove…"; test: `it/remote.rs::remote_management`; E2E: `r-remote-add.script`, `r-remote-edit.script`, `r-remote-remove.script` | |
| Auto-fetch on a timer | Done | impl: `crates/gk-app/src/tab.rs:141`; UI: Settings "Auto fetch (minutes)" (`settings.rs:246`) | No test. It runs on an ad-hoc thread (`tab.rs:156`) outside the serialized op queue. |

### Authentication

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Git Credential Manager | Partial | Implicit: git runs its configured `credential.helper`; `GIT_TERMINAL_PROMPT=0` (`crates/gk-cli/src/lib.rs:141`) | No GCM detection or setup and no test. It works only if GCM is configured. |
| SSH agent | Partial | Implicit: OpenSSH from Git for Windows | No agent detection, key list or diagnostics. Passphrases go through `SSH_ASKPASS`. |
| Password dialog via GIT_ASKPASS | Done | impl: `crates/gk-askpass/src/server.rs:92-98`, `crates/gk-app/src/app.rs:232`; test: `crates/gk-askpass/tests/askpass.rs::git_http_basic_auth_via_askpass`, `ssh_keygen_passphrase_via_ssh_askpass`, `server.rs::answer_and_cancel` | |

### Advanced

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Submodules | Done | impl: sidebar SUBMODULES (open, update; `sidebar.rs:59`, `Cmd::SubmoduleUpdate`), `dialogs/repo_tools.rs` SubmoduleAdd; UI: More ▸ "Add submodule…"; test: `sidebar/model.rs::submodule_status`, `advanced.rs::submodule_status` | Parser tests only. |
| Worktrees | Done | impl: sidebar WORKTREES (add, remove, open; `sidebar.rs:792,1381`), `advanced.rs:271-298`; test: `advanced.rs::worktree_porcelain`, `sidebar/model.rs::worktree_porcelain` | Parser tests only. |
| LFS (lock / unlock) | Done | impl: `crates/gk-app/src/dialogs/lfs.rs:53,245,278`, `advanced.rs:408`; UI: More ▸ "LFS…", palette; test: `lfs.rs::lfs_patterns` | No integration test. |
| Bisect | Done | impl: `crates/gk-app/src/dialogs/bisect.rs:93`, `advanced.rs:360-379`; UI: More ▸ "Bisect…", toolbar badge while bisecting; test: `bisect.rs::log_parsing`, `output_parsing` | Parser tests only. |
| Sparse checkout | Done | impl: `dialogs/repo_tools.rs:41`, `advanced.rs:426-446`; UI: More ▸ "Sparse checkout…"; test: `repo_tools.rs::sparse_dirs` | |
| Patch create / apply | Done | impl: `crates/gk-app/src/dialogs/misc.rs:24,125`; UI: History "Create patch…" (`history.rs:581`), More ▸ "Apply patch…"; test: `it/ops.rs::format_and_apply_patch` | |
| Archive | Done | impl: `misc.rs:185`, `ops.rs:756`; UI: History "Archive…" (`history.rs:584`); test: `it/ops.rs::archive_formats` | |
| Notes | Missing | (none) | Only reachable through the git console. |
| One-click repo optimization (fsmonitor, untrackedCache, commit-graph, multi-pack-index, maintenance) | Done | impl: `dialogs/repo_tools.rs:391`, `crates/gk-core/src/advanced.rs:611-641`; UI: More ▸ "Optimize repository…", palette; test: `repo_tools.rs::optimization_checklist` | `optimize()` has no integration test. |

### Tools

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Terminal and file explorer in repo folder | Done | impl: `crates/gk-app/src/views/toolbar.rs:134-145,175`; UI: toolbar Terminal and Explorer, palette | |
| External diff / merge tools | Partial | impl: merge tool works (`file_status.rs:519`); `external_diff_tool` (`crates/gk-settings/src/lib.rs:46`) can be edited in Settings but nothing reads it | No "Open in external diff tool" action. |
| Editor for .gitconfig / .gitignore | Partial | impl: Settings edits only `user.name` and `user.email` (`settings.rs:362`); `.gitignore` is appended through the Ignore menu | No text editor for either file. |
| Log of all executed git commands | Done | impl: `crates/gk-cli/src/log.rs`, `crates/gk-app/src/views/log.rs:5`; UI: "Output" toggle (`app.rs:221`), palette "Toggle output log"; test: `crates/gk-cli/src/lib.rs::runs_git_and_logs` | |
| Command palette | Done | impl: `crates/gk-app/src/views/palette.rs`; UI: Ctrl+Shift+P (`app.rs:285`); test: `palette.rs::fuzzy_scoring`; E2E: `palette.script` | |
| Built-in git console | Done | impl: `misc.rs:366`; UI: More ▸ "Git console…", palette; test: `misc.rs::destructive_commands`, `common.rs::argument_splitting` | |

## UI/UX

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| SourceTree layout (sidebar, toolbar, history graph top and details bottom, File Status staged top / unstaged bottom / diff right / commit box bottom, Search view) | Done | impl: `crates/gk-app/src/tab.rs:205-224`, `views/history.rs:98,217`, `file_status.rs:224,231,240,408`, `views/search.rs:155`; test: `file_status.rs::renders_headless`, `history/ui_tests.rs` | |
| SourceTree-style context menus | Done | impl: `history.rs:511`, `sidebar.rs:1291,1335`, `file_status/list.rs:761-912`, `diff.rs:600` | |
| Drag a branch onto a branch to merge or rebase | Missing | (none) | Drag-and-drop exists only for files between lists (`file_status/dnd.rs`) and in the interactive-rebase list. |
| Every action keyboard-accessible | Partial | impl: `app.rs:278` (Ctrl+O, W, Tab, Shift+P), `tab.rs:172` (F5, Ctrl+1/2/3), Ctrl+Enter commit, History keys; E2E: `h-keyboard.script` | The palette lacks cherry-pick, revert, reset, tag, stash apply/pop/drop, blame-from-History, worktree and submodule actions. These are mouse-only. |
| Command palette Ctrl+Shift+P | Done | `app.rs:285`; E2E: `palette.script` | |
| Configurable hotkeys | Missing | `crates/gk-settings/src/lib.rs:55` (`keymap` field never read) | All shortcuts are hard-coded. |
| Light / dark themes | Done | impl: `dialogs/settings.rs:225-229`, `crates/gk-app/src/theme.rs:144`; E2E: `r-settings.script`, `r-settings-reopen.script` | |
| Compact density | Partial | impl: UI scale (`settings.rs:232`), History row height (`settings.rs:329`), toolbar auto-compact under 1080 px (`toolbar.rs:9`) | No density preset. File lists and the sidebar have fixed spacing. |
| Configurable monospace font | Partial | impl: "Monospace font size" in Settings (`settings.rs:238`) | The setting has no effect: `theme::apply` (`theme.rs:144`, called from `app.rs:148`) never gets `mono_font_size`. The family is fixed to Consolas (`theme.rs:136`). |
| Long operations in status bar with progress and cancel | Done | impl: `crates/gk-app/src/views/statusbar.rs:68-75`, `crates/gk-core/src/session.rs:650`; test: `it/remote.rs::cancelled_fetch` | |
| No blocking "please wait" modals | Partial | impl: ops run in the background | Clone runs inside an `egui::Modal` (`dialogs/global.rs:284-289`), which blocks the rest of the UI until it finishes or is cancelled. Some dialogs wait with a spinner (`dialogs/common.rs:535`) but can be closed. |

## Architecture / performance

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| UI thread never calls git | Done | impl: `crates/gk-core/src/session.rs:264` (snapshot of Arcs), `session.rs:415` (Load slots), lazy sidebar loaders, `Task::spawn` in dialogs; `docs/ARCHITECTURE.md` | No automated guard. Code review found no direct calls from the UI thread. |
| Cancellation tokens and generation numbers | Partial | impl: `crates/gk-cli/src/lib.rs:60` (CancelToken), `session.rs:56,215`, Load tickets; test: `it/history.rs::cancelled_build`, `history/task.rs::newer_key_wins` | History rebuilds cannot be cancelled: `session.rs:625` passes `AtomicBool::new(false)`. |
| Priorities (visible > prefetch > background) | Missing | `crates/gk-core/src/history.rs:219` and the session loaders use the global rayon pool, first in first out | |
| Write ops serialized | Partial | impl: gk-ops thread `session.rs:650` (`run_op`, `run_index_op`); test: `it/session.rs::session_end_to_end` | Auto-fetch writes refs from its own thread (`tab.rs:156`). |
| Interning / arenas | Partial | impl: dense `u32` commit indices (`history.rs:61-82`), `InternedInput` (`crates/gk-diff/src/lib.rs:12`) | No path or string interning. Each status entry owns a `BString`. |

## Status engine

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Index via gix | Done | impl: `crates/gk-status/src/gix_engine.rs:16` (the default engine); test: `crates/gk-status/tests/engines.rs::gix_matches_cli` | |
| Parallel full scan | Done | impl: gix status (index-worktree, dirwalk and tree-index in parallel) | Measured on r1m: gix 898 ms, CLI 396 ms. |
| Incremental updates from file watcher (ReadDirectoryChangesW) | Done | impl: `crates/gk-status/src/watch.rs:74` (notify, which uses RDCW on Windows), `crates/gk-status/src/fast.rs`, `session.rs:555`; test: `watch.rs::delivers_worktree_events`, `engines.rs::fast_path_matches_cli` | Measured: 0.2 ms with a cached index. |
| Debounce | Done | impl: `watch.rs:18-19` (60 ms, at most 400 ms) | |
| .git watch | Done | impl: `watch.rs:163`, `session.rs:469`; test: `watch.rs::git_dir_classification`, `it/session.rs::session_external_ref_change` | |
| Overflow leads to full rescan | Done | impl: `watch.rs` `Raw::Overflow`, `session.rs:479` | No dedicated test. |
| Cold start from cached snapshot | Missing | (none) | `Session::open` always starts from scratch (`session.rs:256`). |
| Optional fsmonitor daemon query | Missing | (none) | Optimize turns on `core.fsmonitor` for the git CLI only. The gix engine and the fast path never query the daemon. |
| CLI engine switch as reference | Done | impl: `crates/gk-status/src/cli_engine.rs`, Settings "Status engine" (`settings.rs:255`), applied live (`app.rs:157`); test: `engines.rs::gix_matches_cli` | |

## History engine

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Rev-walk with commit-graph | Done | impl: `crates/gk-core/src/history.rs:57`; test: `it/history.rs::*` | Measured: git.git 97 ms. |
| Offer to write commit-graph | Partial | impl: the Optimize dialog shows commit-graph status and writes it (`repo_tools.rs:379`, `advanced.rs:591,615`) | Nothing prompts the user when a large repo has no commit-graph. |
| Lane layout in chunks | Partial | impl: `crates/gk-graph/src/layout.rs:46-49` (512-row checkpoint blocks, edges computed lazily); test: `layout.rs::lazy_edges_match_eager_computation`, `million_rows_is_fast_and_small` | The full walk and lane pass finish before the first paint. Results are not published in chunks. |
| On-disk layout cache | Missing | (none) | |
| Lazy metadata + LRU | Done | impl: `history.rs:180-244` (LRU of 20k); test: `it/history.rs::commit_meta_and_cache` | |
| Search filters in background | Done | impl: `session.rs:394`, `history.rs:261` (rayon chunks); test: `it/history.rs::search_message_author_id`; E2E: `h-search.script` | Measured: 75.6 ms on git.git. |
| Pickaxe via streaming `git log` | Missing | (none) | |
| Refs tree for tens of thousands of refs | Done | impl: one `for-each-ref` (`refs.rs:176`), sidebar trees with filter and lazy rows (`sidebar.rs:235,336`) | Not measured at 10k+ refs. |
| Blame streaming via `--incremental` | Partial | impl: `crates/gk-core/src/advanced.rs:250` (blocking `--porcelain` on a background thread) | Nothing shows until the whole blame is done. |

## Diff

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| imara-diff | Done | impl: `crates/gk-diff/src/lib.rs:12`; test: `gk-diff/src/lib.rs::*` | |
| Unified / side-by-side | Done | impl: `crates/gk-app/src/views/diff.rs:319` ("Split"); test: `diff/model.rs::pairing_runs`, `layout_rows_and_partners` | |
| Word diff | Done | impl: `diff.rs:326`, `crates/gk-diff/src/words.rs`; test: `words.rs::highlights_changed_word` | |
| Ignore whitespace | Done | impl: `diff.rs:333`; test: `gk-diff/src/lib.rs::whitespace_ignored` | Hunk and line staging are turned off in this mode. |
| Context lines | Done | impl: `diff.rs:340-346` (whole-file toggle and count); E2E: `r-settings.script` (6 lines) | |
| Rename detection | Partial | impl: status `--find-renames` (`cli_engine.rs:12`), gix defaults in `content.rs:204`; test: `search.rs::file_log_follows_renames` | On by default. No UI toggle or similarity threshold. |
| Large files with "show anyway" threshold | Partial | impl: hard cap `MAX_DIFF_BYTES` = 64 MB (`crates/gk-core/src/content.rs:9`) | No "show anyway" button. The `max_file_kb` setting (`gk-settings/src/lib.rs:154`) is never read. |
| Syntax highlighting of visible lines | Deviation | impl: `crates/gk-app/src/views/diff/syntax.rs` (whole diff highlighted per hunk in the background, skipped above 20k lines, `syntax.rs:20`); test: `syntax.rs::*` | |
| Binary detection | Done | impl: `gk-diff/src/lib.rs:80`, `diff.rs:191`; test: `gk-diff/src/lib.rs::binary_detection`, `it/ops.rs::stage_lines_binary_rejected` | |
| Image diff (side by side / overlay / slider) | Partial | impl: `crates/gk-app/src/views/diff/preview.rs:187-191` (side by side, swipe slider); test: `preview.rs::*` | No overlay or onion-skin mode. |
| LFS pointers with size | Missing | (none) | Pointer files show as plain text. |
| Staging file / hunk / lines | Done | See Working copy. | |
| Discard with confirmation | Done | impl: `diff.rs:1073`, `dialogs/worktree.rs:69`, Settings confirmations (`settings.rs:356`); E2E: `r-discard-file.script` | |
| Bulk ops via pathspec-from-file | Done | impl: `crates/gk-core/src/ops.rs:16-90`, `crates/gk-cli/src/lib.rs:398`; test: `it/ops.rs::stage_treats_paths_literally` | |
| Built-in 3-way conflict view | Done | See Conflicts. | |

## Write operations

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| git CLI ≥ 2.45 check at startup | Partial | impl: `crates/gk-cli/src/lib.rs:19` (`MIN_VERSION`) | Checked only in the About dialog (`dialogs/global.rs:518`). Nothing warns at startup. |
| Optional bundled MinGit | Missing | (none) | Only a custom git path in Settings (`settings.rs:261`). |
| Process runner: streaming, progress, cancel via Job Object | Done | impl: `gk-cli/src/lib.rs:205`, `crates/gk-cli/src/job.rs:14`; test: `lib.rs::streaming_reports_lines`, `line_splitting_handles_cr_progress`, `it/remote.rs::cancelled_fetch` | |
| Long-lived `cat-file --batch` | Deviation | impl: objects are read in-process through gix (`content.rs:72`) | Reasonable substitute, but not recorded as a deviation in the docs. |
| GIT_ASKPASS / SSH_ASKPASS / GIT_EDITOR / GIT_SEQUENCE_EDITOR | Done | impl: `gk-askpass/src/server.rs:92-98`, `ops.rs:367,419-420,439`; test: `askpass.rs::*`, `it/rebase.rs::interactive_*` | |
| Confirmations before reset --hard / discard / force-push | Done | impl: `crates/gk-settings/src/lib.rs:249`, `settings.rs:355-359`, `remote.rs:373-387`, Reset dialog `rewrite.rs:717`; E2E: `h-reset-hard.script`, `r-force-push.script`, `r-discard-file.script` | |
| Discarded changes saved to a hidden stash object (`git stash create`) | Missing | `crates/gk-core/src/ops.rs:74` (checkout plus delete, no backup) | |
| Output panel with all commands | Done | See Tools (log of commands). | |

## Stack

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Rust | Done | `Cargo.toml` (edition 2024) | |
| UI: egui + DX11 instead of GPUI | Deviation | `crates/gk-dx11`, `docs/ARCHITECTURE.md` (Rendering) | Documented. |
| gix | Done | `Cargo.toml` (gix 0.89) | |
| git CLI | Done | `crates/gk-cli` | |
| imara-diff | Done | `crates/gk-diff/Cargo.toml` | |
| Syntax highlighting | Done | syntect (`crates/gk-app/Cargo.toml`) | |
| Fuzzy search (nucleo) | Deviation | `crates/gk-app/src/views/palette.rs:139` (own subsequence scorer) | nucleo is not a dependency. |
| Watcher | Done | notify 8 (`crates/gk-status/Cargo.toml`) | |
| rayon | Done | workspace dependency | |
| Disk cache (redb) | Missing | (none) | |
| TOML settings | Done | `crates/gk-settings/src/io.rs:27`; test: `gk-settings/src/tests.rs::*` | |
| tracing + Tracy profiling | Missing | `tracing` is declared in workspace dependencies but no crate uses it | No Tracy. |
| cargo-dist / MSI / winget | Deviation | Inno Setup installer and zip (`packaging/gitkwaken.iss`, `packaging/build-release.ps1`, `.github/workflows/release.yml`), winget manifests in `packaging/winget/` | No cargo-dist and no MSI. |
| Auto-update | Done | impl: `crates/gk-update/src/lib.rs:109`, `crates/gk-app/src/update.rs`, updater button in the tab bar (`app.rs:216`); test: `gk-update/src/tests.rs::*`; E2E: `scripts/run-e2e-update.sh` | |

## Tests

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Differential status tests vs git | Done | `crates/gk-status/tests/engines.rs::gix_matches_cli`, `fast_path_matches_cli` | |
| Integration tests of operations | Done | `crates/gk-core/tests/it/{ops,rebase,remote,refs,content,history,session,repo}.rs` (about 90 tests) | `advanced.rs` (bisect, flow, LFS, sparse, worktree, submodule, optimize, blame run) has no integration tests. |
| UI tests | Done | `history/ui_tests.rs`, `file_status.rs::renders_headless`, `diff.rs::hunk_actions_on_a_real_repository`; E2E: `scripts/run-e2e.sh` (7), `run-e2e-history.sh` (19), `run-e2e-remote.sh` (25), `run-e2e-update.sh` | E2E suites do not run in CI (`.github/workflows/ci.yml`). |
| Benchmark corpus (linux kernel, chromium, synthetic 1M, LFS repo) | Partial | `bench-data/` (r200k, r1m from `gk-bench gen`, gitgit, demo) | No linux kernel, chromium or LFS repo. |
| Benchmarks in CI failing on >10% regression | Missing | `.github/workflows/ci.yml` (fmt, clippy, test only) | |
| Comparison with SourceTree / Fork / GitKraken / SourceGit / git CLI before releases | Missing | (none) | Only the gk-bench comparison against git status. |

## Performance budgets

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| First frame < 300 ms from cache | Partial | (no snapshot cache) | Not measured. No cache exists to start from. |
| History first screen < 100 ms | Partial | gk-bench: refs 40 ms + build 97 ms on git.git (given: 95-155 ms) | About 137 ms before the first rows: over budget. |
| Full status warm < 1.5 s at 1M files | Done | gk-bench on r1m: gix 898 ms, CLI 396 ms (given: 415 ms) | |
| Status after save < 50 ms | Partial | fast path 0.2 ms (given: 0.2 ms) | Watcher debounce is 60 ms (`watch.rs:18`), so end-to-end is at least 60 ms. End-to-end is not measured. |
| 1M-row scroll at 120 fps, no frame > 16 ms | Partial | 117 fps (vsync) and > 900 fps unlocked on a 1M-row list; 120 fps scrolling git.git history (given) | Not measured on 1M-commit history. Frame-time spikes were not recorded. |
| Stage 100k files < 2 s | Partial | `ops.rs:16` (one `git add --pathspec-from-file`) | Not measured. |
| Filter 1M paths < 50 ms per keystroke | Partial | `file_status.rs` sync and filter | Not measured. |
| Open a 50k-line diff < 100 ms | Partial | | Not measured. Highlighting is skipped above 20k lines. |
| File history < 1 s for the first 50 | Partial | `views/search.rs:220` | Not measured. It waits for the whole `git log --follow` before showing anything. |
| Memory < 60 MB small repo / < 400 MB reference | Partial | Peak working set: demo 107 MB, git.git 133 MB, r1m 368 MB | Small repo is over budget (includes D3D11 and driver memory). r1m is under budget. |

## Risks

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Defender / Dev Drive hint and diagnostics measurement | Missing | (none) | |
| Case / Unicode / long-path config respected | Partial | `packaging/gitkwaken.manifest:37` (longPathAware), literal pathspecs (`ops.rs` `literal_paths`); `core.ignorecase` and `precomposeunicode` are left to gix and git | No tests for case-only renames, NFD names or paths over 260 characters. |
| Watcher paused during own ops | Partial | `crates/gk-status/src/watch.rs:53` (`pause` / `resume`) | Never called: `Session` keeps the watcher only as `_watcher` (`session.rs:170`). Own ops trigger watcher refreshes on top of the post-op refresh. |

## Gaps

One line per Missing or Partial item: what to add, and where.

**Repositories and working copy**

- Bookmark groups: render grouped sections and add a bookmarks list to the sidebar or a menu while tabs are open (`views/welcome.rs`, `views/sidebar.rs`).
- Sparse clone: add a "Sparse (cone) checkout of…" field that runs `git clone --sparse` then `sparse-checkout set` (`dialogs/global.rs` CloneRepo).
- Scan disk for repositories: add a "Scan folder…" global dialog that walks directories for `.git` in the background and offers to bookmark (new `dialogs/global.rs::ScanRepos`, reached from Welcome and the palette).
- Ignore by mask: add a free-text pattern field to the Ignore submenu, with a preview of matched files (`file_status/list.rs:836`).
- Filters by status: add M/A/D/?/U toggle chips beside the path filter and apply them in `StatusState::sync` (`views/file_status.rs:114,287`).

**Commit**

- Commit templates: prefill an empty message box from `commit.template` once per session (`views/file_status/commit.rs` via `load_async`).
- Author override: add an "Author…" field to the commit options that fills `CommitOptions.author` (`commit.rs:317`).
- Hook output: stream commit output with `run_streaming` and show successful op output as an info message (`ops.rs:191`, `session.rs:688`). Add a "Skip hooks" checkbox for `no_verify`.

**Stash**

- Stash diff: when a stash row is selected, show `stash^1..stash` in the details or diff pane (`sidebar.rs:1099`, reusing `views/details.rs` compare with stash ids).

**History**

- Branch filter: offer a multi-select branch list that uses `Scope::Revs` (`views/history.rs:247`).
- Author, date and path filters: add a filter row to the Search view and back it with `git log --author/--since/--until -- <path>` streamed in the background (`views/search.rs`, new `gk-core` history filter).
- Pickaxe: add `-S` and `-G` modes to that filter row, streaming `git log --format=%H -S…` into `Load<Vec<u32>>` (`gk-core/src/session.rs::search`).
- Reflog: add a "Reflog" sidebar section or view from `git reflog --format`, with checkout and reset from an entry (`views/sidebar.rs`, `gk-core/src/refs.rs`).

**Rewriting**

- Undo: add an "Undo last operation" toolbar or palette action that resets to `HEAD@{1}` / `ORIG_HEAD` with confirmation (`dialogs/rewrite.rs`, `ops.rs::reset`).

**Remotes and authentication**

- Auto-fetch: route it through `session.run_op` or a low-priority op queue instead of `std::thread::spawn` (`tab.rs:156`).
- Git Credential Manager: detect `credential.helper` and the GCM install, and show status in Settings or About (`dialogs/settings.rs`, `dialogs/global.rs` About).
- SSH agent: detect the agent (Windows `ssh-agent` service or `SSH_AUTH_SOCK`), list keys and offer `core.sshCommand` in Settings (`dialogs/settings.rs`).

**Advanced and tools**

- Notes: show `git notes show` in commit details, and add "Add / edit note…" to the commit menu (`views/details.rs`, `views/history.rs:511`, new `ops.rs::notes_*`).
- External diff tool: add "Open in external diff tool" to the file and diff context menus, using `settings.external_diff_tool` (`file_status/list.rs`, `views/details.rs`).
- Config editors: add Settings pages that edit the repo and global `.gitconfig` and `.gitignore` as text with save (`dialogs/settings.rs` or `dialogs/repo_tools.rs`).

**UI/UX**

- Branch drag-and-drop: make sidebar branch rows drag sources and drop targets, and on drop show a "Merge X into Y / Rebase Y onto X" popup (`views/sidebar.rs` `row_ui`, reuse `Dialog::merge` / `Dialog::rebase`).
- Keyboard access: add palette entries for commit-menu actions on the selected commit and for stash, worktree and submodule actions (`views/palette.rs` `items`).
- Configurable hotkeys: map action ids to shortcuts from `settings.keymap` in `app.rs` and `tab.rs` `shortcuts()`, and add a Settings page to edit them.
- Compact density: add a Density setting (comfortable / compact) that scales `spacing.item_spacing` and the row heights of lists and the sidebar (`theme.rs::apply`).
- Monospace font: pass `mono_font_size` and a font family into `theme::apply`, and load the font from the system or a path (`app.rs:148`, `theme.rs:136-144`).
- Clone modal: run clone as a background job with status-bar progress that opens the tab when done, and close the dialog right away (`dialogs/global.rs` CloneRepo).

**Architecture**

- History cancellation: pass a session-owned interrupt flag that a newer refresh sets into `history::build` (`session.rs:625`).
- Priorities: add a small priority scheduler (visible > prefetch > background) in front of rayon for diffs, metas and search (`gk-core/src/session.rs` `load`, `history.rs:219`).
- Interning: intern status paths (arena or `Arc<str>` table) and share them between status, lists and the tree (`gk-status/src/lib.rs` Entry, `views/file_status/tree.rs`).

**Status engine**

- Snapshot cache: persist the last status, refs and history ids per repo (redb), and publish them on open before the first refresh (`gk-core/src/session.rs::open`, new cache module).
- fsmonitor: when `core.fsmonitor` is set, query the daemon (`git fsmonitor--daemon` IPC, or `git status` with fsmonitor) for changed paths before a full scan (`gk-status/src/fast.rs` / `lib.rs::compute`).
- Watcher pause: keep a handle and call `Watcher::pause()` / `resume()` around `op_loop` jobs (`session.rs:650-700`).

**History engine**

- Commit-graph offer: when opening a repo with more than ~10k commits and no commit-graph, show a one-click "Write commit-graph" message (`session.rs` refresh_loop, `statusbar.rs`).
- Chunked layout: publish the first N laid-out rows while the walk continues, extending `History` incrementally (`gk-core/src/history.rs::build`, `session.rs:625`).
- Layout cache: store `ids` and layout checkpoints keyed by tip set in redb, and validate them on open (`gk-graph/src/layout.rs`, `gk-core/src/history.rs`).
- Blame streaming: use `git blame --incremental` through `run_streaming` and fill lines as records arrive (`gk-core/src/advanced.rs:250`, `views/blame.rs`).

**Diff**

- Rename detection: add a diff option for rename detection on/off and a similarity threshold, passed to gix rewrites and `--find-renames=<n>` (`content.rs:204`, `cli_engine.rs:12`, `views/diff.rs` toolbar).
- Large files: honor `settings.diff.max_file_kb` as the soft limit and show a "Show anyway" button that retries with a raised limit (`gk-core/src/content.rs:9,76,130`, `views/diff.rs`).
- Image overlay: add an "Overlay / onion skin" mode with an opacity slider (`views/diff/preview.rs:187`).
- LFS pointers: detect `version https://git-lfs.github.com/spec/v1`, show oid and size, and offer to fetch the object (`views/diff.rs` binary_info, `gk-diff`).

**Write operations**

- git version check: call `Git::version()` on a background thread at startup and show a banner when it is below `MIN_VERSION` or missing (`app.rs::GkApp::new`).
- MinGit: optionally ship MinGit under `<install>/git/` and prefer it in `find_git` when the system git is missing or too old (`gk-cli/src/lib.rs:355`, `packaging/build-release.ps1`, `packaging/gitkwaken.iss`).
- Discard backup: before discarding, run `git stash create` (plus copies of untracked files) and record the commit (for example under `refs/gitkwaken/discarded`), with a "Restore discarded changes" action (`gk-core/src/ops.rs:74`, `dialogs/worktree.rs`).

**Stack**

- redb: add a `gk-cache` crate (redb) for the status snapshot and history layout caches.
- tracing + Tracy: add `tracing` spans to session loops, status and history, plus a `tracing-tracy` subscriber behind a feature or env var (`gk-core`, `gk-status`, `gk-app/src/main.rs`).

**Tests and performance**

- Benchmark corpus: add scripts to fetch the linux kernel and chromium (shallow mirrors) and generate an LFS repo for gk-bench (`scripts/`, `crates/gk-bench`).
- CI benchmarks: add a benchmark job that runs gk-bench on a cached corpus and fails on >10% regression against a stored baseline (`.github/workflows/`).
- Client comparison: add a release checklist and script recording timings against SourceTree, Fork, GitKraken, SourceGit and git CLI (`docs/`, `scripts/`).
- First frame: add a `gk-bench startup` measurement after the snapshot cache exists.
- History first screen: walk the first screen (about 200 commits) and publish before the full layout. Load refs in parallel with the walk (`session.rs` refresh_loop).
- Status after save: lower the watcher debounce for small bursts (for example a 10-20 ms first flush) and add an end-to-end save-to-snapshot benchmark (`gk-status/src/watch.rs:18`).
- 1M-row scroll: add a frame-time histogram (`GK_DX11_*` stats) and a scripted scroll over a 1M-commit synthetic history (`crates/gk-dx11`, `scripts/e2e/`).
- Stage 100k, filter 1M, 50k-line diff: add gk-bench cases for `Repo::stage` with 100k paths, `StatusState` filter rebuild on 1M entries, and `gk_diff::diff` plus layout on a 50k-line file.
- File history: stream `git log --follow` results and show the first 50 as they arrive (`views/search.rs:220`).
- Memory: profile the idle working set on small repos (fonts, syntect assets loaded early, D3D buffers) and load syntect lazily (`views/diff/syntax.rs`, `theme.rs::install_fonts`).

**Risks**

- Defender / Dev Drive: time a sample status, check Defender exclusions and Dev Drive (ReFS) for the repo path, and show a hint in Optimize (`dialogs/repo_tools.rs` Optimize, `gk-core/src/advanced.rs`).
- Case / Unicode / long paths: add integration tests for case-only renames with `core.ignorecase`, NFC/NFD names and paths over 260 characters, and set `core.longpaths` in Optimize (`crates/gk-core/tests/it/ops.rs`, `advanced.rs::optimize`).
