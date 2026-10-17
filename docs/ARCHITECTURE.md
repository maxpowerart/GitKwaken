# GitKwaken architecture

GitKwaken's one rule: **the UI thread never touches the repository.** It renders immutable
snapshots that background threads publish, and it asks for anything expensive (diffs, commit
details, searches) through non-blocking loaders that answer `Loading` until the result is ready.

## Crates

```
gk-app ──► gk-core ──► gk-status ──► gk-cli
  │           │  └────► gk-graph
  │           └───────► gk-diff
  ├──► gk-dx11      (window + D3D11 renderer; eframe fallback off Windows)
  ├──► gk-settings  (TOML settings + per-repo UI state)
  ├──► gk-askpass   (credential prompts from git/ssh into the GUI)
  └──► gk-update    (self-update)
gk-bench ──► gk-core / gk-status / gk-graph   (measurements on real repos)
```

| Crate | Responsibility |
|---|---|
| `gk-cli` | Spawns the system `git` (blocking or line-streaming), parses progress, cancellation via Windows job objects, global log of executed commands (git console). |
| `gk-status` | Status data model, gitoxide and git-CLI engines, incremental merge of partial results, file-system watcher. |
| `gk-graph` | Topological, date-ordered sort and lane assignment on dense `u32` commit indices (git.git: 1.7 MB). |
| `gk-diff` | Display diffs, word-level highlighting, partial patches, in-process line staging. |
| `gk-core` | `Repo` (refs, history, ops, advanced features) and `Session` (threads + snapshots). |
| `gk-dx11` | Event-driven winit loop, Direct3D 11 flip-model swap chain, WARP fallback, device-lost recovery. |
| `gk-update` | Update manifest check, SHA-256-verified download, exe self-replace. |
| `gk-app` | `gitkwaken.exe`: tabs, views, dialogs. Also the askpass helper when launched by git. |

### Deviations from the design document

- **Object reads in process, not a long-lived `git cat-file --batch`.** Blobs, trees and
  commits are read through gitoxide (`gix`, one handle per thread). It needs no pipe protocol
  or child process to keep alive, reads packs and the commit-graph directly, and was faster
  in every measurement. The git CLI stays the tool for writes, network and anything with
  filters git must run (LFS, `ident`, `working-tree-encoding`).
- **Status engine.** The default full scan is `git status --porcelain=v2` (faster on Windows
  with `core.fscache`, and it talks to the fsmonitor daemon); the gitoxide engine is an
  option. The watcher fast path always works in process on the gix index.
- **UI toolkit.** egui on a custom Direct3D 11 host (`gk-dx11`) instead of GPUI.
- **Packaging.** Inno Setup installer, zip and winget manifests instead of cargo-dist / MSI.

## Threads

One `Session` per open repository tab. Per session:

```
            file system                       user actions (dialogs, buttons)
                 │                                         │
          ┌──────▼──────┐  watch::Event       ┌────────────▼───────────┐
          │  gk-watch   │────────────┐        │        gk-ops          │
          │ (gk-status) │            │        │ serialized op queue:   │
          └─────────────┘     ┌──────▼─────┐  │ commit, fetch, push,   │
                              │ gk-events  │  │ rebase... one at a     │
                              │ → Refresh  │  │ time, cancellable,     │
                              └──────┬─────┘  │ streams last line      │
                                     │        └───────┬──────┬─────────┘
                              ┌──────▼──────┐         │      │ refresh after op
                              │ gk-refresh  │◄────────┼──────┘
                              │ status/refs │         │
                              │ /history    │         │
                              └──────┬──────┘         │
                                     │ write          │ write (op progress, messages)
                              ┌──────▼────────────────▼──┐
                              │  RwLock<Snapshot>        │  Arc<Status>, Arc<Refs>,
                              │  (generation counter)    │  Arc<History>, op, messages
                              └──────────────┬───────────┘
                                             │ clone (cheap: Arcs)
       rayon pool ──────────────┐     ┌──────▼──────┐
       diffs, commit details,   │────►│  UI thread  │ egui frame: reads snapshot,
       commit metas, search     │     │  (gk-dx11)  │ lays out visible rows only
       (results into Load<T>    │◄────│             │ requests Load<T> values
        slots + repaint)        │     └─────────────┘
```

- **UI thread** (`gk-dx11` event loop, egui). Each frame calls `Session::snapshot()`, which clones
  a struct of `Arc`s under a read lock, so it never waits on git. Lists and the graph only build
  the rows that are on screen. The loop is event driven: an idle window sleeps until input or a
  `request_repaint` from a worker.
- **`gk-refresh`** drains and coalesces `Refresh` requests (full status, specific worktree paths,
  refs, history), runs them, and swaps new `Arc`s into the snapshot. A newer full-status request
  interrupts one that is still running.
- **`gk-ops`** runs mutating operations strictly one at a time (`run_op` / `run_index_op`), with a
  `CancelToken`, progress (`last_line`) published into the snapshot, and conflict detection.
  Finishing an op schedules the refreshes it needs.
- **`gk-watch`** (in `gk-status`) turns raw OS notifications into debounced events: worktree paths
  (re-check only those), `.git` changes (index / refs / HEAD / state), or `Overflow` when a burst is
  too large (one full refresh). **`gk-events`** maps those events to `Refresh` requests.
- **rayon pool**: lazily computed values: file diffs (keyed by target + `status_gen` + options),
  commit details, history search, and batches of commit metadata for visible graph rows. Each
  `Load<T>` slot holds a ticket, so a stale result (user already moved on) is dropped; completion
  calls the repaint callback.
- **`gk-syntax`** (per shown diff, `views/diff/syntax.rs`): syntax highlighting of the visible
  lines only. The diff body publishes its visible line range (± 200 lines) each frame; the worker
  highlights the 128-line windows of the hunks it intersects (visible ones first), resuming from
  parser checkpoints saved at window boundaries, and hands spans back in batches that the UI merges
  (repaint per batch). A jump far into a huge hunk first shows provisional colors (parse from a
  fresh state) while the exact forward parse catches up; the forward parse yields to a newer
  scroll at each window. The worker exits when the wanted range is done and is restarted on demand;
  a replaced diff stops it. `gk-syntax-warm` loads the syntax of a file type while its diff loads.

App-wide (not per session):

- **askpass** (`gk-askpass`): a loopback TCP server (`askpass-accept`, one `askpass-conn` per
  prompt). git/ssh launch `gitkwaken.exe` as `GIT_ASKPASS`/`SSH_ASKPASS`; that process forwards the
  prompt to the GUI, which shows a dialog and returns the answer.
- **update check** (`gk-update`): one short-lived background thread at startup (`check`), and
  `download_and_stage` when the user accepts; `apply_on_exit` runs after the window closes.

## Data flow of a typical action

1. User clicks *Stage* on a hunk. The view builds a partial patch with `gk-diff` and calls
   `session.run_index_op(...)`.
2. `gk-ops` applies it, then requests a status refresh for the touched paths.
3. `gk-refresh` recomputes just those paths, merges them into a new `Arc<Status>`, bumps
   `status_gen` / `generation`, and calls repaint.
4. The next UI frame sees the new snapshot; the diff for the selected file has a new cache key
   (`status_gen`), so it is recomputed on the rayon pool and shown as soon as it lands.

## Rendering

`gk-dx11` hosts egui on a winit window with a Direct3D 11 flip-model swap chain (legacy blit model
as fallback, WARP if no hardware device, full re-creation on device loss). Measured: 117 FPS with
vsync (display-limited) and > 900 FPS unlocked on a 1M-row list. Off Windows the same `Options` /
`run` API is implemented on top of `eframe`.

## Self-update

`update.json` (published with every GitHub release by `packaging/build-release.ps1`) holds
`version`, `notes`, `url` and `sha256` of the portable zip. The app compares semver, downloads
to `%TEMP%\gitkwaken-update`, verifies the hash, extracts `gitkwaken.exe`, and on exit renames the
running exe to `gitkwaken.exe.old` (allowed while running), moves the new one in, and deletes
`.old` on the next start (`cleanup_old`).
