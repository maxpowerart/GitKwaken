# GitKwaken: design-document coverage

Audit date: 2026-10-17. It was re-verified against the code after commits 6d6c2e8, 46bd036 and 01fb802, plus the uncommitted File History first-page change in `views/search.rs`. Source: workspace at `D:\Projects\GitKwaken`. Code was read only and nothing was changed.

Status legend:

- **Done**: implemented and reachable from the UI (the entry point is named in Evidence).
- **Partial**: implemented in part, not reachable from the UI, or a budget that was not measured or not met.
- **Missing**: not implemented.
- **Deviation**: implemented differently from the design.

Evidence uses `impl:` for the code, `UI:` for the entry point, `test:` for unit/integration tests (`crate/path.rs::fn`), `E2E:` for UI scenarios (`scripts/e2e/*.script`, run by `scripts/run-e2e*.sh`; `fa-*` by `run-e2e-features-a.sh`, `fb-*` by `run-e2e-features-b.sh`; all six suites run in CI through `scripts/ci-e2e.sh` / `.github/workflows/e2e.yml`). Paths are relative to the repo root. `it/` = `crates/gk-core/tests/it/`.

Summary: 167 requirements. **144 Done, 18 Partial, 1 Missing, 4 Deviation.** The full list is under Gaps.

Measurements (Windows 11, release builds, 2026-10-17; details and commands are in `docs/BENCHMARKS.md`):

- Status, r1m (1M files): full refresh 415 ms (CLI engine), gix engine 898 ms, `git status` 407 ms. Watcher-triggered fast path 0.2 ms. Save to status, end to end: 22 ms median (adaptive 15 ms debounce, `gk-bench save-latency`).
- History, git.git (85,618 commits): first rows 19.5 ms without cache and 29 ms from the warm redb snapshot cache (`gk-bench history-first`, `gk-bench open`). Full build 97 ms, search "fix" 75.6 ms.
- History, Linux kernel (1,485,415 commits): build 2.48 s, layout 36 MB, search 492 ms, peak working set 1.17 GB (whole bench process).
- Scrolling: 120 fps on history; 117 fps (vsync) and > 900 fps unlocked on a 1M-row list.
- Stage 100k files: 8-11 s, bound by object writes on this disk (bulk path `update-index --stdin`; was 152 s).
- Filter 1M paths (nucleo): 81 ms single-threaded from `&str`, 21-31 ms with rayon. 50k-line diff computation: 19.8 ms. File history, first 50 commits: 48-244 ms.
- Idle memory: demo repo about 76 MB (about 42 MB of it is shared driver/DLL pages, syntect about 8 MB); r1m 368 MB.
- Against SourceGit (`scripts/compare-clients.ps1`): window shown in 225-256 ms vs 367-490 ms; 0% idle CPU vs about 55%.

## Functionality

### Repositories

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Bookmarks with groups | Done | impl: `crates/gk-app/src/dialogs/global.rs` (Bookmark dialog with group combo), `crates/gk-settings/src/lib.rs`, `crates/gk-app/src/views/welcome.rs` (grouped Welcome list; `repositories_menu`: bookmarks by group, "Bookmark this repository…" / "Remove bookmark" for the open tab); UI: Welcome page (right-click a Recent repo, "Bookmark"), tab bar "Repositories ▾"; test: `crates/gk-settings/src/tests.rs::bookmark_helpers`, `welcome.rs::current_repository_bookmark_entry`; E2E: `fa-bookmark-group.script`, `fb-bookmark-current.script`, `fb-bookmark-remove.script` | No bookmarks section in the sidebar. |
| Tabs | Done | impl: `crates/gk-app/src/app.rs:250` (tab bar: select, middle-click, ×, context menu), `app.rs:380-412` (keymap Open / Close / Next / Prev tab, defaults in `keymap.rs:35-37`), `app.rs:145,244` (open tabs restored and persisted); UI: tab strip, "+" | No test of tab handling. |
| Init | Done | impl: `crates/gk-app/src/dialogs/global.rs:424` (InitRepo), `crates/gk-core/src/repo.rs:103`; UI: Welcome "Create new…" (`welcome.rs:67`), palette "Create repository…" (`palette.rs:93`); test: `it/repo.rs::init_new_repo` | |
| Clone (shallow, partial `--filter=blob:none`, sparse) | Done | impl: `crates/gk-app/src/dialogs/global.rs:161` (CloneRepo, Advanced section `global.rs:327-350`), `global.rs:200` (`--depth`), `global.rs:203` (`--filter=blob:none`), `global.rs:205,256` (`--sparse` then `sparse-checkout set`), `global.rs:65` (CloneJob runs in the background and opens the tab when done); UI: tab bar "Clone…" (`app.rs:308`), Welcome, palette; test: `global.rs::sparse_dirs_are_validated`; E2E: `r-clone.script`, `fb-clone-sparse.script` | Shallow and partial clones have no test or E2E. |
| Open folder | Done | impl: `crates/gk-app/src/app.rs:163` (`open_repo`), `app.rs:190-193` (folder picker); UI: Ctrl+O (keymap), tab bar "+", Welcome "Open repository…" (`welcome.rs:59`), "Repositories ▾", palette (`palette.rs:91`); test: `it/repo.rs::open_from_subdir_and_failures` | |
| Scan disk for repositories | Done | impl: `crates/gk-app/src/dialogs/scan.rs:36` (`scan`: skips `node_modules`/`target`, does not descend into repositories), `scan.rs:88-112` (background thread, live list), `scan.rs:262,275` (Bookmark all, Open); UI: Welcome "Scan folder…" (`welcome.rs:73`), "Repositories ▾" (`welcome.rs:399`), palette (`palette.rs:95`); test: `scan.rs::finds_repositories_without_descending`; E2E: `fa-scan.script` | |

### Working copy

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Staged / unstaged lists | Done | impl: `crates/gk-app/src/views/file_status.rs:306` (`lists`); UI: File Status view; test: `file_status.rs::renders_headless`; E2E: `commit.script`, `drag.script` | |
| Tree and flat view | Done | impl: `crates/gk-app/src/views/file_status.rs:317` ("Tree" toggle), `file_status/tree.rs`; test: `tree.rs::builds_nested_tree_dirs_first`, `collapsed_dirs_hide_content`, `compacted_chain_uses_deepest_path`, `fuzzy_filter`, `file_status.rs::renders_headless` | |
| Stage / unstage / discard file | Done | impl: `crates/gk-core/src/ops.rs:16,82,126` (stage, unstage, discard; above 1,000 paths through `update-index --stdin` / `--index-info`), `ops.rs:77,116` (`stage_all`, `unstage_all`); UI: row checkbox, context menu (`file_status/list.rs:817-833,862`), toolbar Discard (`views/toolbar.rs:98`), Ctrl+Shift+A / Ctrl+Shift+U (keymap); test: `it/ops.rs::stage_unstage_with_head`, `stage_unstage_unborn`, `discard_tracked_and_untracked`, `bulk_stage_and_unstage_match_small_path`, `stage_treats_paths_literally`; E2E: `commit.script`, `r-discard-file.script`, `r-discard-untracked.script`, `r-discard-all.script`, `fb-stage-all.script`, `fb-unstage-all.script` | |
| Stage / unstage / discard hunk | Done | impl: `crates/gk-app/src/views/diff.rs:986-992` (hunk actions), `diff.rs:1079-1086`; UI: hunk header buttons, diff context menu; test: `diff.rs::hunk_actions_on_a_real_repository`; E2E: `fb-large.script` (Stage hunk after "Show anyway") | |
| Stage / unstage / discard line | Done | impl: `crates/gk-app/src/views/diff.rs:982-990,1089-1093,1178`, `crates/gk-core/src/ops.rs:151,223`; UI: line selection, context menu "Stage lines (n)" / "Unstage lines (n)" / "Discard lines (n)…"; test: `it/ops.rs::stage_lines_subset`, `discard_lines_in_worktree`, `stage_lines_crlf_autocrlf`, `stage_lines_untracked_file`, `stage_lines_deleted_file`, `stage_lines_conflicted_rejected`, `stage_lines_binary_rejected`; E2E: `stage-line.script` | |
| Ignore by mask | Done | impl: `crates/gk-app/src/views/file_status/list.rs:874-906` (Ignore submenu: file, `*.ext`, `dir/`, n selected, "Custom pattern…"), `crates/gk-app/src/views/file_status/ignore.rs:66` (pattern editor with validation and a live preview from `git ls-files -o -i --exclude`), `crates/gk-core/src/ops.rs:756`; test: `ignore.rs::pattern_validation`, `it/ops.rs::ignore_appends`; E2E: `fb-ignore-custom.script` | |
| Filters by status | Done | impl: `crates/gk-app/src/views/file_status.rs:319,488` (M A D R ? U chips, combinable, with counts), `file_status/list.rs:69` (`KIND_FILTERS`), `file_status.rs:326` (path text filter); test: `list.rs::status_chips_combine_with_text_filter`, `filter_and_rebuild_key`; E2E: `fb-chips.script` | |

### Commit

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Amend | Done | impl: `crates/gk-app/src/views/file_status/commit.rs:532` (Amend checkbox, loads the HEAD message), `commit.rs:143`; test: `it/ops.rs::commit_amend_signoff`; E2E: `r-amend.script` | |
| Sign-off | Done | impl: `commit.rs:547`, `commit.rs:144`, default in Settings (`crates/gk-app/src/dialogs/settings.rs:447`); test: `it/ops.rs::commit_amend_signoff` | |
| GPG/SSH signing | Done | impl: `commit.rs:658` (per-commit "Sign commit (GPG / SSH)" in Options ▾, follows `commit.gpgsign`), `commit.rs:152-154` (`-S` / `--no-gpg-sign`), Settings Always / Never / Follow git config (`settings.rs:450-452`) | SSH signing goes through `gpg.format`. No test, because the test repos set `commit.gpgsign=false`. |
| Commit templates (`commit.template`) | Done | impl: `commit.rs:44` (reads `config --path commit.template`), `commit.rs:214` (`offer_template` prefills an empty box), `commit.rs:225` (an unedited template counts as empty); test: `commit.rs::template_stripping_and_hook_output`; E2E: `fb-commit-options.script` | |
| History of messages | Done | impl: `commit.rs:423` ("Recent ▾"), `commit.rs:254` (`push_recent_message`), `crates/gk-settings/src/repo_state.rs`; test: `crates/gk-settings/src/tests.rs::recent_messages_cap_dedup` | |
| Author per commit (override) | Done | impl: `commit.rs:670` (Options ▾ "Override author…"), `commit.rs:495-512` (field with validation), `commit.rs:80` (`parse_author`), `commit.rs:157` (`--author`); test: `commit.rs::author_validation`, `commit_runs_hooks_and_reports_output`; E2E: `fb-commit-options.script` | |
| Hooks with visible output | Done | impl: `commit.rs:122` (`run_commit` streams through `run_streaming` into the status-bar progress), `commit.rs:114` (`extra_output`), `commit.rs:618-623` (successful hook output shown as an Info message), `commit.rs:668` ("Skip hooks (--no-verify)"); test: `commit.rs::commit_runs_hooks_and_reports_output`, `template_stripping_and_hook_output`; E2E: `fb-commit-options.script` (pre-commit hook output) | |
| Commit and push | Done | impl: `commit.rs:304` (`push_target`), `commit.rs:551-558,624-631`; test: `commit.rs::push_target_uses_upstream_or_first_remote`; E2E: `r-commit-push.script` | |

### Stash

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Push (incl. partial, keep-index) | Done | impl: `crates/gk-app/src/dialogs/worktree.rs:217` (Stash dialog: keep staged, include untracked, staged only), `crates/gk-core/src/ops.rs:579`; UI: toolbar Stash (`views/toolbar.rs:95`), STASHES header, file context menu "Stash…" / "Stash folder…" (`file_status/list.rs:871,966`); test: `it/ops.rs::stash_keep_index_and_restore_index`, `stash_untracked_and_paths`; E2E: `r-stash.script` | |
| Apply / pop / drop | Done | impl: `worktree.rs:367,428` (StashApply, StashDrop), `ops.rs:602,613`; UI: sidebar stash context menu (`views/sidebar.rs:1180-1188`), stash view Apply… / Pop / Drop… (`views/stash_view.rs:72-92`); test: `it/ops.rs::stash_push_apply_pop_drop`; E2E: `r-stash-apply.script`, `r-stash-pop.script`, `r-stash-drop.script`, `fa-stash.script` | |
| View stash diff | Done | impl: `crates/gk-app/src/views/stash_view.rs:55` (the stash against its first parent, plus untracked files from `stash^3`; file list and diff through `details::files_view`, `stash_view.rs:188`); UI: click a stash in the sidebar (`sidebar.rs:1177`, `Cmd::ShowStash`); test: `stash_view.rs::ls_tree_blobs`; E2E: `fa-stash.script` | |
| Branch from stash | Done | impl: `worktree.rs:479` (StashBranch), `ops.rs:619`; UI: sidebar "Branch from stash…" (`sidebar.rs:1190`), stash view "Branch…" (`stash_view.rs:99`); test: `it/ops.rs::stash_apply_conflict_and_branch` | No E2E. |

### History

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Graph | Done | impl: `crates/gk-core/src/history.rs:74` (`build`, cancellable), `crates/gk-graph/src/layout.rs`, `views/history/graph.rs`; test: `history/ui_tests.rs::history_view_selection_keyboard_and_worktree_row`, `layout.rs::lazy_edges_match_eager_computation`, `million_rows_is_fast_and_small`, `graph.rs::edge_geometry`, `gk-core/src/session/tests.rs::partial_history_then_full`; E2E: all `h-*.script` | |
| Filter: branches | Done | impl: `crates/gk-app/src/views/history.rs:322` (branch picker: All, Current, or a checkbox per local and remote branch, which gives `Scope::Revs`), `history.rs:305` (`set_scope`, persisted), `gk-core/src/history.rs:201`; UI: History toolbar "All branches ▾"; test: `it/history.rs::merges_and_multiple_tips`, `tag_tips_are_walked`, `it/session.rs::session_without_watcher_and_scope`; E2E: `fa-branch-filter.script` | |
| Filter: author | Done | impl: `crates/gk-app/src/views/history/filter.rs:82` (`--author`), Author field in the filter row (`history.rs:455`); UI: History toolbar "Filters"; test: `filter.rs::builds_git_log_args`; E2E: `fa-filter-text.script` | Matches show as a flat list without the graph (`history.rs:513`). |
| Filter: date | Done | impl: `filter.rs:85-89` (`--since` / `--until`), Since and Until fields (`history.rs:470-471`); test: `filter.rs::builds_git_log_args`; E2E: `fa-filter-path.script` (Since) | Dates are typed as free text; there is no date picker. Until has no E2E. |
| Filter: path | Done | impl: `filter.rs:98-100` (pathspec after `--`, file or folder), Path field (`history.rs:472`); test: `filter.rs::builds_git_log_args`; E2E: `fa-filter-path.script` | Flat list, no graph. |
| Filter: text | Done | impl: toolbar "Filter by message…" (`history.rs:438`, `filter.rs:78` `--grep`), Search view (`views/search.rs`), `gk-core/src/history.rs` search; UI: History toolbar, Search view (Ctrl+3); test: `it/history.rs::search_message_author_id`, `filter.rs::builds_git_log_args`; E2E: `h-search.script`, `fa-filter-text.script` | |
| Filter: pickaxe `-S` / `-G` | Done | impl: `filter.rs:91-95`, `-S`/`-G` combo and content field (`history.rs:474-497`), streamed through `run_streaming` on its own thread with cancel (`filter.rs:235`); test: `filter.rs::builds_git_log_args`; E2E: `fa-pickaxe.script` (`-G`) | Only argument building is tested. `-S` has no E2E. |
| Compare two commits | Done | impl: `crates/gk-app/src/views/details.rs:85,326`; UI: select two commits in History; test: `details.rs::filtering_and_picking`; E2E: `h-keyboard.script` | |
| File history | Done | impl: `views/search.rs:18-19,222-243` (a first page from `git log --follow -n 50` runs alongside the full walk and is shown while the rest loads), `search.rs:229` (`git log --follow`); UI: file context menu "File history" (`file_status/list.rs:935`); test: `search.rs::file_log_follows_renames`, `file_log_without_status_keeps_path`; E2E: `h-file-history.script` | |
| Blame | Done | impl: `crates/gk-app/src/views/blame.rs:56`, `crates/gk-core/src/advanced.rs:251`; UI: file context menu "Blame" (`list.rs:938`), palette "Blame <path>" (`palette.rs:188`); test: `blame.rs::runs_and_heat`, `advanced.rs::blame_porcelain`, `it/advanced.rs::blame_matches_git_blame`, `blame_on_larger_generated_file`; E2E: `r-blame.script` | |
| Reflog | Done | impl: `crates/gk-app/src/views/reflog.rs:111` (`git reflog show -n5000 HEAD`), toolbar Checkout / Create branch / Reset (`reflog.rs:159-178`), entry menu (`reflog.rs:362`: checkout, branch, tag, reset, copy SHA, show in history); UI: sidebar view list (`sidebar.rs:400,867`), palette "View: Reflog" (`palette.rs:105`); test: `reflog.rs::parses_reflog_lines`; E2E: `fa-reflog.script`, `fa-reflog-checkout.script` | Shows the HEAD reflog only; there is no per-branch reflog. |

### Branches and tags

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Create / rename / delete | Done | impl: `crates/gk-app/src/dialogs/branch.rs:17,245,324`, `crates/gk-core/src/ops.rs:308-334`; UI: toolbar Branch, sidebar branch menu (`sidebar.rs:1415`, Rename 1446, Delete 1449, Delete key), History "Create branch here…" (`history.rs:930`, Ctrl+B); test: `it/ops.rs::branches`; E2E: `branch.script`, `h-create-branch.script`, `h-sidebar-rename.script`, `h-sidebar-delete.script`, `fa-keys-history.script`, `fa-keys-sidebar.script` | |
| Checkout (incl. detached) | Done | impl: `branch.rs:388` (Checkout), `ops.rs:293,301`; UI: double-click in History or sidebar, Enter on a sidebar row, menus, palette "Checkout <b>" (`palette.rs:167`), reflog, drag a branch onto another (`sidebar.rs:1607`); test: `it/refs.rs::branch_and_detached_head`; E2E: `h-checkout-detached.script`, `h-sidebar-checkout.script`, `h-select-checkout.script`, `fa-keys-sidebar.script`, `fa-reflog-checkout.script`, `fa-drag-checkout.script` | |
| Tracking | Done | impl: `ops.rs:301,335`, `branch.rs:526` (SetUpstream); UI: "Track remote branch…" (`sidebar.rs:1439`), "Checkout as tracking branch…" (`sidebar.rs:1072`), Push dialog "Track" column; test: `it/remote.rs::checkout_remote_and_set_upstream`; E2E: `r-push-branch.script` | |
| Ahead / behind | Done | impl: `crates/gk-core/src/refs.rs:152-160`, badges at `sidebar.rs:973-977`, tooltip at `sidebar.rs:1000`; test: `it/refs.rs::upstream_ahead_behind_and_remote_branches`; E2E: `r-fetch.script` ("↓ 1") | |
| Tags: lightweight / annotated / signed | Done | impl: `crates/gk-app/src/dialogs/tag.rs:10,81` ("Sign tag (GPG/SSH)"), `ops.rs:362`; UI: toolbar Tag, History "Create tag here…" (`history.rs:933`), sidebar tag menu; test: `it/ops.rs::tags`, `it/remote.rs::push_tags_and_delete`; E2E: `h-create-tag.script`, `r-tag-push.script`, `fa-keys-history.script` | Signed tags are not tested: every test calls `create_tag(..., false)`. |
| git-flow | Done | impl: `crates/gk-app/src/dialogs/flow.rs:45`, `crates/gk-core/src/advanced.rs:481-536`; UI: toolbar "Git Flow", palette "Git Flow…" (`palette.rs:156`); test: `it/advanced.rs::flow_feature_and_release_cycle`, `flow_finish_stops_on_conflict`, `flow_init_without_master_fails`, `advanced.rs::flow_classify`, `flow.rs::master_choice`, `bases` | No E2E. |

### History rewriting

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Merge (ff / no-ff / squash) | Done | impl: `crates/gk-app/src/dialogs/rewrite.rs:47`, `ops.rs:391`; UI: toolbar Merge, History "Merge into current…" (`history.rs:940`), sidebar (`sidebar.rs:1425`), drag a branch onto the current one (`sidebar.rs:1607`), palette "Merge…"; test: `it/ops.rs::merge_opts`, `merge_fast_forward_and_no_ff`, `merge_squash_and_no_commit`; E2E: `h-merge.script`, `fa-drag-merge.script` | |
| Rebase onto | Done | impl: `rewrite.rs:184`, `ops.rs:415`; UI: History "Rebase current onto…" (`history.rs:943`), sidebar (`sidebar.rs:1429`), drag-and-drop popup, palette "Rebase…"; test: `it/rebase.rs::rebase_clean`; E2E: `h-rebase.script` | |
| Interactive rebase by drag-and-drop | Done | impl: `rewrite.rs:276,308` (drag ☰, ↑/↓), `ops.rs:425`; UI: History "Interactive rebase children of this commit…" (`history.rs:946`); test: `it/rebase.rs::interactive_reorder_and_plain_reword`, `interactive_reword_squash_fixup_drop`, `interactive_edit_stops`, `interactive_conflict_then_abort`; E2E: `h-interactive-rebase.script` | |
| Cherry-pick | Done | impl: `rewrite.rs:618` (Picks), `ops.rs:523`; UI: `history.rs:957`; test: `it/ops.rs::cherry_pick_clean_and_conflict`; E2E: `h-cherry-pick.script` | |
| Revert | Done | impl: `rewrite.rs:618`, `ops.rs:533`; UI: `history.rs:960`; test: `it/ops.rs::revert_clean_and_conflict`, `revert_conflict_continue`; E2E: `h-revert.script` | |
| Reset soft / mixed / hard | Done | impl: `rewrite.rs:717`, `ops.rs:543`; UI: `history.rs:963`, reflog "Reset to here…" (`reflog.rs:178`), palette "Reset current branch to selected commit…" (`palette.rs:124`); test: `it/ops.rs::reset_modes`; E2E: `h-reset-mixed.script`, `h-reset-hard.script` | |
| Undo last operation via reflog | Done | impl: `crates/gk-app/src/dialogs/undo.rs:41` (`plan_for`: commit or amend gives `reset --soft <branch>@{1}`; merge, rebase, reset, pull, cherry-pick and revert give `reset --keep <branch>@{1}`; checkout goes back to the previous ref), `undo.rs:100` (`make_plan` refuses during an operation in progress, on the first commit, and when local edits would be lost); UI: History toolbar "↶ Undo…" (`history.rs:407`), palette "Undo last operation…" (`palette.rs:106`); test: `undo.rs::plans`; E2E: `fa-undo-commit.script`, `fa-undo-merge.script`, `fa-undo-refuse.script` | Single step only (`@{1}`). No redo. No default shortcut. |

### Conflicts

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Conflict list | Done | impl: `Conflict` / `Kind::Conflict` in `crates/gk-status/src/lib.rs:18`, File Status banner (`views/file_status.rs:703`); test: `it/refs.rs::state_merge_conflict`, `state_rebase_conflict`, `state_cherry_pick_and_revert`, `it/session.rs::session_conflict_message`; E2E: `conflict-guard.script` | |
| Ours / theirs per file | Done | impl: `file_status/list.rs:837-843` (Resolve using mine / theirs, Mark resolved), `file_status.rs:572-584`, `ops.rs:556,567`; UI: conflicted-file context menu; test: `it/ops.rs::merge_conflict_ours_mark_resolved_abort`; E2E: `r-merge-ours.script` | |
| Built-in 3-way merge | Done | impl: `crates/gk-app/src/views/merge.rs:78,382`, `views/merge/diff3.rs`; UI: double-click a conflicted file, "Resolve in merge editor" (`list.rs:847`), palette "Resolve in merge editor: <path>" (`palette.rs:178`); test: `diff3.rs::*` (15 tests), `merge.rs` (3 tests); E2E: `merge.script`, `r-merge-ours.script`, `r-merge-both.script` | |
| External merge tool | Done | impl: `views/file_status/merge_tool.rs`, `file_status.rs:609`; UI: context menu "Open in external merge tool" (`list.rs:853`), Settings tools; test: `merge_tool.rs::splits_quoted_args` | No E2E. |
| Continue / skip / abort | Done | impl: `file_status.rs:703-766` (banner: Abort, Skip, Continue), `ops.rs:481,495,510`; test: `it/ops.rs::merge_conflict_resolve_continue`, `cherry_pick_conflict_skip_and_abort`, `revert_conflict_continue`, `it/rebase.rs::rebase_conflict_continue_skip_abort`, `interactive_conflict_then_abort` | |

### Remotes

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Fetch (all, prune) | Done | impl: `crates/gk-app/src/dialogs/remote.rs:467-520` ("All remotes" 501, prune 506, tags 509), `crates/gk-core/src/ops.rs:627`; UI: toolbar Fetch (`views/toolbar.rs:85`), sidebar "Fetch from X" (`views/sidebar.rs:948`), palette; test: `it/remote.rs::push_set_upstream_and_fetch`, `cancelled_fetch`; E2E: `r-fetch.script`, `fb-keymap-use.script` | |
| Pull (merge / rebase) | Done | impl: `remote.rs:29,149` ("Rebase instead of merge"), `ops.rs:649`; UI: toolbar Pull (`toolbar.rs:79`), palette; test: `it/remote.rs::pull_merge_and_rebase`, `pull_conflict_is_ok`; E2E: `r-pull-merge.script`, `r-pull-rebase.script` | |
| Push (force-with-lease, tags, to another branch) | Done | impl: `remote.rs:195-460` (editable remote branch per row 360, tags 372, force-with-lease 373), `ops.rs:675,710`; UI: toolbar Push (`toolbar.rs:82`), palette; test: `it/remote.rs::push_rejected_then_force_with_lease`, `push_tags_and_delete`, `push_new_branch_without_refspec_sets_upstream`; E2E: `r-push.script`, `r-force-push.script`, `r-push-branch.script`, `r-tag-push.script` | |
| Manage remotes | Done | impl: `remote.rs:533` (Add), `629` (Edit), `724` (Remove); UI: REMOTES "+", "Edit…" / "Remove…" (`sidebar.rs:952,955`), palette "Add remote…"; test: `it/remote.rs::remote_management`; E2E: `r-remote-add.script`, `r-remote-edit.script`, `r-remote-remove.script` | |
| Auto-fetch on a timer | Done | impl: `crates/gk-app/src/tab.rs:146-170` (through `Session::run_quiet_op`, `crates/gk-core/src/session.rs:409`, serialized on the op queue and quiet on failure); UI: Settings "Auto fetch (minutes)" (`dialogs/settings.rs:390`); test: `gk-core/src/session/tests.rs::quiet_ops_are_serialized_and_silent` | The timer itself has no test. |

### Authentication

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Git Credential Manager | Done | impl: `crates/gk-app/src/dialogs/diagnostics.rs` (`detect_auth`, `parse_helpers`, `is_gcm_helper`, `find_gcm`: Git for Windows `mingw64/bin` / `libexec/git-core`, exec path, PATH; version via `git credential-manager --version`); UI: More ▸ "Authentication…" (also Optimize ▸ Diagnostics ▸ "Authentication…"): effective `credential.helper` list with scopes, GCM found / version, "Use Git Credential Manager for all repositories…" (confirm, `git config --global --add credential.helper manager`); test: `diagnostics.rs::helper_parsing_and_gcm_names`, `gcm_found_but_not_configured`, `gcm_on_path` (fake probe); E2E: `fb-auth.script` | |
| SSH agent | Done | impl: `diagnostics.rs` (`resolve_ssh`: `GIT_SSH_COMMAND` > `core.sshCommand` > `GIT_SSH` > Git's own ssh; Git for Windows / Windows OpenSSH / plink; `ssh-add -l` of the matching OpenSSH: 0 keys, 1 none, 2 no agent; `sc query` / `sc qc ssh-agent`; `test_connection`: `ssh -T -o BatchMode=yes` for ssh remotes, `git ls-remote --heads` with a 20 s timeout and no prompts otherwise); `dialogs/settings.rs` "SSH" section (`core.sshCommand` global and for the open repository); UI: More ▸ "Authentication…", Settings ▸ SSH; test: `diagnostics.rs::ssh_resolution_order_and_flavor`, `ssh_add_and_service_parsing`, `detect_agent_for_each_ssh`, `remote_urls_and_connection_checks`, `ls_remote_test_against_local_bare_repo_and_timeout_runner`, `settings.rs::config_changes_only_for_edited_values`; E2E: `fb-auth.script` | Passphrases still go through `SSH_ASKPASS`. Pageant (plink) is not queried. |
| Password dialog via GIT_ASKPASS | Done | impl: `crates/gk-askpass/src/server.rs:95-97`, `crates/gk-app/src/app.rs:113` (server start), `app.rs:43` (prompt); test: `crates/gk-askpass/tests/askpass.rs::git_http_basic_auth_via_askpass`, `ssh_keygen_passphrase_via_ssh_askpass`, `helper_prints_answer_through_pipe`, `server.rs::answer_and_cancel` | |

### Advanced

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Submodules | Done | impl: sidebar SUBMODULES (`views/sidebar.rs:60`, menu Open / Update `1243-1248`, `Cmd::SubmoduleUpdate` `570`), `dialogs/repo_tools.rs:624` (SubmoduleAdd), `crates/gk-core/src/advanced.rs:304-354`; UI: More ▸ "Add submodule…" (`toolbar.rs:46`), palette; test: `it/advanced.rs::submodules_add_update_status_sync`, `sidebar/model.rs::submodule_status`, `advanced.rs::submodule_status` | No E2E. |
| Worktrees | Done | impl: sidebar WORKTREES (`sidebar.rs:61`, menu `1314-1342`, Add `1533`, Remove `1639`), `advanced.rs:262-298`; test: `it/advanced.rs::worktrees_add_list_remove_prune`, `advanced.rs::worktree_porcelain`, `sidebar/model.rs::worktree_porcelain` | No E2E. |
| LFS (lock / unlock) | Done | impl: `crates/gk-app/src/dialogs/lfs.rs:53` (unlock `259`, lock `281`, pull `382`), `advanced.rs:385-418`; UI: More ▸ "LFS…" (`toolbar.rs:42`), palette "Git LFS…"; test: `lfs.rs::lfs_patterns`, `it/advanced.rs::lfs_track_commit_pointer` (skipped without git-lfs) | Lock / unlock are not tested, because they need an LFS server. |
| Bisect | Done | impl: `crates/gk-app/src/dialogs/bisect.rs:93`, `advanced.rs:360-379`; UI: More ▸ "Bisect…" (`toolbar.rs:41`), toolbar badge while bisecting (`toolbar.rs:113-132`), palette; test: `it/advanced.rs::bisect_finds_first_bad_commit`, `bisect_skip_and_explicit_revs`, `bisect_rejects_option_like_revisions`, `bisect.rs::log_parsing`, `output_parsing` | No E2E. |
| Sparse checkout | Done | impl: `dialogs/repo_tools.rs:41`, `advanced.rs:426-446`; UI: More ▸ "Sparse checkout…" (`toolbar.rs:43`), palette; test: `repo_tools.rs::sparse_dirs`, `it/advanced.rs::sparse_set_list_disable` | |
| Patch create / apply | Done | impl: `crates/gk-app/src/dialogs/misc.rs:24,125`, `ops.rs:788,797`; UI: History "Create patch…" (`views/history.rs:967`), More ▸ "Apply patch…" (`toolbar.rs:48`), palette; test: `it/ops.rs::format_and_apply_patch` | |
| Archive | Done | impl: `misc.rs:185`, `ops.rs:808`; UI: History "Archive…" (`history.rs:970`); test: `it/ops.rs::archive_formats` | |
| Notes | Done | impl: `crates/gk-app/src/dialogs/notes.rs:12-131` (multi-line editor, `notes add -f -F -`, `notes remove --ignore-missing`, on the op queue), shown in commit details (`views/details.rs:123,189-197`); UI: History commit menu "Add/Edit note…" / "Remove note…" (`history.rs:1023,1026`), palette "Add/Edit note of selected commit…" (`views/palette.rs:120`); E2E: `fa-notes.script`, `fa-notes-remove.script` | No unit or integration test. Only `refs/notes/commits` is used. |
| One-click repo optimization (fsmonitor, untrackedCache, commit-graph, multi-pack-index, maintenance) | Done | impl: `dialogs/repo_tools.rs:391` (plus Diagnostics at `546`), `advanced.rs:583,614`; UI: More ▸ "Optimize repository…" (`toolbar.rs:45`), palette; test: `repo_tools.rs::optimization_checklist`, `it/advanced.rs::optimize_sets_config_and_writes_commit_graph`; E2E: `fb-optimize-diag.script` | |

### Tools

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Terminal and file explorer in repo folder | Done | impl: `crates/gk-app/src/views/toolbar.rs:140-148,180`; UI: toolbar Terminal and Explorer, palette "Open terminal" / "Show in Explorer" (`palette.rs:162-163`) | No test. |
| External diff / merge tools | Done | impl: `crates/gk-app/src/views/diff/external.rs:41,111` (`$LOCAL` / `$REMOTE` temp sides; for unstaged changes, the worktree file), `views/diff.rs:1283`; merge tool `views/file_status/merge_tool.rs`; UI: file context menu "Open in external diff tool" (`views/file_status/list.rs:914`), diff header (`diff.rs:188,702`), commit details (`details.rs:556`), Settings tools (`settings.rs:104,152`); test: `external.rs::writes_both_sides`, `merge_tool.rs::splits_quoted_args`; E2E: `fb-external-diff.script` | |
| Editor for .gitconfig / .gitignore | Done | impl: `crates/gk-app/src/dialogs/config_edit.rs:161` (`.gitignore` with an ignored-path preview at `110`; repo and global config validated by `git config -f --list` at `96`, `.bak` backup at `139`); UI: More ▸ "Edit .gitignore…", "Edit repository config…", "Edit global git config…" (`toolbar.rs:49-53`), palette (`palette.rs:144-152`); test: `config_edit.rs::validate_save_and_preview`; E2E: `fb-gitignore.script`, `fb-repo-config.script`, `fb-repo-config-invalid.script`, `fb-global-config.script` | |
| Log of all executed git commands | Done | impl: `crates/gk-cli/src/log.rs`, `crates/gk-app/src/views/log.rs`; UI: "Output" toggle (`app.rs:305`), palette "Toggle output log" (`palette.rs:100`); test: `crates/gk-cli/src/lib.rs::runs_git_and_logs` | |
| Command palette | Done | impl: `crates/gk-app/src/views/palette.rs`; UI: Ctrl+Shift+P (`keymap.rs` `Act::Palette`, `app.rs:389`); test: `palette.rs::fuzzy_scoring`; E2E: `palette.script`, `fa-undo-merge.script` | |
| Built-in git console | Done | impl: `dialogs/misc.rs:366`; UI: More ▸ "Git console…" (`toolbar.rs:47`), palette; test: `misc.rs::destructive_commands`, `common.rs::argument_splitting` | |

## UI/UX

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| SourceTree layout (sidebar, toolbar, history graph top and details bottom, File Status staged top / unstaged bottom / diff right / commit box bottom, Search view) | Done | impl: `crates/gk-app/src/tab.rs:226-254` (toolbar, status bar, left sidebar panel, central view), `views/history.rs:103,209-253` (split), `views/file_status.rs:260-262` (banner, commit panel), `views/search.rs`; test: `file_status.rs::renders_headless`, `history/ui_tests.rs::history_view_selection_keyboard_and_worktree_row` | |
| SourceTree-style context menus | Done | impl: `views/history.rs:901` (`commit_menu`), `views/sidebar.rs:631,858,947,1243,1314`, `views/file_status/list.rs:554,624`, `views/diff.rs:649` | |
| Drag a branch onto a branch to merge or rebase | Done | impl: `views/sidebar.rs:1011-1030` (drag payload / drop), popup `1593-1630` (Merge, Rebase, Checkout + merge, Checkout + rebase), `1671-1690`; E2E: `fa-drag-merge.script`, `fa-drag-checkout.script` | Sidebar rows only; branch labels in the graph cannot be dragged. The rebase choices have no E2E. |
| Every action keyboard-accessible | Done | impl: `app.rs:278` (Ctrl+O, W, Tab, Shift+P), `tab.rs:172` (F5, Ctrl+1/2/3), Ctrl+Enter commit, History keys; scoped keymap actions (`keymap.rs` `Scope::FileList` / `Scope::Diff`, editable in Settings › Keyboard); File Status lists: Space stage/unstage, Delete discard (confirm), H file history, B blame, E external diff, I ignore, Shift+F10 context menu at the cursor row (`views/file_status/list.rs` `key_action`); diff: N / P next / previous hunk, S / U / Delete stage / unstage / discard the current hunk or the selected lines, Shift+Down / Shift+Up line selection (`views/diff.rs` `key_action`); palette: merge, rebase, interactive rebase, cherry-pick, revert, reset, tag, patch, archive and notes on the selected commits, blame of the selected file at the selected commit, stash apply / pop / drop / branch, add / open / remove worktree, update / open submodule (`views/palette.rs` `items`); sidebar submodule and worktree rows: Shift+F10, Enter, Delete; menus show their shortcuts; test: `keymap.rs::scoped_shortcuts_conflict_only_where_both_fire`, `list.rs::keyboard_actions_on_selection_and_cursor`, `diff/model.rs::keyboard_hunk_and_line_navigation`; E2E: `fb-keys-status.script`, `h-keyboard.script`, `fa-keys-history.script`, `fa-keys-sidebar.script` | The Menu (Apps) key is not delivered by egui-winit; Shift+F10 opens the keyboard menu (mapping the Menu key needs a change in `gk-dx11`). |
| Command palette Ctrl+Shift+P | Done | `keymap.rs:39`, `app.rs:389`; E2E: `palette.script` | |
| Configurable hotkeys | Done | impl: `crates/gk-app/src/keymap.rs` (`Keymap::from_overrides`, conflict check), applied in `app.rs:364-390` and `tab.rs:173-182`; UI: Settings › Keyboard (`dialogs/settings.rs:203-221`); test: `keymap.rs::parse_and_format_roundtrip`, `defaults_have_no_conflicts`, `overrides_unbind_report_and_conflict`, `consume_needs_exact_modifiers`; E2E: `fb-keymap.script`, `fb-keymap-use.script` | In-list keys (Shift+F10, Enter, Ctrl+B/T, Delete, arrows, Space) cannot be rebound. |
| Light / dark themes | Done | impl: `dialogs/settings.rs:371-372`, `crates/gk-app/src/theme.rs:65,88,244`; E2E: `r-settings.script`, `r-settings-reopen.script` | |
| Compact density | Done | impl: `theme.rs` `Density` (Settings › Density: Compact, the default, or Comfortable) applied to global `item_spacing` / `button_padding` / `interact_size` (`theme::apply_density`, called from `app.rs`), File Status rows, sidebar rows, History / Search / Reflog rows and diff rows; commit-details and stash file lists follow `interact_size`; plus UI scale (`settings.rs:232`), History row height (`settings.rs:329`), toolbar auto-compact under 1080 px (`toolbar.rs:9`); test: `theme.rs::compact_density_keeps_the_classic_look` | Compact keeps the existing look (the default, so E2E coordinates are unchanged); Comfortable adds room everywhere. |
| Configurable monospace font | Done | impl: family `theme.rs:137-173` (system font lookup `228-239`, `install_fonts_with`), size `theme.rs:244-293`, applied in `app.rs:93,217,542-551`; UI: Settings "Monospace font size" / "Monospace font" (`settings.rs:382,421-426`); test: `theme.rs::mono_font_names_resolve`, `mono_size_is_applied` | |
| Long operations in status bar with progress and cancel | Done | impl: `crates/gk-app/src/views/statusbar.rs:85-96` (spinner, Cancel), jobs panel for clones (`app.rs:473-500`), `crates/gk-core/src/session.rs:389` (op queue); test: `it/remote.rs::cancelled_fetch` | |
| No blocking "please wait" modals | Done | impl: clone runs as a background `CloneJob` (`dialogs/global.rs:65-81,193-266,396`): the dialog closes at once, progress and Cancel are in the jobs panel (`app.rs:473`), and the tab opens when done; dialogs that wait on an op (`TrackedOp`, `dialogs/common.rs:498-545`) show a spinner but can be closed; E2E: `fb-clone-sparse.script` | |

## Architecture / performance

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| UI thread never calls git | Done | impl: `crates/gk-core/src/session.rs:337` (`snapshot()` of Arcs), `session.rs:533` (`load` slots, ticketed), `Task::spawn` (`crates/gk-app/src/dialogs/common.rs:469`) used by the notes/undo/config dialogs (`dialogs/notes.rs:15`, `dialogs/undo.rs:197`, `dialogs/config_edit.rs:200`); keyed background tasks in `views/reflog.rs:115`, `views/stash_view.rs:153`; `docs/ARCHITECTURE.md` | No automated guard. Every `Command::new("git")` in gk-app is inside `#[cfg(test)]`. |
| Cancellation tokens and generation numbers | Done | impl: `crates/gk-cli/src/lib.rs` (CancelToken); `session.rs:204-207` (search, status and history interrupts), `session.rs:633` (`request_history` interrupts the running build), `session.rs:833-896` (history_loop), `session.rs:508` (a newer search cancels the old one), Load tickets (`session.rs:533`); test: `it/history.rs::cancelled_build`, `gk-app/src/views/history/task.rs::newer_key_wins`, `gk-core/src/session/tests.rs::partial_history_then_full` | |
| Priorities (visible > prefetch > background) | Done | impl: `crates/gk-core/src/pool.rs:18` (3-thread `gk-fg` foreground pool), `pool.rs:52-62` (`Priority::Visible` / `Background`); metas, diff and details run as Visible (`session.rs:467,490`, `history.rs:455`), search as Background (`session.rs:520`); test: `session/tests.rs::metas_complete_while_background_pool_is_busy` | There are two tiers: prefetch shares the background pool. |
| Write ops serialized | Done | impl: gk-ops thread `session.rs:915` (`op_loop`), `run_op` / `run_quiet_op` / `run_index_op` (`session.rs:389,409,429`); auto-fetch goes through `run_quiet_op` (`crates/gk-app/src/tab.rs:146,162`); test: `it/session.rs::session_end_to_end`, `session/tests.rs::quiet_ops_are_serialized_and_silent`, `own_op_is_refreshed_once` | |
| Interning / arenas | Partial | impl: dense `u32` commit rows (`history.rs:23-66`), `IStr` interner for author and email (`history.rs:255,344,406-422`), `InternedInput` (`crates/gk-diff/src/lib.rs:12`) | Status paths are not interned: `Entry.path` is still a `BString` (`crates/gk-status/src/lib.rs:76`). |

## Status engine

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Index via gix | Done | impl: `crates/gk-status/src/gix_engine.rs:16` (app default: `gk-settings/src/lib.rs:144-146`); test: `crates/gk-status/tests/engines.rs::gix_matches_cli` | |
| Parallel full scan | Done | impl: gix status (`gix_engine.rs:16-26`: index-worktree, dirwalk and tree-index in parallel) | Measured on r1m: 415 ms full refresh (CLI engine), gix 898 ms, `git status` 407 ms. |
| Incremental updates from file watcher (ReadDirectoryChangesW) | Done | impl: `crates/gk-status/src/watch.rs:93,153` (notify, which uses RDCW on Windows), `crates/gk-status/src/fast.rs:35,77` (FastStatus, case-insensitive under `core.ignorecase`), `session.rs:603-617` (`event_loop`); test: `watch.rs::delivers_worktree_events`, `engines.rs::fast_path_matches_cli`, `it/paths.rs::fast_path_case_only_rename_with_ignorecase` | Measured: 0.2 ms for a watcher-triggered fast refresh. |
| Debounce | Done | impl: `watch.rs:22-33` (adaptive: 15 ms quiet, 400 ms max; after 3 flushes within 1 s it switches to 150 ms / 1 s); test: `watch.rs::pace_switches_to_busy_after_frequent_flushes`, `save_reaches_the_session_quickly`, `session/tests.rs::status_after_save_is_fast` | Measured: 22 ms median from save to status. |
| .git watch | Done | impl: `watch.rs:229` (`classify`), `session.rs:617` (`Event::GitDir`); test: `watch.rs::git_dir_classification`, `it/session.rs::session_external_ref_change` | |
| Overflow leads to full rescan | Done | impl: `watch.rs:17` (`OVERFLOW_PATHS` = 2000), `watch.rs:146-166,316,356` (`Raw::Overflow` → `Event::Overflow`), `session.rs:613` | No dedicated test. |
| Cold start from cached snapshot | Done | impl: `crates/gk-core/src/cache.rs` (redb at `<config dir>/cache.redb`: refs, status and history; 32 repos; versioned encoding), `session.rs:260-271` (on by default, `session.rs:99`), `session.rs:777` (`publish_cached`, then validated live, `from_cache` flag); test: `cache.rs::refs_round_trip`, `status_round_trip`, `history_round_trip`, `database_store_load_and_evict`, `writer_coalesces_and_flushes`, `session/tests.rs::cached_snapshot_is_published_then_validated` | Statuses over 200k entries are not cached (`cache.rs:32`). |
| Optional fsmonitor daemon query | Partial | impl: the CLI engine's `git status` uses the daemon when `core.fsmonitor` is set (`crates/gk-status/src/cli_engine.rs:7-13`); `fsmonitor_state()` (`gk-status/src/lib.rs:185-215`); Optimize turns it on (`gk-core/src/advanced.rs:615`); test: `gk-status/src/lib.rs::fsmonitor_detection` | The default gix engine and the watcher fast path never query the daemon. Nothing in the app calls `fsmonitor_state()`. |
| CLI engine switch as reference | Done | impl: `crates/gk-status/src/cli_engine.rs`, Settings "Status engine" (`gk-app/src/dialogs/settings.rs:399-401`), applied live (`gk-app/src/app.rs:227`); test: `engines.rs::gix_matches_cli` | |

## History engine

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Rev-walk with commit-graph | Done | impl: `crates/gk-core/src/history.rs:83-101` (`build_with_partial`, `use_commit_graph(true)`); test: `it/history.rs::*` | Measured: git.git 97 ms full build; Linux (1.48M commits) 2.48 s. |
| Offer to write commit-graph | Done | impl: `crates/gk-app/src/views/commit_graph_offer.rs` (non-modal banner under the toolbar when the loaded history has more than `COMMIT_GRAPH_OFFER_MIN_COMMITS` = 20k commits and `Repo::has_commit_graph` is false, checked once on a background thread; "Write" queues `Repo::write_commit_graph` on the session's op queue; "Don't ask for this repository" sets `RepoState::commit_graph_offer_dismissed`), `crates/gk-core/src/advanced.rs` (`offer_commit_graph`, `has_commit_graph`, `write_commit_graph`); the Optimize dialog also writes it; test: `it/advanced.rs::write_commit_graph_and_offer`, `gk-settings/src/tests.rs::repo_state_roundtrip_and_path`; E2E: `fb-commit-graph-offer.script`, `fb-commit-graph-never.script` | |
| Lane layout in chunks | Done | impl: `crates/gk-graph/src/layout.rs:47-50,216,233` (512-row blocks, lazy edges); `history.rs:71` (`PARTIAL_ROWS` = 2000: the first rows are published while the walk continues), `session.rs:860-878` (`on_partial`); test: `layout.rs::lazy_edges_match_eager_computation`, `million_rows_is_fast_and_small`, `session/tests.rs::partial_history_then_full` | There is one early publish of 2,000 rows, then the full result. Measured: 19.5 ms to first rows on git.git. |
| On-disk layout cache | Done | impl: `cache.rs` stores ids, parent rows, tips and scope (`cache.rs:51-56`); lanes are rebuilt from them with `History::from_rows` (`cache.rs:731`); the cache is reused when tips are unchanged (`session.rs:838-855`); test: `cache.rs::history_round_trip`, `session/tests.rs::cached_snapshot_is_published_then_validated` | Lane positions are recomputed, not stored (1M rows: 15-17 ms, `layout_1m_rows`). Measured: 29 ms to first rows from the warm cache. |
| Lazy metadata + LRU | Done | impl: `history.rs:401-455` (`MetaCache`, LRU of 20k, foreground pool, interned author and email); test: `it/history.rs::commit_meta_and_cache`, `session/tests.rs::metas_complete_while_background_pool_is_busy` | |
| Search filters in background | Done | impl: `session.rs:501-521` (background pool; a newer needle interrupts), `history.rs:498`; History filter row (`gk-app/src/views/history/filter.rs`); test: `it/history.rs::search_message_author_id`, `filter.rs::builds_git_log_args`; E2E: `h-search.script`, `fa-filter-text.script`, `fa-filter-path.script` | Measured: 75.6 ms on git.git, 492 ms on Linux. |
| Pickaxe via streaming `git log` | Done | impl: `gk-app/src/views/history/filter.rs:17,91-94` (`-S` / `-G`), `filter.rs:203-258` (`run_streaming`: ids pushed as they arrive, cancelled on change); UI: History filter row (`views/history.rs:454`); test: `filter.rs::builds_git_log_args`; E2E: `fa-pickaxe.script` | |
| Refs tree for tens of thousands of refs | Done | impl: one `for-each-ref` (`gk-core/src/refs.rs:168-177`), sidebar trees with filter and lazy rows (`gk-app/src/views/sidebar.rs:198,353,1850`) | Not measured at 10k+ refs. |
| Blame streaming via `--incremental` | Partial | impl: `gk-core/src/advanced.rs:251-252` (blocking `blame --porcelain -w` on the `gk-blame` thread, `gk-app/src/views/blame.rs:90`) | Nothing shows until the whole blame is done. |

## Diff

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| imara-diff | Done | impl: `crates/gk-diff/src/lib.rs:12`; test: `gk-diff/src/lib.rs::*` | |
| Unified / side-by-side | Done | impl: `gk-app/src/views/diff.rs:368` ("Split"); test: `diff/model.rs::pairing_runs`, `layout_rows_and_partners` | |
| Word diff | Done | impl: `diff.rs:375` ("Words"), `gk-diff/src/words.rs`; test: `words.rs::highlights_changed_word` | |
| Ignore whitespace | Done | impl: `diff.rs:383-386`; test: `gk-diff/src/lib.rs::whitespace_ignored` | Hunk and line staging are turned off in this mode. |
| Context lines | Done | impl: `diff.rs:390-399` (whole-file toggle and count); E2E: `r-settings.script` | |
| Rename detection | Done | impl: status `--find-renames` (`gk-status/src/cli_engine.rs:20`); tree diffs `gk-core/src/content.rs:196-210` (`set_detect_renames`); Settings "Detect renames in commit file lists" (`dialogs/settings.rs:481`, `gk-settings/src/lib.rs:162`), applied in `app.rs:116,211`; test: `search.rs::file_log_follows_renames` | No similarity threshold. Status always detects renames. The toggle has no test. |
| Large files with "show anyway" threshold | Done | impl: `gk-app/src/views/diff/guard.rs:1-60` (background size probe), `diff.rs:162-184` ("File is large", "Show anyway"), `settings.diff.max_file_kb` (`dialogs/settings.rs:474`); test: `guard.rs::probes_sizes_and_gates`; E2E: `fb-large.script` | The hard cap `MAX_DIFF_BYTES` = 64 MB still applies (`content.rs:9`). |
| Syntax highlighting of visible lines | Deviation | impl: `gk-app/src/views/diff/syntax.rs` (the whole diff is highlighted per hunk in the background, syntect loads lazily, highlighting is skipped above 20k lines, `syntax.rs:20`); test: `syntax.rs::highlights_both_sides_of_a_hunk`, `long_lines_and_huge_diffs_are_skipped`, `cache_keys_by_diff_pointer_and_theme` | |
| Binary detection | Done | impl: `gk-diff/src/lib.rs:141,164`, `diff.rs:233,456` (`binary_info`); test: `gk-diff/src/lib.rs::binary_detection`, `it/ops.rs::stage_lines_binary_rejected` | |
| Image diff (side by side / overlay / slider) | Done | impl: `gk-app/src/views/diff/preview.rs:126-135,201,236-242,449-486` (side by side, swipe, overlay/onion skin with opacity); test: `preview.rs::overlay_tint_follows_opacity`, `fit_scales_and_centers`, `decodes_and_caps_sizes`; E2E: `fb-overlay.script` | |
| LFS pointers with size | Done | impl: `guard.rs:113` (`LfsPointer` parse), `diff.rs:234-237,1287-1320` (`lfs_info`: object id and size, "show raw" toggle); test: `guard.rs::detects_lfs_pointer_changes`; E2E: `fb-lfs.script` | No action to fetch the LFS object. |
| Staging file / hunk / lines | Done | See Working copy. | |
| Discard with confirmation | Done | impl: `diff.rs:1119-1127` (`confirm_modal`), `dialogs/worktree.rs:16,69` (`DiscardPlan`, `Discard`), Settings confirmations; E2E: `r-discard-file.script` | |
| Bulk ops via pathspec-from-file | Done | impl: `gk-core/src/ops.rs:16-38` (over `BULK_PATHS` = 1000 (`ops.rs:1017`): `update-index --add --remove -z --stdin`, otherwise `add --pathspec-from-file`), `ops.rs:45-73,82-116` (unstage through `update-index --index-info`), `stage_all` / `unstage_all` (`ops.rs:77,116`); test: `it/ops.rs::stage_treats_paths_literally`, `bulk_stage_and_unstage_match_small_path`; E2E: `fb-stage-all.script`, `fb-unstage-all.script` | 100k files: 8-11 s, bound by object writes. |
| Built-in 3-way conflict view | Done | See Conflicts. | |

## Write operations

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| git CLI ≥ 2.45 check at startup | Done | impl: `crates/gk-cli/src/lib.rs:19` (`MIN_VERSION`), `lib.rs:303` (`check_minimum`), `crates/gk-app/src/app.rs:70` (`check_git`), `app.rs:100-110` (`gk-git-version` background thread at startup), `app.rs:424` (non-modal banner with Dismiss and a download link); test: `gk-cli/src/lib.rs::version_parsing`; E2E: `fb-git-banner.script` (`GITKWAKEN_FAKE_GIT_VERSION=2.30.1`) | |
| Optional bundled MinGit | Done | impl: `crates/gk-cli/src/lib.rs:388` (`find_git` prefers `<exe_dir>/git/cmd/git.exe`), `packaging/build-release.ps1:57,153` (`-BundleGit`: downloads busybox-free MinGit, checks its SHA-256, refuses versions below 2.45), `packaging/gitkwaken.iss:75,100` | Opt-in at build time. A bundled git is preferred over the system git even when the system git is newer. |
| Process runner: streaming, progress, cancel via Job Object | Done | impl: `crates/gk-cli/src/lib.rs:205` (`run_streaming`), `crates/gk-cli/src/job.rs:14,30`; test: `lib.rs::streaming_reports_lines`, `line_splitting_handles_cr_progress`, `it/remote.rs::cancelled_fetch` | |
| Long-lived `cat-file --batch` | Deviation | impl: objects are read in-process through gix (`crates/gk-core/src/content.rs:72`) | Reasonable substitute, but `docs/ARCHITECTURE.md` does not record it as a deviation. |
| GIT_ASKPASS / SSH_ASKPASS / GIT_EDITOR / GIT_SEQUENCE_EDITOR | Done | impl: `crates/gk-askpass/src/server.rs:95-97`, `crates/gk-core/src/ops.rs:419,471-472,491`; test: `askpass.rs::*`, `it/rebase.rs::interactive_*` | |
| Confirmations before reset --hard / discard / force-push | Done | impl: `crates/gk-settings/src/lib.rs:255` (`Confirmations`), `dialogs/settings.rs:516`, `dialogs/remote.rs:301,373-377`, `dialogs/rewrite.rs:717,744`; E2E: `h-reset-hard.script`, `r-force-push.script`, `r-discard-file.script` | |
| Discarded changes saved to a hidden stash object (`git stash create`) | Missing | `crates/gk-core/src/ops.rs:126` (`discard`: checkout plus delete, no backup) | There is no `stash create` and no backup ref anywhere in `crates/`. |
| Output panel with all commands | Done | See Tools (log of commands). | |

## Stack

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Rust | Done | `Cargo.toml:7` (edition 2024) | |
| UI: egui + DX11 instead of GPUI | Deviation | `crates/gk-dx11`, `docs/ARCHITECTURE.md` (Rendering) | Documented. |
| gix | Done | `Cargo.toml:11` (gix 0.89) | |
| git CLI | Done | `crates/gk-cli` | |
| imara-diff | Done | `crates/gk-diff/Cargo.toml:8` | |
| Syntax highlighting | Done | syntect (`crates/gk-app/Cargo.toml:29`), loaded lazily (`views/diff/syntax.rs:32`, `OnceLock`) | |
| Fuzzy search (nucleo) | Done | nucleo-matcher 0.3: command palette (`crates/gk-app/src/views/palette.rs` `Ranker`: exact > prefix > word start > nucleo score, matched characters highlighted) and the File Status filter (`views/file_status/tree.rs` `Filter`); test: `palette.rs::exact_and_prefix_first`, `palette.rs::highlight_indices_follow_chars` | |
| Watcher | Done | notify 8 (`crates/gk-status/Cargo.toml:14`) | |
| rayon | Done | workspace dependency (`Cargo.toml:16`) | |
| Disk cache (redb) | Done | `crates/gk-core/Cargo.toml:21` (redb), `crates/gk-core/src/cache.rs` (refs, status, history), used in `session.rs:262,271`; test: `cache.rs::refs_round_trip`, `status_round_trip`, `history_round_trip`, `database_store_load_and_evict`, `writer_coalesces_and_flushes`, `session/tests.rs::cached_snapshot_is_published_then_validated` | |
| TOML settings | Done | `crates/gk-settings/src/io.rs:27,56`; test: `gk-settings/src/tests.rs::*` | |
| tracing + Tracy profiling | Done | `tracing` spans in `gk-core` (`history.rs`, `session.rs`, `refs.rs`, `cache.rs`) and `gk-status/src/lib.rs:223`; Tracy through the `profile` feature (`crates/gk-core/Cargo.toml:10`, `crates/gk-core/src/profiling.rs`, `crates/gk-app/Cargo.toml:37`, `gk-app/src/main.rs:21`) | gk-app has no spans for the UI frame, and `ops.rs` has none apart from the `op` span in `op_loop`. |
| cargo-dist / MSI / winget | Deviation | Inno Setup installer and zip (`packaging/gitkwaken.iss`, `packaging/build-release.ps1`, `.github/workflows/release.yml`), winget manifests in `packaging/winget/` | No cargo-dist and no MSI. |
| Auto-update | Done | impl: `crates/gk-update/src/lib.rs:107`, `crates/gk-app/src/update.rs`; test: `gk-update/src/tests.rs::*`; E2E: `scripts/run-e2e-update.sh` | |

## Tests

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Differential status tests vs git | Done | `crates/gk-status/tests/engines.rs::gix_matches_cli`, `fast_path_matches_cli`; `it/paths.rs` (fast path vs full status on edge-case names) | |
| Integration tests of operations | Done | `crates/gk-core/tests/it/{ops,rebase,remote,refs,content,history,session,repo,advanced,paths}.rs` (108 tests; `advanced.rs` has 15: bisect, flow, sparse, worktrees, submodules, clean, optimize, LFS, blame) | |
| UI tests | Done | `history/ui_tests.rs`, `file_status.rs::renders_headless`, `diff.rs::hunk_actions_on_a_real_repository`; E2E: `run-e2e.sh`, `run-e2e-history.sh`, `run-e2e-remote.sh`, `run-e2e-update.sh`, `run-e2e-features-a.sh` (18 `fa-*`), `run-e2e-features-b.sh` (18 `fb-*`); CI: `.github/workflows/e2e.yml` runs all six suites through `scripts/ci-e2e.sh` (windows-latest, WARP) | |
| Benchmark corpus (linux kernel, chromium, synthetic 1M, LFS repo) | Done | `scripts/fetch-corpus.sh` (`linux`, `chromium`, `lfs`), `gk-bench gen` / `gen-dag`; `docs/BENCHMARKS.md` (Corpus) | Chromium can be fetched by script but was not fetched or measured. |
| Benchmarks in CI failing on >10% regression | Partial | `crates/gk-bench/src/regress.rs` (gate: 10% and 2 ms), `.github/workflows/bench.yml` | No baseline is committed (`bench/` does not exist), so CI only records a baseline and gates nothing until `bench/baseline-windows.json` is committed. |
| Comparison with SourceTree / Fork / GitKraken / SourceGit / git CLI before releases | Partial | `scripts/compare-clients.ps1`; `docs/BENCHMARKS.md` (Comparison): window shown sooner than SourceGit (225-256 ms vs 367-490 ms), 0% idle CPU vs about 55% | Only SourceGit and the git CLI were measured. The script supports Fork, but Fork was not run. SourceTree and GitKraken are not covered. The comparison is not part of the release process. |

## Performance budgets

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| First frame < 300 ms from cache | Partial | redb snapshot published on open (`session.rs:262`, `cache.rs`); `gk-bench open` (`crates/gk-bench/src/startup.rs`, Session::open to first rows): 29 ms from a warm cache; window shown in 225-256 ms (`scripts/compare-clients.ps1`); test: `session/tests.rs::cached_snapshot_is_published_then_validated` | Both parts are under budget, but they were measured separately. There is no single measurement from process start to the first frame with cached content. |
| History first screen < 100 ms | Done | first 2,000 rows are published before the full layout (`history.rs:71`, `session.rs:860-878`); git.git first rows: 19.5 ms without cache (`gk-bench history-first`), 29 ms from cache (`gk-bench open`); test: `session/tests.rs::partial_history_then_full` | |
| Full status warm < 1.5 s at 1M files | Done | r1m full refresh 415 ms (CLI engine), gix 898 ms (`gk-bench status`) | |
| Status after save < 50 ms | Done | adaptive 15 ms debounce (`crates/gk-status/src/watch.rs:22`; busy mode 150 ms at `watch.rs:32`), fast path 0.2 ms; save to status, end to end: 22 ms median (`gk-bench save-latency`); test: `session/tests.rs::status_after_save_is_fast` | |
| 1M-row scroll at 120 fps, no frame > 16 ms | Partial | 120 fps scrolling history; 117 fps (vsync) and > 900 fps unlocked on a 1M-row list; Linux (1.48M commits) history build 2.48 s | Frame-time spikes are not recorded (no frame-time histogram). Scrolling the 1.48M-commit history is not measured. |
| Stage 100k files < 2 s | Partial | above 1,000 paths: `update-index --add --remove -z --stdin` / `--index-info` (`crates/gk-core/src/ops.rs:16-73`); `gk-bench stage` | Not met: 8-11 s, bound by writing 100k loose objects on this disk (it was 152 s). |
| Filter 1M paths < 50 ms per keystroke | Done | `views/file_status/filter.rs` (rayon over 8k-entry chunks, results concatenated in status order; when the query only grows, only the previous matches are filtered), `views/file_status/list.rs` `sync`; test (release, `--ignored`): `filter.rs::filter_1m_paths_per_keystroke`, `list.rs::sync_1m_paths_per_keystroke` | 1M synthetic paths (`src/module_NNN/sub_NN/file_NNNNNNN.ext`), 32 hardware threads: filter 4-14 ms per keystroke (one thread: 47-272 ms), whole flat-list row rebuild 8-24 ms. Fewer cores scale the full pass down accordingly; typing forward uses the smaller incremental set. |
| Open a 50k-line diff < 100 ms | Partial | `gk-bench diff 50000`: diff 19.8 ms, whole file 15.3 ms, ignore whitespace 33.5 ms | Only the diff computation is measured, not open-to-rendered in the app. Highlighting is skipped above 20k lines (`views/diff/syntax.rs:20`). |
| File history < 1 s for the first 50 | Done | `views/search.rs:18-19,222-243` (a `-n 50` first page is shown while the full `--follow` walk runs in parallel); `gk-bench file-history`: 48-244 ms for the first 50 on git.git (full `--follow` walk 484-786 ms) | |
| Memory < 60 MB small repo / < 400 MB reference | Partial | demo repo about 76 MB (was 107 MB: emoji font dropped, fonts memory-mapped, no CPU copy of textures, syntect lazy); r1m 368 MB; Linux 1.17 GB peak (whole gk-bench process) | Small repo is over budget: about 42 MB is shared driver/DLL pages and the full syntect syntax set is about 8 MB. Linux is far over 400 MB. |

## Risks

| Requirement | Status | Evidence | Note |
|---|---|---|---|
| Defender / Dev Drive hint and diagnostics measurement | Done | impl: `crates/gk-app/src/dialogs/diagnostics.rs:46` (write-speed test: 200 blobs with `hash-object -w`), `:97` (`fsutil devdrv query`), `:121,131` (commands for a Defender exclusion and a Dev Drive hint); UI: Optimize ▸ Diagnostics "Measure object write speed" (`dialogs/repo_tools.rs:535-551`); test: `diagnostics.rs::measures_writes_and_cleans_up`; E2E: `fb-optimize-diag.script` | It does not read the current Defender exclusions. The hint only prints commands. |
| Case / Unicode / long-path config respected | Done | `packaging/gitkwaken.manifest:37` (longPathAware), literal pathspecs, case-insensitive fast path when `core.ignorecase` is set (`crates/gk-status/src/fast.rs`); test: `it/paths.rs::special_characters_in_names`, `unicode_names`, `nfc_and_nfd_names_are_distinct`, `long_paths_over_260_chars`, `many_edge_case_paths_staged_in_one_call`, `case_only_rename_with_ignorecase`, `fast_path_case_only_rename_with_ignorecase` | Optimize does not set `core.longpaths`; only the tests set it. |
| Watcher paused during own ops | Done | `crates/gk-status/src/watch.rs:102,106,115` (`pause`, `resume`, `resume_discarding`), called in `crates/gk-core/src/session.rs:927,991` (`op_loop`); test: `watch.rs::resume_discarding_drops_own_changes`, `session/tests.rs::own_op_is_refreshed_once` | Quiet ops (auto-fetch) deliberately leave the watcher running. |

## Gaps

One line per Partial or Missing item: what to add, and where. 18 Partial and 1 Missing.

**Repositories**

- Bookmarks (Partial): add "Bookmark this repository…" to the tab context menu (`app.rs:263`), the "Repositories ▾" menu (`views/welcome.rs:347`) and the palette (`views/palette.rs`), so you can bookmark while tabs are open.

**Authentication**

- Git Credential Manager (Partial): detect `credential.helper` and the GCM install, and show the status in Optimize › Diagnostics or About (`dialogs/diagnostics.rs`, `dialogs/repo_tools.rs:546`).
- SSH agent (Partial): detect the Windows `ssh-agent` service or `SSH_AUTH_SOCK`, list keys (`ssh-add -l`), and offer `core.sshCommand` in Settings (`dialogs/settings.rs`, `dialogs/diagnostics.rs`).

**UI/UX**

- Keyboard access (Partial): add Shift+F10 / `ListKeys` to the File Status lists (reuse `history/keys.rs::keyboard_menu` with the menus at `list.rs:554,624`) and Delete = discard. Add keymap actions for stage/unstage/discard of the hunk or selected lines in the diff (`views/diff.rs:735-743`).
- Compact density (Partial): apply `settings.compact` to the sidebar row height, the History/Reflog row height and `spacing.item_spacing` in `theme::apply_with` (`theme.rs:244`, `views/sidebar.rs`, `views/history.rs:531`).

**Architecture**

- Interning (Partial): intern status paths (an `Arc<str>` table or an arena) and share them between status, lists and the tree (`gk-status/src/lib.rs:76` `Entry.path`, `views/file_status/tree.rs`).

**Status engine**

- fsmonitor (Partial): when `core.fsmonitor` is on and the engine is gix, ask the daemon for changed paths before a full scan, or switch that repo to the CLI engine automatically. Show `fsmonitor_state()` in Optimize or Diagnostics (`gk-status/src/lib.rs:199`, `gk-status/src/fast.rs`).

**History engine**

- Commit-graph offer (Partial): when a repo with more than ~10k commits opens with no commit-graph, show a one-click "Write commit-graph" message (`session.rs` history_loop, `views/statusbar.rs`).
- Blame streaming (Partial): use `git blame --incremental` through `run_streaming` and fill lines as records arrive (`gk-core/src/advanced.rs:251`, `views/blame.rs:90`).

**Write operations**

- Discard backup (Missing): before discarding, run `git stash create` (plus copies of untracked files), record the commit under `refs/gitkwaken/discarded`, and add a "Restore discarded changes" action (`gk-core/src/ops.rs:126`, `dialogs/worktree.rs`).

**Stack**

- nucleo in the palette (Partial): replace the scorer in `views/palette.rs:200` with `nucleo-matcher`, reusing `file_status/tree.rs::Filter`.

**Tests**

- CI benchmarks (Partial): run `bench.yml` once, download the `bench-baseline` artifact and commit it as `bench/baseline-windows.json`, so the gate becomes active.
- Client comparison (Partial): run `compare-clients.ps1 -Fork <path>`, measure SourceTree and GitKraken by hand, and add the run to a release checklist (`docs/`, `.github/workflows/release.yml`).

**Performance budgets**

- First frame (Partial): add a single process-start-to-first-cached-frame measurement, for example a frame timestamp in the app logged when the cached snapshot first renders, read by `compare-clients.ps1` or a `gk-bench` case.
- 1M-row scroll (Partial): record a frame-time histogram (`crates/gk-dx11` stats) during a scripted scroll over `bench-data/linux` (`scripts/e2e/`).
- Stage 100k (Partial): the 2 s budget looks unreachable through git on NTFS with Defender (8-11 s, bound by object writes). Either write blobs in-process with gix in parallel, or document the budget as hardware-bound.
- Filter 1M (Partial): cache `Utf32String` per status generation and match in parallel with rayon, or narrow incrementally from the previous result set (`views/file_status/list.rs:166`, `tree.rs::Filter`).
- 50k-line diff (Partial): add an app-side open-to-render measurement (diff, `diff/model.rs` layout and first paint) to `gk-bench diff` or an E2E timing.
- Memory (Partial): about 42 MB of shared driver/DLL pages block the small-repo budget. Trim the syntect set to common syntaxes (about 8 MB), and reduce the Linux peak (1.17 GB) in the history layout and cache.

Follow-ups on Done rows that are not gaps: record the gix-instead-of-`cat-file` deviation in `docs/ARCHITECTURE.md`; have Optimize offer `core.longpaths=true` on Windows (`gk-core/src/advanced.rs:614`); add integration tests for signed tags, history filter arguments run through `git log`, and the notes round trip; add a rename similarity threshold.
