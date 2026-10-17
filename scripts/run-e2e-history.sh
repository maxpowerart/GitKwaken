#!/usr/bin/env bash
# End-to-end UI tests of the history, search and sidebar views: drives the
# real app through scripted input (GK_DX11_INPUT_SCRIPT, scenarios in
# scripts/e2e/h-*.script) and checks the resulting repository with git.
# Usage: scripts/run-e2e-history.sh <path to gitkwaken.exe> [scenario...]
#
# Coordinates in the scripts assume the default 1400x900 window and the
# history layout of scripts/make-demo-repo.sh (cleaned up, see `repo`).
# Some scenarios read the Windows clipboard (Copy SHA, Ctrl+C).
set -uo pipefail
EXE="$1"; shift
ONLY=" $* "
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench-data/e2e-history"
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
want() { [ "$ONLY" = "  " ] || [[ "$ONLY" == *" $1 "* ]]; }

# id <subject>: the commit of $R with that subject (merges in the demo
# repository carry the current date, so most ids differ between runs).
id() { git -C "$R" log --all --format="%H %s" | awk -v s="$1" 'substr($0, 42) == s { print $1; exit }'; }
ids() {
  F5=$(id 'Add f5 to lib'); F6=$(id 'Add f6 to lib'); F7=$(id 'Add f7 to lib')
  FIX=$(id 'Fix greeting crash'); DOCS=$(id 'Document usage'); ADD_SEARCH=$(id 'Add search')
}

# repo <dir> [dirty]: the demo repository without working copy changes
# (history rows start at the top), or with README.md modified.
repo() {
  bash "$ROOT/scripts/make-demo-repo.sh" "$1" >/dev/null
  git -C "$1" reset -q --hard
  git -C "$1" clean -fdq
  if [ "${2:-}" = dirty ]; then printf '\nMore docs.\n' >> "$1/README.md"; fi
}

run() { # name repo view [VAR=value...]
  local name="$1" repo="$2" view="$3"; shift 3
  if [ ! -e "$repo/.git" ]; then echo "ABORT $name: $repo is not a repository"; exit 2; fi
  sed "s#\$OUT#$(cygpath -m "$OUT")#g" "$ROOT/scripts/e2e/$name.script" > "$WORK/$name.script"
  env "$@" GK_DX11_INPUT_SCRIPT="$(cygpath -m "$WORK/$name.script")" \
    GITKWAKEN_CONFIG_DIR="$(cygpath -w "$WORK/cfg-$name")" \
    GITKWAKEN_START_VIEW="$view" GITKWAKEN_NO_UPDATE_CHECK=1 \
    timeout 120 "$EXE" "$repo"
}

clip() { powershell.exe -NoProfile -Command Get-Clipboard 2>/dev/null | tr -d '\r\n'; }
clip_reset() { powershell.exe -NoProfile -Command "Set-Clipboard -Value e2e-none" >/dev/null 2>&1; }
g() { git -C "$R" "$@"; }

# 1. select a commit (details screenshot); double-click a branch tip with a
# dirty worktree -> checkout dialog -> Checkout (changes carried over)
if want h-select-checkout; then
  R="$WORK/select"; repo "$R" dirty; ids
  run h-select-checkout "$R" history
  cur=$(g branch --show-current); dirty=$(g diff --name-only)
  [ "$cur" = feature/search ] && [ "$dirty" = README.md ] \
    && ok h-select-checkout || bad h-select-checkout "branch='$cur' modified='$dirty'"
fi

# 1b. double-click a commit without branch -> detached HEAD checkout
if want h-checkout-detached; then
  R="$WORK/detached"; repo "$R"; ids
  run h-checkout-detached "$R" history
  head=$(g rev-parse HEAD); sym=$(g symbolic-ref -q HEAD)
  [ "$head" = "$F6" ] && [ -z "$sym" ] \
    && ok h-checkout-detached || bad h-checkout-detached "HEAD=$head ref='$sym'"
fi

# 2. history context menu
if want h-create-branch; then
  R="$WORK/branch"; repo "$R"; ids
  run h-create-branch "$R" history
  at=$(g rev-parse -q --verify e2e-at-f6); cur=$(g branch --show-current)
  [ "$at" = "$F6" ] && [ "$cur" = e2e-at-f6 ] \
    && ok h-create-branch || bad h-create-branch "e2e-at-f6=$at current=$cur"
fi

if want h-create-tag; then
  R="$WORK/tag"; repo "$R"; ids
  run h-create-tag "$R" history
  kind=$(g cat-file -t v-e2e 2>/dev/null); at=$(g rev-parse -q --verify 'v-e2e^{commit}')
  msg=$(g tag -l --format='%(contents:subject)' v-e2e)
  [ "$kind" = tag ] && [ "$at" = "$F5" ] && [ "$msg" = "E2E annotated tag" ] \
    && ok h-create-tag || bad h-create-tag "type=$kind at=$at msg='$msg'"
fi

if want h-cherry-pick; then
  R="$WORK/cherry"; repo "$R"; ids
  run h-cherry-pick "$R" history
  subj=$(g log -1 --format=%s main); parent=$(g rev-parse main~1)
  [ "$subj" = "Add search" ] && [ "$parent" = "$F7" ] && [ -f "$R/src/search.rs" ] \
    && [ "$(g rev-parse main)" != "$(g rev-parse $ADD_SEARCH)" ] \
    && ok h-cherry-pick || bad h-cherry-pick "subject='$subj' parent=$parent"
fi

if want h-revert; then
  R="$WORK/revert"; repo "$R"; ids
  run h-revert "$R" history
  subj=$(g log -1 --format=%s); parent=$(g rev-parse HEAD~1)
  [ "$subj" = 'Revert "Fix greeting crash"' ] && [ "$parent" = "$F7" ] \
    && grep -q 'println!("hello");' "$R/src/main.rs" \
    && ok h-revert || bad h-revert "subject='$subj' parent=$parent"
fi

if want h-reset-mixed; then
  R="$WORK/reset-mixed"; repo "$R"; ids
  run h-reset-mixed "$R" history
  head=$(g rev-parse HEAD); unst=$(g diff --name-only); st=$(g diff --cached --name-only)
  [ "$head" = "$F5" ] && [ "$unst" = src/lib.rs ] && [ -z "$st" ] && grep -q f7 "$R/src/lib.rs" \
    && ok h-reset-mixed || bad h-reset-mixed "HEAD=$head unstaged='$unst' staged='$st'"
fi

if want h-reset-hard; then
  R="$WORK/reset-hard"; repo "$R" dirty; ids
  run h-reset-hard "$R" history
  head=$(g rev-parse HEAD); st=$(g status --porcelain)
  [ "$head" = "$F5" ] && [ -z "$st" ] && ! grep -q f6 "$R/src/lib.rs" \
    && ok h-reset-hard || bad h-reset-hard "HEAD=$head status='$st'"
fi

if want h-copy-sha; then
  R="$WORK/copy"; repo "$R"; clip_reset; ids
  run h-copy-sha "$R" history
  c=$(clip)
  [ "$c" = "$F6" ] && ok h-copy-sha || bad h-copy-sha "clipboard='$c'"
fi

# 3. interactive rebase: reorder, squash, reword (multi-line)
if want h-interactive-rebase; then
  R="$WORK/irebase"; repo "$R"; ids
  git -C "$R" checkout -q -b topic3
  for x in a b c; do
    echo $x > "$R/$x.txt"; git -C "$R" add $x.txt
    d="2026-02-01T1$(printf %d "'$x" | tail -c1):00:00"
    GIT_AUTHOR_DATE=$d GIT_COMMITTER_DATE=$d git -C "$R" commit -q -m "Add $x"
  done
  run h-interactive-rebase "$R" history
  subjects=$(g log --format=%s main..topic3 | tr '\n' '|')
  body=$(g log -1 --format=%B topic3~1)
  files=$(g ls-tree --name-only topic3 | tr '\n' ' ')
  exp_body=$'Reworded A\n\nSecond line of the body.\nThird line.'
  [ "$subjects" = "Add c|Reworded A|" ] && [ "$body" = "$exp_body" ] \
    && [ "$(g rev-parse topic3~2)" = "$F7" ] && [[ "$files" == *"a.txt b.txt c.txt"* ]] \
    && [ "$(g branch --show-current)" = topic3 ] \
    && ok h-interactive-rebase || bad h-interactive-rebase "subjects='$subjects' body='$body' files='$files'"
fi

# 4. merge (no fast-forward) and rebase dialogs
if want h-merge; then
  R="$WORK/merge"; repo "$R"; ids
  git -C "$R" checkout -q -b feature/e2e-merge
  for x in 1 2; do
    echo m$x > "$R/m$x.txt"; git -C "$R" add m$x.txt
    GIT_AUTHOR_DATE=2026-02-01T1$x:00:00 GIT_COMMITTER_DATE=2026-02-01T1$x:00:00 \
      git -C "$R" commit -q -m "Merge test m$x"
  done
  git -C "$R" checkout -q main
  tip=$(g rev-parse feature/e2e-merge)
  run h-merge "$R" history
  parents=$(g log -1 --format=%P main); subj=$(g log -1 --format=%s main)
  [ "$parents" = "$F7 $tip" ] && [[ "$subj" == *"feature/e2e-merge"* ]] && [ -f "$R/m2.txt" ] \
    && ok h-merge || bad h-merge "parents='$parents' subject='$subj'"
fi

if want h-rebase; then
  R="$WORK/rebase"; repo "$R"; ids
  git -C "$R" checkout -q feature/search
  run h-rebase "$R" history
  base=$(g rev-parse feature/search~2); n=$(g rev-list --count main..feature/search)
  merges=$(g rev-list --merges main..feature/search)
  subj=$(g log -2 --format=%s feature/search | tr '\n' '|')
  [ "$base" = "$F7" ] && [ "$n" = 2 ] && [ -z "$merges" ] && [ "$subj" = "Tune search ranking|Add search|" ] \
    && ok h-rebase || bad h-rebase "base=$base ahead=$n subjects='$subj'"
fi

# 5. sidebar
if want h-sidebar-checkout; then
  R="$WORK/sb-checkout"; repo "$R"; ids
  run h-sidebar-checkout "$R" history
  cur=$(g branch --show-current); st=$(g status --porcelain)
  [ "$cur" = feature/login ] && [ -z "$st" ] \
    && ok h-sidebar-checkout || bad h-sidebar-checkout "branch='$cur' status='$st'"
fi

if want h-sidebar-rename; then
  R="$WORK/sb-rename"; repo "$R"; ids
  run h-sidebar-rename "$R" history
  at=$(g rev-parse -q --verify refs/heads/bugfix/renamed); old=$(g rev-parse -q --verify refs/heads/bugfix/crash)
  [ "$at" = "$FIX" ] && [ -z "$old" ] \
    && ok h-sidebar-rename || bad h-sidebar-rename "renamed=$at old=$old"
fi

if want h-sidebar-delete; then
  R="$WORK/sb-delete"; repo "$R"; ids
  run h-sidebar-delete "$R" history
  b=$(g rev-parse -q --verify refs/heads/bugfix/crash); t=$(g rev-parse -q --verify refs/tags/v0.1.0)
  t2=$(g rev-parse -q --verify refs/tags/v0.2.0)
  [ -z "$b" ] && [ -z "$t" ] && [ -n "$t2" ] \
    && ok h-sidebar-delete || bad h-sidebar-delete "bugfix/crash=$b v0.1.0=$t"
fi

if want h-sidebar-show; then
  R="$WORK/sb-show"; repo "$R"; clip_reset; ids
  run h-sidebar-show "$R" history
  c=$(clip)
  [ "$c" = "$FIX" ] && ok h-sidebar-show || bad h-sidebar-show "details Copy gave '$c'"
fi

# 6. search view and file history
if want h-search; then
  R="$WORK/search"; repo "$R"; ids
  run h-search "$R" search
  at=$(g rev-parse -q --verify 'search-tag^{commit}')
  [ "$at" = "$FIX" ] && ok h-search || bad h-search "search-tag=$at"
fi

if want h-file-history; then
  R="$WORK/file-history"; repo "$R" dirty; ids
  run h-file-history "$R" status
  at=$(g rev-parse -q --verify fh-branch); cur=$(g branch --show-current)
  [ "$at" = "$DOCS" ] && [ "$cur" = main ] \
    && ok h-file-history || bad h-file-history "fh-branch=$at current=$cur"
fi

# 7. keyboard navigation, compare, palette (selection checked via Ctrl+C;
# compare and palette on the screenshots)
if want h-keyboard; then
  R="$WORK/keyboard"; repo "$R"; clip_reset; ids
  run h-keyboard "$R" history
  c=$(clip)
  [ "$c" = "$F6" ] && [ -s "$OUT/h-compare.png" ] && [ -s "$OUT/h-palette-file-status.png" ] \
    && ok h-keyboard || bad h-keyboard "Ctrl+C after Down Down Up gave '$c'"
fi

echo "passed $pass, failed $fail (screenshots: $OUT)"
[ "$fail" = 0 ]
