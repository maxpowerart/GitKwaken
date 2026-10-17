# Changelog

Each release's section is shown in GitKwaken's "update available" dialog and on its GitHub
release page (the release workflow copies the `## <version>` section). Keep entries short and
user-facing.

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
