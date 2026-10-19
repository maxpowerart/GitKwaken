# Changelog

Each release's section is shown in GitKwaken's "update available" dialog and on its GitHub
release page (the release workflow copies the `## <version>` section). Keep entries short and
user-facing.

## 1.0.4

- New look, "Kwak": every icon is drawn by GitKwaken in one style taken from the logo (teal
  line icons with orange commit nodes), with the logo's navy title bar and teal selection.
  Icons look the same on every system and at every font size.
- Styles: Settings › General › Style switches between Kwak and Classic (the previous blue
  look). Your own styles are small TOML files in the config folder's `styles` directory;
  "New style from this one" starts one, and saving the file applies it immediately.
- Custom icons: a style can replace any icon with an SVG file (`[icons.files]`); "Export
  icons" writes all of them as SVGs to start from. Lucide/Feather-style icons work as they are.
- File status badges redrawn: filled squares in the status color with a centered letter,
  dark in the dark theme and white in the light one.
- Install another version (Help menu): pick any release, read its changelog and install it,
  older ones included, to go back after an update that went wrong.
- Withdrawn releases: when a release is pulled, installations running it offer to roll back
  to the last good version, with the reason. The update dialog can also skip a version.
- Pinned branches can be reordered by dragging them within PINNED.
- Settings: choose whether staged or unstaged files are listed on top in File Status.
- File Status: no check boxes (stage the selection with a double click, Space, the header
  buttons, drag and drop or the context menu); new untracked files (?) are green like added
  ones (A).
- Tabs of repositories with the same folder name show the part of the path that differs.
- Font size: toolbar, tabs, sidebar icons, file status badges and history
  rows now grow with it; the status letters no longer spill out of their boxes. Code font
  size (diffs) is in Settings and follows Ctrl+mouse wheel over a diff.
- Panel sizes (file list, commit box, sidebar, output log) survive minimizing and smaller
  windows.
- The sidebar scroll bar no longer covers the pin / filter buttons.
- Linux: icons render again (they no longer depend on the installed fonts; the Arch package
  also depends on `ttf-dejavu` for the remaining symbols in text).
- Window: the menu bar and tabs are one band without a dividing line.

## 1.0.3

- Update dialog: the update button now shows what's new in the release and asks before
  installing. "Not now" keeps the button for later.
- Pinned branches: pin a branch with the pin button next to it (or its context menu) to keep
  it in a PINNED list above the local branches.
- Branch filter buttons in the sidebar: the funnel next to a local or remote branch adds it to
  the history filter (the "Show branches" list of "All branches ▾"); both stay in sync.
- The history branch filter is remembered per repository across restarts.
- Windows: the menu bar is the window's title bar, with its own minimize, maximize and close
  buttons (drag it to move the window, double-click to maximize). Set
  `GITKWAKEN_SYSTEM_FRAME=1` to get the system title bar back.
- Menus: with a menu open, pointing at another one in the menu bar opens it.

## 1.0.2

- Linux: GitKwaken updates itself. A pacman install downloads the release's package and
  installs it with `pkexec pacman -U`; a tarball install replaces its executable.
- Updates are checked at startup and every 6 hours.
- Releases include a ready-made Arch package (`gitkwaken-bin-<version>-1-x86_64.pkg.tar.zst`).

## 1.0.1

- First Linux x86_64 release: tarball and PKGBUILD for Arch / Manjaro.
- New logo and icons.

## 1.0.0

- First release: SourceTree-style git client for Windows.
