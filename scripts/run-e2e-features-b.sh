#!/usr/bin/env bash
# End-to-end UI tests for the "features B" gaps: status filter chips, custom
# ignore patterns, commit options (template, author override, hook output),
# external diff tool, config editors (.gitignore, .git/config, global),
# keymap editor + rebound shortcut, background sparse clone, stage/unstage
# all shortcuts, Optimize diagnostics, Authentication diagnostics (GCM setup,
# connection test), bookmarking the open repository from "Repositories ▾",
# the large-history commit-graph banner, large-file / LFS / image overlay views,
# File Status from the keyboard (Shift+F10 menu, hunk staging, discard).
# Drives the real app through scripted input (GK_DX11_INPUT_SCRIPT) and
# checks repositories and settings files afterwards.
# Usage: scripts/run-e2e-features-b.sh <path to gitkwaken.exe> [scenario filter]
set -uo pipefail
EXE="$1"
FILTER="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench-data/e2e-features-b"
# Safety: git and the app must never climb out of the test area into a real
# repository (a missing test repo once made them reset the project repo).
export GIT_CEILING_DIRECTORIES="$(cygpath -m "$(dirname "$WORK")")"
mkdir -p "$(dirname "$WORK")"
if ! mkdir "$WORK.lock" 2>/dev/null; then echo "another run of this suite is active ($WORK.lock)"; exit 3; fi
trap 'rmdir "$WORK.lock" 2>/dev/null' EXIT
OUT="$WORK/shots"
rm -rf "$WORK"; mkdir -p "$OUT"
pass=0; fail=0
ok() { echo "PASS $1"; pass=$((pass + 1)); }
bad() { echo "FAIL $1: $2"; fail=$((fail + 1)); }
want() { [ -z "$FILTER" ] || [[ "$1" == *"$FILTER"* ]]; }
g() { git -C "$@"; }
# Keep the user's global git config out of the tests.
export GIT_CONFIG_GLOBAL="$WORK/global.gitconfig"
printf '[user]\n\tname = E2E Global\n\temail = global@example.com\n[core]\n\tautocrlf = false\n' > "$GIT_CONFIG_GLOBAL"

mkcfg() { # dir
  mkdir -p "$1"
  [ -f "$1/settings.toml" ] || printf 'auto_fetch_minutes = 0\n' > "$1/settings.toml"
}

# run <script name> <repo> <view> [VAR=value ...]; config dir: $CFG or cfg-<name>.
run() {
  local name="$1" repo="$2" view="$3"; shift 3
  if [ ! -e "$repo/.git" ]; then echo "ABORT $name: $repo is not a repository"; exit 2; fi
  local cfg="${CFG:-$WORK/cfg-$name}"
  mkcfg "$cfg"
  sed -e "s#\$OUT#$(cygpath -m "$OUT")#g" -e "s#\$WORK#$(cygpath -m "$WORK")#g" \
    "$ROOT/scripts/e2e/$name.script" > "$WORK/$name.script"
  env "$@" GK_DX11_INPUT_SCRIPT="$(cygpath -m "$WORK/$name.script")" \
    GITKWAKEN_CONFIG_DIR="$(cygpath -w "$cfg")" \
    GITKWAKEN_START_VIEW="$view" GITKWAKEN_NO_UPDATE_CHECK=1 \
    timeout 120 "$EXE" "$(cygpath -m "$repo")"
}

demo() { # name [--conflict]
  bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/$1" ${2:-} >/dev/null
  echo "$WORK/$1"
}

# ---------------------------------------------------------------- file lists
if want chips; then
  r=$(demo chips)
  run fb-chips "$r" status
  staged=$(g "$r" diff --cached --name-only | tr '\n' ' ')
  [[ "$staged" == *TODO.txt* ]] && [[ "$staged" == *assets/logo.svg* ]] && [[ "$staged" != *README.md* ]] \
    && ok chips || bad chips "staged='$staged'"
fi

if want ignore-custom; then
  r=$(demo ignore)
  run fb-ignore-custom "$r" status
  grep -qx '\*.txt' "$r/.gitignore" 2>/dev/null \
    && [ -z "$(g "$r" status --porcelain -- TODO.txt)" ] \
    && ok ignore-custom || bad ignore-custom ".gitignore='$(cat "$r/.gitignore" 2>/dev/null)'"
fi


if want external-diff; then
  r=$(demo extdiff)
  cfg="$WORK/cfg-fb-external-diff"; mkdir -p "$cfg"
  o=$(cygpath -w "$WORK")
  cat > "$cfg/settings.toml" <<TOML
auto_fetch_minutes = 0
[external_diff_tool]
name = "copy"
command = 'C:\Windows\System32\cmd.exe'
args = '/C copy /Y \$LOCAL "$o\ext-old.txt" & copy /Y \$REMOTE "$o\ext-new.txt"'
TOML
  run fb-external-diff "$r" status
  old=$(cat "$WORK/ext-old.txt" 2>/dev/null); new=$(cat "$WORK/ext-new.txt" 2>/dev/null)
  [[ "$old" == *"Usage: run it."* ]] && [[ "$old" != *"More docs."* ]] && [[ "$new" == *"More docs."* ]] \
    && ok external-diff || bad external-diff "old='$old' new='$new'"
fi

if want stage-keys; then
  r=$(demo stagekeys)
  run fb-stage-all "$r" status
  left=$(g "$r" status --porcelain | grep -c '^.[^ ]')
  [ "$left" = 0 ] && ok stage-all-shortcut || bad stage-all-shortcut "$left unstaged entries left"
  run fb-unstage-all "$r" status
  staged=$(g "$r" diff --cached --name-only | wc -l)
  [ "$staged" = 0 ] && ok unstage-all-shortcut || bad unstage-all-shortcut "$staged staged entries left"
fi

if want keys-status; then
  r=$(demo keysstatus)
  for i in $(seq 1 30); do echo "line $i"; done > "$r/A-long.txt"
  g "$r" add A-long.txt; g "$r" commit -q -m "Add a long file"
  sed -i -e 's/^line 3$/LINE 3/' -e 's/^line 27$/LINE 27/' "$r/A-long.txt"
  run fb-keys-status "$r" status
  staged=$(g "$r" diff --cached -- A-long.txt); unstaged=$(g "$r" diff -- A-long.txt)
  [[ "$staged" == *"+LINE 27"* ]] && [[ "$staged" != *"+LINE 3"* ]] && [[ "$unstaged" == *"+LINE 3"* ]] \
    && [ -s "$OUT/fb-keys-menu.png" ] \
    && ok keys-hunk-stage || bad keys-hunk-stage "staged='$staged'"
  [ -z "$(g "$r" diff -- README.md)" ] && ok keys-discard || bad keys-discard "README.md still modified"
fi

if want commit-options; then
  r=$(demo commitopts)
  printf 'TEMPLATE-SUBJECT\n# remove me\n' > "$r/.git/tpl.txt"
  g "$r" config commit.template .git/tpl.txt
  printf '#!/bin/sh\necho "hook: checks passed"\n' > "$r/.git/hooks/pre-commit"
  run fb-commit-options "$r" status
  body=$(g "$r" log -1 --format=%B); author=$(g "$r" log -1 --format='%an <%ae>')
  [[ "$body" == TEMPLATE-SUBJECT* ]] && [[ "$body" == *"Body from E2E"* ]] && [[ "$body" != *"remove me"* ]] \
    && [ "$author" = "Override Person <override@example.com>" ] \
    && ok commit-options || bad commit-options "author='$author' body='$body'"
fi

# ---------------------------------------------------------------- config editors
if want gitignore; then
  r=$(demo gitignore)
  printf 'x\n' > "$r/debug.log"
  run fb-gitignore "$r" status GITKWAKEN_DIALOG=gitignore
  [ "$(cat "$r/.gitignore" 2>/dev/null)" = '*.log' ] \
    && [ -z "$(g "$r" status --porcelain -- debug.log)" ] \
    && ok gitignore-editor || bad gitignore-editor ".gitignore='$(cat "$r/.gitignore" 2>/dev/null)'"
fi

if want repo-config; then
  r=$(demo repoconfig)
  run fb-repo-config "$r" status GITKWAKEN_DIALOG=repo-config
  v=$(g "$r" config gkfeat.flag)
  [ "$v" = on ] && [ -f "$r/.git/config.bak" ] && ! grep -q gkfeat "$r/.git/config.bak" \
    && ok repo-config-editor || bad repo-config-editor "gkfeat.flag='$v' backup=$(ls "$r/.git/config.bak" 2>&1)"
  before=$(cat "$r/.git/config")
  rm -f "$r/.git/config.bak"
  run fb-repo-config-invalid "$r" status GITKWAKEN_DIALOG=repo-config
  [ "$(cat "$r/.git/config")" = "$before" ] && [ ! -f "$r/.git/config.bak" ] \
    && ok repo-config-invalid-refused || bad repo-config-invalid-refused "config changed"
fi

if want global-config; then
  r=$(demo globalcfg)
  run fb-global-config "$r" status GITKWAKEN_DIALOG=global-config
  v=$(git config -f "$GIT_CONFIG_GLOBAL" gkglobal.x)
  [ "$v" = 1 ] && grep -q 'global@example.com' "$GIT_CONFIG_GLOBAL.bak" 2>/dev/null \
    && [ "$(git config -f "$GIT_CONFIG_GLOBAL" user.email)" = global@example.com ] \
    && ok global-config-editor || bad global-config-editor "gkglobal.x='$v'"
fi

# ---------------------------------------------------------------- keymap
if want keymap; then
  r=$(demo keymap)
  bare="$WORK/keymap-remote.git"; other="$WORK/keymap-other"
  git clone -q --bare "$r" "$bare"; g "$r" remote add origin "$bare"; g "$r" fetch -q origin
  git clone -q "$bare" "$other"; g "$other" config user.name O; g "$other" config user.email o@o
  g "$other" commit -q --allow-empty -m "Remote commit"; g "$other" push -q origin main
  run fb-keymap "$r" status GITKWAKEN_DIALOG=settings:keyboard
  grep -q '^fetch = "Ctrl+Alt+F"' "$WORK/cfg-fb-keymap/settings.toml" \
    && ok keymap-editor || bad keymap-editor "keymap: $(grep -A3 '^\[keymap\]' "$WORK/cfg-fb-keymap/settings.toml")"
  CFG="$WORK/cfg-fb-keymap" run fb-keymap-use "$r" status
  [ "$(g "$r" rev-parse origin/main)" = "$(g "$bare" rev-parse main)" ] \
    && ok keymap-rebound-shortcut || bad keymap-rebound-shortcut "origin/main not fetched"
fi

# ---------------------------------------------------------------- clone
if want clone; then
  r=$(demo clonesrc)
  git clone -q --bare "$r" "$WORK/sparse-src.git"
  mkdir -p "$WORK/clones"
  run fb-clone-sparse "$r" status
  c="$WORK/clones/sparse-src"
  cone=$(g "$c" sparse-checkout list 2>/dev/null | tr '\n' ' ')
  [ -f "$c/README.md" ] && [ -f "$c/src/main.rs" ] && [ ! -e "$c/docs" ] && [ "$cone" = "src " ] \
    && grep -q 'sparse-src' "$WORK/cfg-fb-clone-sparse/settings.toml" \
    && ok clone-sparse-background || bad clone-sparse-background "cone='$cone' files=$(ls "$c" 2>&1 | tr '\n' ' ')"
fi

# ---------------------------------------------------------------- diagnostics
if want diagnostics; then
  r=$(demo diag)
  run fb-optimize-diag "$r" status GITKWAKEN_DIALOG=optimize
  left=$(ls -a "$WORK" "$r/.git" | grep -c 'gitkwaken-diag')
  [ -s "$OUT/fb-optimize-diag.png" ] && [ "$left" = 0 ] \
    && ok optimize-diagnostics || bad optimize-diagnostics "scratch left=$left"
fi

if want auth; then
  r=$(demo auth)
  git clone -q --bare "$r" "$WORK/auth-remote.git"; g "$r" remote add origin "$(cygpath -m "$WORK/auth-remote.git")"
  # Without the system config, Git for Windows' "credential.helper = manager"
  # is gone: the dialog offers to configure GCM (when GCM is installed).
  run fb-auth "$r" status GITKWAKEN_DIALOG=authentication GIT_CONFIG_NOSYSTEM=1
  root=$(cygpath -u "$(git --exec-path)"); root="${root%/mingw64/libexec/git-core}"
  helper=$(git config -f "$GIT_CONFIG_GLOBAL" --get-all credential.helper)
  if [ -f "$root/mingw64/bin/git-credential-manager.exe" ]; then
    [ "$helper" = manager ] && [ -s "$OUT/fb-auth.png" ] \
      && ok auth-gcm-setup || bad auth-gcm-setup "credential.helper='$helper'"
  else
    [ -z "$helper" ] && [ -s "$OUT/fb-auth.png" ] \
      && ok auth-diagnostics-no-gcm || bad auth-diagnostics-no-gcm "credential.helper='$helper'"
  fi
  git config -f "$GIT_CONFIG_GLOBAL" --unset-all credential.helper
fi

# ---------------------------------------------------------------- bookmarks
if want bookmark-current; then
  r=$(demo bmcur)
  cfg="$WORK/cfg-fb-bookmark-current"
  CFG="$cfg" run fb-bookmark-current "$r" status
  n=$(grep -c '^\[\[bookmarks\]\]' "$cfg/settings.toml" 2>/dev/null)
  paths=$(grep '^path' "$cfg/settings.toml" | tr '\\' '/' | tr -s '/')
  [ "$n" = 1 ] && [[ "$paths" == *"/bmcur"[\'\"]* ]] \
    && ok bookmark-open-repo || bad bookmark-open-repo "bookmarks=$n: $paths"
  CFG="$cfg" run fb-bookmark-remove "$r" status
  n=$(grep -c '^\[\[bookmarks\]\]' "$cfg/settings.toml" 2>/dev/null)
  [ "$n" = 0 ] && ok bookmark-remove-open-repo || bad bookmark-remove-open-repo "bookmarks=$n"
fi

# ---------------------------------------------------------------- commit-graph offer
# A history above the 20k-commit threshold (fast-import, no commit-graph).
bigrepo() { # dir commits
  local d="$1" n="$2" i
  git init -q -b main "$d"; g "$d" config user.name E2E; g "$d" config user.email e2e@example.com
  for ((i = 1; i <= n; i++)); do
    printf 'commit refs/heads/main\ncommitter E2E <e2e@example.com> %d +0000\ndata 3\nc%d\nM 644 inline f.txt\ndata <<E\n%d\nE\n\n' \
      $((1700000000 + i)) $((i % 10)) "$i"
  done | g "$d" fast-import --quiet
  g "$d" reset -q --hard main
}
if want commit-graph-offer; then
  r="$WORK/cgbig"; bigrepo "$r" 20500
  [ ! -e "$r/.git/objects/info/commit-graph" ] || bad commit-graph-offer "fixture already has a commit-graph"
  run fb-commit-graph-offer "$r" history
  [ -e "$r/.git/objects/info/commit-graph" ] || [ -e "$r/.git/objects/info/commit-graphs" ] \
    && ok commit-graph-offer-write || bad commit-graph-offer-write "no commit-graph written"
  # "Don't ask for this repository" is remembered in the per-repository state.
  rm -rf "$r/.git/objects/info/commit-graph" "$r/.git/objects/info/commit-graphs"
  run fb-commit-graph-never "$r" history
  st=$(cat "$WORK"/cfg-fb-commit-graph-never/repos/*.toml 2>/dev/null)
  [[ "$st" == *"commit_graph_offer_dismissed = true"* ]] && [ ! -e "$r/.git/objects/info/commit-graph" ] \
    && ok commit-graph-offer-never || bad commit-graph-offer-never "state: $(echo "$st" | grep commit_graph)"
fi

# ---------------------------------------------------------------- diff views
if want views; then
  r=$(demo views)
  for i in $(seq 1 200); do echo "line $i of a file that is larger than one kilobyte"; done > "$r/big.txt"
  oid() { printf '%064d' "$1"; }
  printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize 1048576\n' "$(oid 1)" > "$r/model.bin"
  cp "$ROOT/assets/icon-256.png" "$r/img.png"
  g "$r" add big.txt model.bin img.png; g "$r" commit -q -m "Add view fixtures"
  sed -i 's/^line 1 of/LINE ONE of/' "$r/big.txt"
  printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize 2097152\n' "$(oid 2)" > "$r/model.bin"
  cfg="$WORK/cfg-fb-large"; mkdir -p "$cfg"
  printf 'auto_fetch_minutes = 0\n[diff]\nmax_file_kb = 1\n' > "$cfg/settings.toml"
  run fb-large "$r" status GITKWAKEN_SELECT=big.txt
  staged=$(g "$r" diff --cached -- big.txt)
  [[ "$staged" == *"+LINE ONE of"* ]] && ok large-file-show-anyway || bad large-file-show-anyway "big.txt hunk not staged"
  run fb-lfs "$r" status GITKWAKEN_SELECT=model.bin
  [ -s "$OUT/fb-lfs.png" ] && ok lfs-pointer-view || bad lfs-pointer-view "no screenshot"
  # A different image: the screenshot just taken.
  cp "$OUT/fb-lfs.png" "$r/img.png"
  run fb-overlay "$r" status GITKWAKEN_SELECT=img.png
  [ -s "$OUT/fb-overlay.png" ] && ok image-overlay || bad image-overlay "no screenshot"
fi

# ---------------------------------------------------------------- startup checks
if want git-banner; then
  r=$(demo banner)
  run fb-git-banner "$r" status GITKWAKEN_FAKE_GIT_VERSION=2.30.1
  [ -s "$OUT/fb-git-banner.png" ] && ok git-version-banner || bad git-version-banner "no screenshot"
fi
echo "features-b: $pass passed, $fail failed"
[ "$fail" = 0 ]
