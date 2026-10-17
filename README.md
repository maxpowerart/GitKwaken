<p align="center"><img src="assets/icon-256.png" width="128" alt="GitKwaken logo"></p>

# GitKwaken

A fast, lightweight, SourceTree-style git client for Windows, written in Rust.

GitKwaken gives you the familiar SourceTree workflow (commit graph, staging, branches, stashes,
remotes, tabs) but starts instantly and stays smooth on huge repositories. The UI is
[egui](https://github.com/emilk/egui) rendered directly with Direct3D 11; all git work happens on
background threads, so the UI never blocks on the repository.

> Status: pre-1.0. Windows is the primary target; Linux and macOS build and run through an
> `eframe` fallback renderer.

## Features

SourceTree parity, roughly grouped the way SourceTree does it:

- **Repositories & tabs**: open / clone / create repositories, multiple repos in tabs, bookmarks,
  welcome page with recent repositories, "Open in GitKwaken" folder context menu (installer).
- **File status**: staged / unstaged lists (flat or tree), stage / unstage / discard whole files,
  hunks or individual lines, commit with amend, sign-off and GPG signing, commit-message history,
  apply / create patch.
- **History**: commit graph with lanes for all branches, filter by branch, search by message,
  author, hash or path, commit details, file history, blame.
- **Diffs**: side-by-side and unified, intra-line (word) highlighting, whitespace options, image
  and binary detection.
- **Branches & tags**: create, checkout (incl. remote tracking), rename, delete local / remote,
  set upstream, merge, rebase, interactive rebase (reorder / squash / fixup / reword / drop),
  cherry-pick, revert, reset (soft / mixed / hard), tags (create, push, delete).
- **Remotes**: fetch, pull (merge or rebase), push (incl. tags, force-with-lease), add / edit /
  remove remotes, credential and SSH passphrase prompts handled in the GUI (askpass).
- **Stashes**: stash (incl. untracked / keep index), apply, pop, drop, branch from stash.
- **Advanced**: submodules, worktrees, git-flow, LFS, bisect, sparse checkout, clean, repository
  optimization, archive export, built-in git console with a log of every executed command,
  command palette.
- **Layout**: SourceTree's window: menu bar (File, Edit, View, Repository, Actions, Tools, Help),
  repository tabs, large-icon toolbar, sidebar with WORKSPACE (File Status, History, Search)
  above branches, tags, remotes, stashes and submodules.
- **App**: dark / light themes, persistent window and per-repository layout, self-updater with
  SHA-256-verified downloads, per-user installer or portable zip.

## Performance

Measured on a Ryzen-class desktop, Windows 11, NVMe SSD (see `crates/gk-bench`):

| Scenario | Result |
|---|---|
| Working-tree status, 200k-file repository (git CLI engine) | **75 ms** |
| History build, git.git (85k commits, with commit-graph) | **95 ms** |
| Graph layout memory, git.git | **1.7 MB** |
| DX11 renderer, 1M-row list, vsync | **117 FPS** (display-limited) |
| DX11 renderer, 1M-row list, unlocked | **> 900 FPS** |

The UI thread never runs git: it renders immutable snapshots published by background threads and
only materializes visible rows, so list and graph cost is independent of repository size.

## Installing

Grab a release from the Releases page:

- `GitKwaken-<ver>-x64-setup.exe`: per-user installer (no admin rights), Start-menu entry,
  optional desktop icon and "Open in GitKwaken" folder context menu; upgrades in place.
- `GitKwaken-<ver>-x64.msi`: the same per-user install as an MSI (for Intune/GPO-style
  deployment or `msiexec /i GitKwaken-<ver>-x64.msi /qn`); installs to
  `%LOCALAPPDATA%\Programs\GitKwaken`, newer MSIs upgrade it (one Add/Remove Programs entry).
  Features: `ContextMenu` (on by default) and `DesktopShortcut` (off), e.g.
  `msiexec /i ... ADDLOCAL=Main,DesktopShortcut`. Use either the MSI or the setup.exe, not
  both (they share the folder).
- `GitKwaken-<ver>-x64.zip`: portable build; unzip anywhere and run `gitkwaken.exe`.

The built-in updater replaces `gitkwaken.exe` in place, which works for the zip and the
per-user installs (MSI, default setup.exe); an all-users setup.exe install under Program Files
is not writable for it. Add/Remove Programs keeps showing the installed version until the next
installer run.

Requirements: Windows 10 1809+ x64 and [Git for Windows](https://git-scm.com/download/win)
(GitKwaken drives the system `git`; a custom path can be set in Settings).

## Building

```sh
# Rust stable (edition 2024), MSVC toolchain on Windows
cargo build --release -p gk-app        # -> target/release/gitkwaken.exe
cargo run -p gk-app -- path/to/repo    # open a repository directly
cargo test --workspace
```

Linux needs the usual eframe/GTK development packages:

```sh
sudo apt-get install libxkbcommon-dev libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev \
                     libxcb-xfixes0-dev libssl-dev
```

Release packaging (zip, `update.json`, the Inno Setup installer if Inno Setup 6 is installed,
and the MSI if the WiX CLI is installed):

```powershell
dotnet tool install --global wix --version 6.0.2   # once, for the MSI (packaging/wix/gitkwaken.wxs)
powershell -ExecutionPolicy Bypass -File packaging\build-release.ps1
```

[cargo-dist](https://github.com/axodotdev/cargo-dist) 0.33.0 is configured in
`dist-workspace.toml` as an alternative build of the zip: `dist plan`,
`dist build --artifacts=local` (output in `target/distrib`). The release workflow does not use
it; see `docs/ARCHITECTURE.md` (Packaging).

The app icon is generated from `assets/icon.svg` with
`cargo run --release --manifest-path packaging/icongen/Cargo.toml`.

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Open repository |
| `Ctrl+W` | Close tab |
| `Ctrl+Tab` | Next tab |
| `F5` | Refresh |
| `Ctrl+1` / `Ctrl+2` / `Ctrl+3` | File status / History / Search view |
| `Ctrl+Enter` | Commit (in the commit message box) |

## Architecture

A Cargo workspace; the GUI crate is thin and everything git-related is testable without a window.

| Crate | Role |
|---|---|
| `gk-cli` | Runs the system `git`: blocking and streaming calls, progress parsing, cancellation (job objects on Windows), global command log. |
| `gk-status` | Working-tree status model, two engines (in-process gitoxide and git CLI), incremental merging, debounced file-system watcher. |
| `gk-graph` | Commit-graph layout: date-ordered topological sort and lane assignment on dense `u32` indices. |
| `gk-diff` | Display diffs, intra-line highlighting, partial patches and line-level stage / unstage / discard. |
| `gk-core` | Repository model and orchestration: refs, history, diffs, operations, and the background `Session` the UI talks to. |
| `gk-settings` | Persistent app settings and per-repository UI state (TOML in `%APPDATA%\GitKwaken`). |
| `gk-askpass` | Credential / passphrase / host-key prompts from git and ssh routed to the GUI over loopback. |
| `gk-dx11` | Runs an egui app in a native window rendered with Direct3D 11 (eframe fallback elsewhere). |
| `gk-update` | Self-update: channel manifest check, SHA-256-verified download, Windows exe self-replace. |
| `gk-app` | The `gitkwaken` executable: views, dialogs, tabs. |
| `gk-bench` | Benchmarks for status, history and graph on real repositories. |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the threading model.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.
