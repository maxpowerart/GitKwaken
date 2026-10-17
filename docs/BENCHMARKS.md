# GitKwaken benchmarks

Measurement date: 2026-10-17. Machine: the development workstation (Windows 11
Pro 10.0.26300, NVMe SSD, git 2.45.1.windows.1), release builds
(`cargo build --release -p gk-bench`). Times are medians of 5 runs unless noted.
All numbers can be reproduced with the commands next to them.

## Corpus

| Repository | Origin | Size | Commits | Files | How |
|---|---|---|---:|---:|---|
| `bench-data/linux` | Linux kernel, blobless partial clone, no checkout | 2.2 GB `.git` | 1,485,415 | ~95k at HEAD | `scripts/fetch-corpus.sh linux`: clone 521 s, `commit-graph write --reachable --changed-paths` 72 s |
| `bench-data/gitgit` | git.git | | 85,618 | 4,857 | full clone |
| `bench-data/r1m`, `r200k` | synthetic (`gk-bench gen <dir> <files>`) | | 1 | 1,000,000 / 200,000 | 100 files per leaf directory |
| `target/bench-regress/status-50000` | synthetic (`gk-bench regress` generates it) | | 1 | 50,000 | ~20 s to generate |
| `target/bench-regress/dag-50000` | synthetic DAG via `git fast-import` (`gk-bench gen-dag <dir> 50000`): main line, 6 topic branches merged every 40 commits, a tag every 2,000 commits, commit-graph | | 50,000 | ~2,000 | 4.4 s |
| `bench-data/lfs-test` | `scripts/fetch-corpus.sh lfs`: 200 x 64 KiB LFS binaries, two commits (needs git-lfs; skipped with a message otherwise) | 13 MB LFS objects | 2 | 202 | 8.4 s |
| `bench-data/chromium` | optional: `scripts/fetch-corpus.sh chromium` (blobless, no checkout) | ~10-15 GB of commits and trees | ~1.4M | ~450k | 1-3 h, not fetched for this report |

`scripts/fetch-corpus.sh` is idempotent: re-running fetches updates and rewrites the commit-graph.

## Results against the performance budgets

| Budget | Measurement | Result | Status |
|---|---|---|---|
| Full status warm < 1.5 s at 1M files | `gk-bench status bench-data/r1m` | CLI engine 396-415 ms, gix engine 898 ms, `git status` 407 ms | met |
| Status after save < 50 ms | fast path, 10 paths, index cached | ~0.2 ms (50k files: 0.17-0.24 ms) | met: save-to-status end to end 22 ms (adaptive 15 ms watcher debounce + fast path) |
| History first screen < 100 ms | `gk-bench history bench-data/gitgit` (85k commits) | first rows 19.5 ms (early history, no cache), 29 ms from the warm snapshot cache; full build ~97 ms runs after the first screen | met |
| First frame < 300 ms from cache | `scripts/measure-startup.ps1 target/…/gitkwaken.exe bench-data/gitgit 7`: process creation → first frame drawing history rows from the snapshot cache (`GITKWAKEN_STARTUP_LOG`) | median **218 ms** (205-280 ms, 7 runs); first frame of any kind 175-231 ms; cold start without cache: rows at 306 ms | met |
| Memory < 60 MB small repo / < 400 MB reference (1M files, 1M commits) | `scripts/measure-memory.ps1 <exe> <repo> -View <view> -SettleMs 20000` (median of 3); `-Warp` forces WARP to separate the GPU driver | private working set (Task Manager "Memory"): demo **23 MB**, git.git 36 MB, Linux **152 MB**, r1m (1M files, clean) under 64 MB working set. Working set: 62 / 87 / 311 MB (includes ~38 MB of shared DLL and D3D11-driver pages, and memory-mapped packs). Private bytes 69 / 84 / 224 MB; with WARP the demo's private bytes are 31 MB, so ~38 MB is the GPU driver's commit. `D3D11_CREATE_DEVICE_SINGLETHREADED` saved 3.5 MB. Cold first open of Linux peaks at ~500 MB while the history is built | met (Task Manager figure) |
| Full status, gix engine (optional; default is CLI) with `core.fsmonitor` | `gk-bench status <200k-file repo> 5` without / with `core.fsmonitor=true` + `core.untrackedCache=true` | gix engine full scan 171 ms in-process → 73-75 ms (handed to `git status`, which asks the daemon); `git status` itself 68 → 73 ms (fscache already makes it fast on this tree) | |
| Status snapshot memory (path interning) | `gk-bench status-mem <r200k with .git/index moved away> 1000000`: 200k staged deletions + 200k untracked; synthetic 1M entries built from those paths. Before: one `BString` per path; after: `PathStr` spans of one shared buffer per scan (`gk-status/src/path.rs`) | `size_of::<Entry>()` 56 → 40 bytes. 400k-entry scan retains 47.1 MB in 400k blocks → 27.4 MB in 4 blocks (-42 %). 1M entries: 103.4 MB → 71.4 MB (-31 %; heap bytes plus ~16 B allocator overhead per block). gix full scan median 381-448 → 300 ms; CLI engine unchanged (~650 ms, dominated by git). 1M-entry status: build 129-159 → 43-53 ms, clone 80-84 → 9-10 ms, drop 27-37 → 6 ms, sort + dedup 48-58 → 16 ms | |
| History, 1.48M commits (linux) | `gk-bench history bench-data/linux`; `gk-bench history-first bench-data/linux 3` | first screen (2,000 rows, early partial history): **20.9 ms** median (cold first run 1.39 s while the packs are paged in); full build 1.89 s in the background. Older figures: refs::load 45.6 ms, full build 2,481 ms, 577 lanes max width, layout 36 MB, search "fix" over all commits 492 ms | first screen met; the full graph follows in ~1.9 s |
| 1M-row scroll at 120 fps, no frame > 16 ms | app on `bench-data/linux` (1,485,415 commits), History view, `GITKWAKEN_AUTOSCROLL=40` (40 rows per frame), `GK_DX11_FRAME_STATS`, 30 s, 120 Hz vsync | after the first 300 frames (startup): 120 fps in every 300-frame window, worst frame interval 9.7-10.3 ms, UI build 0.4-0.9 ms average / 2.3 ms worst | met |
| 1M-row lane layout | `gk-bench regress` `layout_1m_rows` | 15-17 ms | |
| Stage 100k files < 2 s | `gk-bench stage target/bench/r100k 100000` (100k-file synthetic repo, `gk-bench gen`) | In-process path (`gk-core/src/stage.rs`, 3 runs): `Repo::stage` 100k modified files (1,000 distinct contents) **0.55-0.62 s**; 100k files with all-new blobs (one pack written) **0.66-0.69 s**; `git diff --name-only` right after 38-60 ms (stat data valid); `Repo::unstage` 0.31 s; `unstage_all` (`git reset -q`) 6.8-7.1 s. Previous git path (`GITKWAKEN_GIT_STAGE=1`, `update-index --add --remove -z --stdin`): 2.3-21 s for 1,000 distinct contents (7.4 s of it is git reading and hashing 100k files on one thread: `git hash-object --stdin-paths`), and **546-640 s** for 100k new blobs (one loose object file per blob, NTFS with Defender). Earlier audit (one `git add --pathspec-from-file`): 152 s | **met** (~3x margin). `unstage_all` is not part of the budget but still slow. |
| Filter 1M paths < 50 ms per keystroke | `gk-bench filter 1000000` (nucleo-matcher 0.3, `Config::DEFAULT.match_paths()`, pre-converted `Utf32String`s) | 1 thread: 28-120 ms (54 ms for "s", 120 ms for "src", 63 ms for "core/main"); rayon on all cores: 21-31 ms; from `&str` without cached UTF-32: 81 ms; plain substring: 3 ms | met only when matching in parallel (rayon) or incrementally on the previous result set |
| Open a 50k-line diff < 100 ms | app on `bench-data/diff50k` (50k-line file, 1,515 scattered edits, 12,120 diff lines), `GITKWAKEN_STARTUP_LOG` `diff_open`: click in File Status to the frame with the diff drawn; `gk-bench diff-open bench-data/diff50k big.rs` for the parts | click → drawn **26.8-27.4 ms** (5 runs); right at startup (with status and history loading in parallel) 89-102 ms. Parts: both sides read 4.9 ms, diff 5.2 ms (was 95 ms: Histogram on scattered edits + quadratic hunk positions; now Myers above 20k lines). `gk-bench diff 50000`: 5.0 ms (was 19.8), whole file 6.0 ms. Syntax highlighting (visible lines only, `views/diff/syntax.rs`), release build, repo with a 50k-line `.rs` file edited every 33rd line (12,120 diff lines) and a new 50k-line `.rs` file (one 50,000-line hunk), `GITKWAKEN_STARTUP_LOG` `diff_open` / `diff_highlighted` from the click: new file drawn 50-62 ms, highlighted 65-84 ms; edited file drawn 53-77 ms, highlighted 67-93 ms (before: no highlighting above 20k lines, and the whole diff on one thread: 4.2 s for the 50k-line file). End key on the new file: provisional colors on the next frames (< 100 ms), exact ones after the forward parse (~2.5 s of background parsing, 18k lines/s). Wheel-scrolling 300 px per frame through both diffs (`GK_DX11_FRAME_STATS`): 117-120 fps per 300-frame window, average UI build 0.65-0.79 ms, worst UI build 1.2-4.0 ms (one 11 ms outlier), colors present on every screenshot | met |
| File history < 1 s for the first 50 | `gk-bench file-history bench-data/gitgit …` (`git log --follow -n 50 --format=%H -- <path>`) | Makefile 48 ms, diff.c 79 ms, builtin/log.c 113 ms, Documentation/git.txt 244 ms; whole history with `--follow`: 484-786 ms; without `--follow` first 50: 25-33 ms | met: File History runs `-n 50` first and shows it while the full `--follow` walk (up to ~0.8 s on git.git) finishes |

Earlier numbers kept from the audit (`docs/PLAN-COVERAGE.md`): git.git history
refs::load 40.3 ms, history::build 97.2 ms, layout 1.7 MB, search "fix" 75.6 ms;
r1m status `git status` 407 ms, gix 898 ms, CLI engine 396 ms, fast path 0.2 ms
(110.8 ms when the index has to be loaded); peak working set after opening
History: demo 107 MB, git.git 133 MB, r1m 368 MB.

## Regression gate (`gk-bench regress`)

`gk-bench regress` generates (once, cached under `--data`, default
`target/bench-regress`) a 50k-file repository and a 50k-commit DAG, then measures:

| Metric | What | Local result (median of 5) |
|---|---|---:|
| `status_full_cli_50k` | `gk_status::compute`, CLI engine, `-uall`, 10 modified files | 34-39 ms |
| `status_full_gix_50k` | same, gix engine | 38-55 ms |
| `status_fast_10_paths` | `FastStatus::refresh` of the 10 paths, index cached | 0.17-0.24 ms |
| `history_build_50k` | `refs::load` + `resolve_tips` + `history::build` of the DAG | 46-50 ms |
| `diff_50k_lines` | `gk_diff::diff` of a 50k-line file, ~3% edited, 3 lines context | 18-20 ms (5 ms since Myers above 20k lines) |
| `diff_50k_lines_full_context` | same, whole file | 10.7-10.9 ms |
| `layout_1m_rows` | `gk_graph::Layout::push` of 1M synthetic rows (merge every 10) | 15-17 ms |

Each metric is the median of `--runs` (default 5) runs after one warm-up. It
**fails** when it is more than 10% slower than the baseline median **and** more
than 2 ms slower in absolute terms (`--threshold`, `--floor-ms`), for both the
median and the fastest run, so timer noise and one-off stalls on a shared
runner cannot flap the gate. A run prints a Markdown
table (also appended to the GitHub job summary) and exits 1 on a regression.
Two consecutive local runs against a fresh baseline stayed within -30%..+8%.

Baseline in CI: the `Benchmarks` workflow (`.github/workflows/bench.yml`,
windows-latest) builds `gk-bench` twice, from the base commit (the PR's merge
base, or the previous commit on a push to main) and from the current commit,
and runs both on the same runner, each on its own generated data. The base
run records `target/bench-results/base.json`; the current run compares against
it and fails the job on a regression. Same runner, same minute: no committed
baseline is needed, and runner hardware differences do not matter.

- **Committed baseline** (optional): `gk-bench regress --record` writes
  `bench/baseline-<os>.json` for the machine it runs on; useful locally to
  compare a working tree against an earlier state on the same workstation. Run
  the workflow manually with `use_committed: true` to compare against a
  committed `bench/baseline-windows.json` instead of the base commit.
- Changing what a metric measures: bump `SCHEMA` in `crates/gk-bench/src/regress.rs`;
  an old-schema baseline is then re-recorded instead of compared (the A/B job
  then passes once and gates again from the next commit).

Shared CI runners are noisy; if the gate flaps, raise `--runs` first, then
`--threshold`.

## Comparison with other clients

`scripts/compare-clients.ps1` launches each client with the repository path as
argument inside a Job Object and samples the CPU time of the whole process
tree (including git.exe children) every ~100 ms for 20 s:

- **window**: process start until the main window has a title;
- **loaded**: until 97% of the startup CPU work is done, where startup work is
  the CPU time above the steady background rate of the last 3 s;
- **startup CPU**: that work in CPU-milliseconds (all cores);
- **idle CPU**: the steady background rate after loading (100% = one core).

A window-only screenshot (PrintWindow) per run in `bench-data/clients/shots/`
confirms the repository was actually shown. Median of 3 runs on an otherwise
idle machine, 2026-10-17 (raw: `bench-data/clients/compare-1.0-final.json`):
GitKwaken 1.0.0 from `dist/` (opens in the History view), SourceTree 3.4.24
(the workstation's own install; its open-tab list was emptied for the runs and
restored byte for byte afterwards), Fork 2.21.0 (winget `Fork.Fork`, per-user),
SourceGit 2026.21 portable. GitKraken was left out by the user's decision (it
also needs an account sign-in before it opens a repository). git.git = 85,618
commits / 4,857 files; r200k = synthetic, 1 commit / 200,000 files, clean
working tree (`gk-bench gen`).

| Repository | Client | Window | Loaded | Startup CPU | Idle CPU |
|---|---|---:|---:|---:|---:|
| git.git | GitKwaken | 232 ms | 1,886 ms | 2,922 ms | 0% |
| git.git | SourceTree | 122 ms | 3,118 ms | 4,425 ms | 0.5% |
| git.git | Fork | 1,135 ms | 4,022 ms | 1,719 ms | 0% |
| git.git | SourceGit | 364 ms | 920 ms | 2,212 ms | 1.6% |
| git.git | `git status` | | 35 ms | | |
| git.git | `git log --graph --all --format='%h %s'` | | 847 ms (first 200: 76 ms) | | |
| r200k | GitKwaken | 238 ms | 553 ms | 1,219 ms | 0% |
| r200k | SourceTree | 116 ms | 2,551 ms | 4,696 ms | 2.1% |
| r200k | Fork | 1,163 ms | 4,072 ms | 2,828 ms | 0% |
| r200k | SourceGit | 341 ms | 451 ms | 1,062 ms | 3.2% |
| r200k | `git status` | | 63 ms | | |

Reading the numbers:

- Against the two GUI clients the design targets, GitKwaken is fully loaded
  first on both repositories: git.git 1.9 s vs SourceTree 3.1 s and Fork 4.0 s;
  200k files 0.55 s vs 2.6 s and 4.1 s. SourceTree shows its (empty) shell
  window first, Fork after ~1.1 s.
- SourceGit finishes git.git sooner (0.9 s) because it loads one page of
  commits; GitKwaken lays out the whole 85k-commit graph (first rows at ~20 ms,
  see the budgets table). On r200k the two are close (451 vs 553 ms).
- GitKwaken and Fork go fully idle; SourceTree and SourceGit keep 0.5-3% of a
  core busy.
- An earlier run the same day (while a build ran in parallel) showed SourceTree
  at ~100% idle CPU and SourceGit at 6.5 s on r200k; those numbers were noise
  from the busy machine and are superseded by this table.

Re-run: `powershell -ExecutionPolicy Bypass -File scripts\compare-clients.ps1 -FetchSourceGit -Repos bench-data\gitgit,bench-data\r200k -Runs 3 -OutFile compare.json`.
It starts real windows on the desktop; do not use the machine meanwhile.

## Other measurement commands

```text
gk-bench gen <dir> <files>               synthetic working tree
gk-bench gen-dag <dir> <commits>         synthetic history via git fast-import
gk-bench status <dir> [runs]             status engines vs git status
gk-bench status-mem <dir> [entries]      heap retained by a status snapshot (scan + synthetic N entries)
gk-bench history <dir>                   refs, history build, metadata, search
gk-bench objects <dir> [n]               object reads: gitoxide vs long-lived git cat-file
gk-bench regress [--record] [--runs N]   regression gate (see above)
gk-bench stage <repo> [files]            Repo::stage / unstage of N modified files
gk-bench discard <repo>                  discard of one file with and without the backup
gk-bench filter [paths]                  nucleo-matcher over N synthetic paths
gk-bench diff [lines]                    gk_diff on an N-line file
gk-bench file-history <repo> [paths]     git log --follow -n 50
scripts/compare-clients.ps1              GitKwaken vs SourceGit/Fork vs git CLI
scripts/fetch-corpus.sh [linux|chromium|lfs]
```

## Object reader: gitoxide vs `git cat-file --batch`

`gk-bench objects <dir> [n]` reads the first `n` objects of `git rev-list --objects --all`
with gitoxide and with the long-lived `git cat-file --batch-command` reader
(`gk_cli::CatFile`, the fallback and the `GITKWAKEN_OBJECT_READER=cli` path), checks that
both return the same bytes, and times `content::commit_details` for 300 commits with each
reader. Best of 3 passes, release build, Windows 11, 2026-10-17:

| Repository | Objects | gitoxide | cat-file, 1 process | process per object |
|---|---|---|---|---|
| `gen-dag` 20,000 commits (packed, 23.5 MB read) | 50,000 | 174 ms, 3.5 µs each | 823 ms, 16.5 µs each | 26 ms each |
| this repository (packed, 10.4 MB read) | 1,284 | 75 ms, 59 µs each | 193 ms, 150 µs each | 25 ms each |
| `gen` 20,000 files (loose objects) | 1,205 | 0.05 ms (object cache) | 102 ms, 85 µs each | 21 ms each |

`commit_details` (commit, parent, tree diff with rename detection), `gen-dag`: 506 µs per
commit through gitoxide, 711 µs through cat-file. gitoxide stays the primary reader
(4.7x faster on packed objects); the pipe protocol costs one round trip per object but is
~1,600x cheaper than a process per object.
