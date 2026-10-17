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
| Full status, default (gix) engine with `core.fsmonitor` | `gk-bench status <200k-file repo> 5` without / with `core.fsmonitor=true` + `core.untrackedCache=true` | gix engine full scan 171 ms in-process → 73-75 ms (handed to `git status`, which asks the daemon); `git status` itself 68 → 73 ms (fscache already makes it fast on this tree) | |
| Status snapshot memory (path interning) | `gk-bench status-mem <r200k with .git/index moved away> 1000000`: 200k staged deletions + 200k untracked; synthetic 1M entries built from those paths. Before: one `BString` per path; after: `PathStr` spans of one shared buffer per scan (`gk-status/src/path.rs`) | `size_of::<Entry>()` 56 → 40 bytes. 400k-entry scan retains 47.1 MB in 400k blocks → 27.4 MB in 4 blocks (-42 %). 1M entries: 103.4 MB → 71.4 MB (-31 %; heap bytes plus ~16 B allocator overhead per block). gix full scan median 381-448 → 300 ms; CLI engine unchanged (~650 ms, dominated by git). 1M-entry status: build 129-159 → 43-53 ms, clone 80-84 → 9-10 ms, drop 27-37 → 6 ms, sort + dedup 48-58 → 16 ms | |
| Status after save < 50 ms | fast path, 10 paths, index cached | ~0.2 ms (50k files: 0.17-0.24 ms) | met for the computation; the watcher debounce (60 ms) comes on top |
| History first screen < 100 ms | `gk-bench history bench-data/gitgit` (85k commits) | refs 40 ms + build 97 ms (~100-150 ms total) | over budget on git.git |
| History, 1.48M commits (linux) | `gk-bench history bench-data/linux` | refs::load 45.6 ms, history::build (walk + topo sort + lane layout) **2,481 ms**, 577 lanes max width, layout 36 MB, search "fix" over all commits 492 ms, peak working set 1,175 MB (whole bench process) | first screen far over budget until layout is chunked |
| 1M-row scroll at 120 fps | earlier manual measurement | 117 fps (vsync), > 900 fps unlocked on a 1M-row list; 120 fps scrolling git.git history | met (frame-time spikes not recorded) |
| 1M-row lane layout | `gk-bench regress` `layout_1m_rows` | 15-17 ms | |
| Stage 100k files < 2 s | `gk-bench stage <200k-file repo> 100000` | `Repo::stage` 100k modified files: **152 s** (2nd run 138 s); `Repo::unstage` 100k: **142 s**; `unstage_all`: 7.3 s. Cause: one `git add -A --pathspec-from-file` with 100k `:(literal)` pathspecs; git matches every index entry against every pathspec (20k pathspecs: `add` 12.5-15 s, `reset` 18.8 s). Same 100k files without pathspecs: `git update-index --add --remove -z --stdin` 10.6 s, `git add -A` 8.6 s, `git reset -q` 7.3 s | **not met**, by ~75x. Fix (gk-core, not done here): above ~1,000 paths stage with `update-index --add --remove -z --stdin` (or `add -A` when the selection is "everything"), unstage with `ls-tree -r -z HEAD` piped into `update-index -z --index-info`. Even then hashing 100k blobs into loose objects costs 8-11 s on this machine, so the 2 s budget looks unreachable through git on NTFS with Defender. |
| Filter 1M paths < 50 ms per keystroke | `gk-bench filter 1000000` (nucleo-matcher 0.3, `Config::DEFAULT.match_paths()`, pre-converted `Utf32String`s) | 1 thread: 28-120 ms (54 ms for "s", 120 ms for "src", 63 ms for "core/main"); rayon on all cores: 21-31 ms; from `&str` without cached UTF-32: 81 ms; plain substring: 3 ms | met only when matching in parallel (rayon) or incrementally on the previous result set |
| Open a 50k-line diff < 100 ms | `gk-bench diff 50000` (`gk_diff::diff`, ~3% of lines edited) | 3 lines context 19.8 ms (1,160 hunks), whole file 15.3 ms, new 50k-line file 6.1 ms, ignore whitespace 33.5 ms, `to_unified` 0.5 ms | met (diff computation; rendering not included) |
| File history < 1 s for the first 50 | `gk-bench file-history bench-data/gitgit …` (`git log --follow -n 50 --format=%H -- <path>`) | Makefile 48 ms, diff.c 79 ms, builtin/log.c 113 ms, Documentation/git.txt 244 ms; whole history with `--follow`: 484-786 ms; without `--follow` first 50: 25-33 ms | met for the first 50; the app currently waits for the whole `--follow` run (up to ~0.8 s on git.git) |

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
| `diff_50k_lines` | `gk_diff::diff` of a 50k-line file, ~3% edited, 3 lines context | 18-20 ms |
| `diff_50k_lines_full_context` | same, whole file | 10.7-10.9 ms |
| `layout_1m_rows` | `gk_graph::Layout::push` of 1M synthetic rows (merge every 10) | 15-17 ms |

Each metric is the median of `--runs` (default 5) runs after one warm-up. It
**fails** when it is more than 10% slower than the baseline median **and** more
than 2 ms slower in absolute terms (`--threshold`, `--floor-ms`), so
sub-millisecond metrics cannot flap on timer noise. A run prints a Markdown
table (also appended to the GitHub job summary) and exits 1 on a regression.
Two consecutive local runs against a fresh baseline stayed within -30%..+8%.

Baseline: `bench/baseline-<os>.json` (`bench/baseline-windows.json` in CI). It
must be recorded on the machine type that compares against it, so no baseline
from a workstation is committed:

- **First run**: the `Benchmarks` workflow (`.github/workflows/bench.yml`,
  windows-latest) finds no baseline, records one, passes, and uploads it as the
  `bench-baseline` artifact. Download it and commit it as
  `bench/baseline-windows.json`; from then on the job gates.
- **Update after an intended change** (faster code, new metric, runner image
  change): run the workflow manually with `record: true` (Actions > Benchmarks >
  Run workflow), download the `bench-baseline` artifact and commit it. Locally:
  `gk-bench regress --record` writes `bench/baseline-windows.json` for this machine.
- Changing what a metric measures: bump `SCHEMA` in `crates/gk-bench/src/regress.rs`;
  an old-schema baseline is then re-recorded instead of compared.

Shared CI runners are noisy; if the gate flaps, raise `--runs` first, then
`--threshold`.

## Comparison with other clients

`scripts/compare-clients.ps1` launches each client with the repository path as
argument inside a Job Object and samples the CPU time of the whole process
tree (including git.exe children) every ~100 ms for 15 s:

- **window**: process start until the main window has a title;
- **loaded**: until 97% of the startup CPU work is done, where startup work is
  the CPU time above the steady background rate of the last 3 s;
- **startup CPU**: that work in CPU-milliseconds (all cores);
- **idle CPU**: the steady background rate after loading (100% = one core).

A window-only screenshot (PrintWindow) per run in `bench-data/clients/shots/`
confirms the repository was actually shown. Median of 3 runs; GitKwaken release
build from 2026-10-17 (opens in the History view), SourceGit 2026.21 portable
(SHA-256 checked, run in portable mode). git.git = 85,618 commits / 4,857 files;
r200k = 1 commit / 200,000 files (private copy).

| Repository | Client | Window | Loaded | Startup CPU | Idle CPU |
|---|---|---:|---:|---:|---:|
| git.git | GitKwaken | 256 ms | 2,111 ms | 3,078 ms | 0% |
| git.git | SourceGit | 490 ms | 703 ms (see note) | 1,698 ms | 55.7% |
| git.git | `git status` | | 33 ms | | |
| git.git | `git log --graph --all --format='%h %s'` | | 942 ms (first 200: 86 ms) | | |
| r200k | GitKwaken | 225 ms | 553 ms | 1,469 ms | 0% |
| r200k | SourceGit | 367 ms | 10,353 ms (see note) | 2,122 ms | 53.4% |
| r200k | `git status` | | 158 ms | | |

Reading the numbers:

- GitKwaken shows its window about 2x sooner than SourceGit (225-256 ms vs
  367-490 ms) and goes fully idle (0% CPU) after loading; SourceGit keeps
  ~55% of a core busy while idle on this machine. That background load makes
  SourceGit's "loaded" figure unreliable (on r200k the 97% point drifts to the
  end of the window), so compare its startup CPU instead.
- On git.git GitKwaken spends ~3 s of CPU (2.1 s wall) after the window
  appears: it lays out the whole 85k-commit graph and loads the sidebar,
  status and details, while SourceGit only loads the first page of commits.
  This matches the "history first screen" gap above.
- **Fork** was not measured: winget offers it only as a Squirrel installer
  (`Fork.Fork` 2.21.0, installs and starts itself per user, commercial
  license), which this unattended run did not install on the workstation. The
  script measures it when `-Fork <path to Fork.exe>` is given (or
  `%LOCALAPPDATA%\Fork\Fork.exe` exists).
- **SourceTree, GitKraken**: not installed and not measured (installers with
  sign-in or account flows; not automatable silently). The git CLI rows are
  the reference instead.

Re-run: `powershell -ExecutionPolicy Bypass -File scripts\compare-clients.ps1 -FetchSourceGit -Repos bench-data\gitgit,bench-data\r200k -Runs 3 -OutFile compare.json`.
It starts real windows on the desktop; do not use the machine meanwhile.

## Other measurement commands

```text
gk-bench gen <dir> <files>               synthetic working tree
gk-bench gen-dag <dir> <commits>         synthetic history via git fast-import
gk-bench status <dir> [runs]             status engines vs git status
gk-bench status-mem <dir> [entries]      heap retained by a status snapshot (scan + synthetic N entries)
gk-bench history <dir>                   refs, history build, metadata, search
gk-bench regress [--record] [--runs N]   regression gate (see above)
gk-bench stage <repo> [files]            Repo::stage / unstage of N modified files
gk-bench filter [paths]                  nucleo-matcher over N synthetic paths
gk-bench diff [lines]                    gk_diff on an N-line file
gk-bench file-history <repo> [paths]     git log --follow -n 50
scripts/compare-clients.ps1              GitKwaken vs SourceGit/Fork vs git CLI
scripts/fetch-corpus.sh [linux|chromium|lfs]
```
