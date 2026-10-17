#!/usr/bin/env bash
# End-to-end UI tests of: repository scan, bookmark groups, history filters
# (message / author / date / path / pickaxe / branch picker), reflog view,
# undo, notes, stash view, branch drag-and-drop and keyboard access.
# Drives the real app through scripted input (GK_DX11_INPUT_SCRIPT,
# scenarios in scripts/e2e/fa-*.script) and checks the results with git
# (or the settings file for bookmarks).
# Usage: scripts/run-e2e-features-a.sh <path to gitkwaken.exe> [scenario...]
#
# Coordinates in the scripts assume the default 1400x900 window and the
# demo repository of scripts/make-demo-repo.sh (cleaned up, see `repo`).
# Some scenarios read the Windows clipboard (Ctrl+C in commit lists).
set -uo pipefail
EXE="$1"; shift
ONLY=" $* "
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench-data/e2e-features-a"
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

g() { git -C "$R" "$@"; }
id() { g log --all --format="%H %s" | awk -v s="$1" 'substr($0, 42) == s { print $1; exit }'; }

# repo <dir> [dirty]: the demo repository without working copy changes.
repo() {
  bash "$ROOT/scripts/make-demo-repo.sh" "$1" >/dev/null
  # Never let `git -C` find an enclosing repository (see GIT_CEILING_DIRECTORIES).
  if [ ! -d "$1/.git" ]; then echo "ABORT: demo repository $1 was not created"; exit 2; fi
  git -C "$1" reset -q --hard
  git -C "$1" clean -fdq
  if [ "${2:-}" = dirty ]; then printf '\nMore docs.\n' >> "$1/README.md"; fi
}

# commit <message> <file> <text>: a commit with a fixed date (stable rows).
commit() {
  printf '%s\n' "$3" >> "$R/$2"
  g add -- "$2"
  GIT_AUTHOR_DATE=2026-03-01T10:00:00 GIT_COMMITTER_DATE=2026-03-01T10:00:00 \
    git -C "$R" commit -q -m "$1"
}

# run <name> <repo|-> <view> [VAR=value...]; "-" starts without a repository.
run() {
  local name="$1" repo="$2" view="$3"; shift 3
  if [ "$repo" != - ] && [ ! -e "$repo/.git" ]; then echo "ABORT $name: $repo is not a repository"; exit 2; fi
  local cfg="$WORK/cfg-$name"
  mkdir -p "$cfg"
  [ -f "$cfg/settings.toml" ] || printf 'auto_fetch_minutes = 0\n' > "$cfg/settings.toml"
  sed -e "s#\$OUT#$(cygpath -m "$OUT")#g" -e "s#\$WORK#$(cygpath -m "$WORK")#g" \
    "$ROOT/scripts/e2e/$name.script" > "$WORK/$name.script"
  local args=()
  [ "$repo" != - ] && args=("$repo")
  env "$@" GK_DX11_INPUT_SCRIPT="$(cygpath -m "$WORK/$name.script")" \
    GITKWAKEN_CONFIG_DIR="$(cygpath -w "$cfg")" \
    GITKWAKEN_START_VIEW="$view" GITKWAKEN_NO_UPDATE_CHECK=1 \
    timeout 120 "$EXE" "${args[@]}"
}

clip() { powershell.exe -NoProfile -Command Get-Clipboard 2>/dev/null | tr -d '\r' | sed '/^$/d'; }
clip_reset() { powershell.exe -NoProfile -Command "Set-Clipboard -Value e2e-none" >/dev/null 2>&1; }
# sorted <text>: lines sorted (lists compared as sets).
sorted() { printf '%s\n' "$1" | sed '/^$/d' | sort; }

# ------------------------------------------------------------------ scan
# Welcome -> Scan folder… -> type the folder -> Scan -> Bookmark all.
if want fa-scan; then
  S="$WORK/scan-root"
  mkdir -p "$S/one" "$S/deep/a/b" "$S/node_modules/pkg" "$S/plain"
  git init -q "$S/one"; git init -q "$S/deep/a/b"; git init -q "$S/node_modules/pkg"
  git init -q "$S/one/inner" # inside a repository: not listed
  run fa-scan - status
  s="$WORK/cfg-fa-scan/settings.toml"
  n=$(grep -c '^\[\[bookmarks\]\]' "$s" 2>/dev/null)
  # Paths as '/'-separated text, whatever TOML quoting was used.
  paths=$(grep '^path' "$s" | tr '\\' '/' | tr -s '/')
  [[ "$paths" == *"scan-root/one"[\'\"]* ]] && [[ "$paths" == *"scan-root/deep/a/b"[\'\"]* ]] \
    && [[ "$paths" != *node_modules* ]] && [[ "$paths" != *inner* ]] && [ "$n" = 2 ] \
    && ok fa-scan || bad fa-scan "bookmarks=$n: $(echo $paths)"
fi

# ------------------------------------------------------------------ bookmark groups
# Two bookmarks; right-click the first -> Move to group -> New group… "Work".
if want fa-bookmark-group; then
  cfg="$WORK/cfg-fa-bookmark-group"; mkdir -p "$cfg"
  cat > "$cfg/settings.toml" <<'EOF'
auto_fetch_minutes = 0

[[bookmarks]]
name = "alpha"
path = "C:/e2e/alpha"

[[bookmarks]]
name = "beta"
path = "C:/e2e/beta"
EOF
  run fa-bookmark-group - status
  s="$cfg/settings.toml"
  grp=$(awk '/^\[\[bookmarks\]\]/{n=""} /^name = /{n=$3} /^group = /{print n, $3}' "$s" | tr -d '"')
  grep -q '^groups = \["Work"\]' "$s" && [ "$grp" = "alpha Work" ] \
    && ok fa-bookmark-group || bad fa-bookmark-group "groups='$(grep '^groups' "$s")' grouped='$grp'"
fi

# ------------------------------------------------------------------ history filters
# Filters row: path src/lib.rs + since 2026-01-08 -> Shift+End, Ctrl+C = the list.
if want fa-filter-path; then
  R="$WORK/filter-path"; repo "$R"; clip_reset
  run fa-filter-path "$R" history
  want_ids=$(g log --format=%H --since=2026-01-08 --branches --remotes --tags HEAD -- src/lib.rs)
  got=$(clip)
  [ -n "$want_ids" ] && [ "$(sorted "$got")" = "$(sorted "$want_ids")" ] \
    && ok fa-filter-path || bad fa-filter-path "got '$(echo $got)' want '$(echo $want_ids)'"
fi

# Message text "search" + author "demo" in the toolbar / filter row.
if want fa-filter-text; then
  R="$WORK/filter-text"; repo "$R"; clip_reset
  run fa-filter-text "$R" history
  want_ids=$(g log --format=%H -i --grep=search --author=demo --branches --remotes --tags HEAD)
  got=$(clip)
  [ -n "$want_ids" ] && [ "$(sorted "$got")" = "$(sorted "$want_ids")" ] \
    && ok fa-filter-text || bad fa-filter-text "got '$(echo $got)' want '$(echo $want_ids)'"
fi

# Pickaxe -G "f[67]" (regex mode).
if want fa-pickaxe; then
  R="$WORK/pickaxe"; repo "$R"; clip_reset
  run fa-pickaxe "$R" history
  want_ids=$(g log --format=%H '-Gf[67]' --branches --remotes --tags HEAD)
  got=$(clip)
  [ -n "$want_ids" ] && [ "$(sorted "$got")" = "$(sorted "$want_ids")" ] \
    && ok fa-pickaxe || bad fa-pickaxe "got '$(echo $got)' want '$(echo $want_ids)'"
fi

# Branch picker: only feature/login -> the history lists its commits.
if want fa-branch-filter; then
  R="$WORK/branch-filter"; repo "$R"; clip_reset
  run fa-branch-filter "$R" history
  want_ids=$(g rev-list feature/login)
  got=$(clip)
  scope=$(grep -o 'history_scope = "[^"]*"' "$R/.git/gitkwaken.toml" 2>/dev/null || true)
  [ "$(sorted "$got")" = "$(sorted "$want_ids")" ] \
    && ok fa-branch-filter || bad fa-branch-filter "got $(echo "$got" | wc -l) ids, want $(echo "$want_ids" | wc -l) ($scope)"
fi

# ------------------------------------------------------------------ reflog
# Reflog view: HEAD@{1} (before the last commit) -> Create branch here…
if want fa-reflog; then
  R="$WORK/reflog"; repo "$R"
  before=$(g rev-parse HEAD)
  commit "Reflog test" README.md "reflog"
  run fa-reflog "$R" reflog
  at=$(g rev-parse -q --verify refs/heads/from-reflog)
  [ "$at" = "$before" ] && ok fa-reflog || bad fa-reflog "from-reflog=$at want $before"
fi

# Reflog: select HEAD@{1}, Enter -> checkout dialog -> detached at it.
if want fa-reflog-checkout; then
  R="$WORK/reflog-co"; repo "$R"
  before=$(g rev-parse HEAD)
  commit "Reflog test" README.md "reflog"
  run fa-reflog-checkout "$R" reflog
  head=$(g rev-parse HEAD); sym=$(g symbolic-ref -q HEAD)
  [ "$head" = "$before" ] && [ -z "$sym" ] \
    && ok fa-reflog-checkout || bad fa-reflog-checkout "HEAD=$head ref='$sym'"
fi

# ------------------------------------------------------------------ undo
# Undo a commit (history toolbar button): branch back, changes staged.
if want fa-undo-commit; then
  R="$WORK/undo-commit"; repo "$R"
  before=$(g rev-parse HEAD)
  commit "To be undone" README.md "undo me"
  run fa-undo-commit "$R" history
  head=$(g rev-parse HEAD); staged=$(g diff --cached --name-only)
  [ "$head" = "$before" ] && [ "$staged" = README.md ] \
    && ok fa-undo-commit || bad fa-undo-commit "HEAD=$head staged='$staged'"
fi

# Undo a merge (palette "Undo last operation…").
if want fa-undo-merge; then
  R="$WORK/undo-merge"; repo "$R"
  before=$(g rev-parse HEAD)
  g merge -q --no-ff feature/search -m "Merge feature/search"
  run fa-undo-merge "$R" history GITKWAKEN_PALETTE="Undo last operation"
  head=$(g rev-parse HEAD); st=$(g status --porcelain)
  [ "$head" = "$before" ] && [ -z "$st" ] \
    && ok fa-undo-merge || bad fa-undo-merge "HEAD=$head status='$st'"
fi

# Undo refuses when uncommitted changes would be lost (merge touched them).
if want fa-undo-refuse; then
  R="$WORK/undo-refuse"; repo "$R"
  g merge -q --no-ff feature/search -m "Merge feature/search"
  merged=$(g rev-parse HEAD)
  printf '// local edit\n' >> "$R/src/search.rs"
  run fa-undo-refuse "$R" history GITKWAKEN_PALETTE="Undo last operation"
  head=$(g rev-parse HEAD)
  [ "$head" = "$merged" ] && grep -q 'local edit' "$R/src/search.rs" \
    && ok fa-undo-refuse || bad fa-undo-refuse "HEAD=$head (merge $merged)"
fi

# ------------------------------------------------------------------ notes
# Context menu -> Add/Edit note… -> two lines -> Ctrl+Enter.
if want fa-notes; then
  R="$WORK/notes"; repo "$R"
  F6=$(id 'Add f6 to lib')
  run fa-notes "$R" history
  note=$(g notes show "$F6" 2>/dev/null)
  [ "$note" = $'Reviewed by E2E\nSecond line' ] \
    && ok fa-notes || bad fa-notes "note='$note'"
fi

if want fa-notes-remove; then
  R="$WORK/notes-rm"; repo "$R"
  F6=$(id 'Add f6 to lib')
  g notes add -m "old note" "$F6"
  run fa-notes-remove "$R" history
  note=$(g notes show "$F6" 2>/dev/null)
  [ -z "$note" ] && ok fa-notes-remove || bad fa-notes-remove "note='$note'"
fi

# ------------------------------------------------------------------ stash view
# Stash with an untracked file; click it in the sidebar -> stash view -> Pop.
if want fa-stash; then
  R="$WORK/stash"; repo "$R"
  g stash drop -q
  printf 'stashed change\n' >> "$R/README.md"; printf 'new file\n' > "$R/untracked.txt"
  g stash push -q -u -m "E2E stash"
  run fa-stash "$R" history
  n=$(g stash list | wc -l)
  [ "$n" = 0 ] && grep -q 'stashed change' "$R/README.md" && [ -f "$R/untracked.txt" ] \
    && ok fa-stash || bad fa-stash "stashes=$n readme=$(tail -1 "$R/README.md")"
fi

# ------------------------------------------------------------------ branch drag and drop
# Drag feature/search onto main (current) -> Merge feature/search into main… -> Merge.
if want fa-drag-merge; then
  R="$WORK/drag-merge"; repo "$R"
  tip=$(g rev-parse feature/search); before=$(g rev-parse main)
  run fa-drag-merge "$R" history
  parents=$(g log -1 --format=%P main)
  [ "$parents" = "$before $tip" ] \
    && ok fa-drag-merge || bad fa-drag-merge "parents='$parents'"
fi

# Drag main onto feature/login (not current) -> Checkout feature/login and merge main.
if want fa-drag-checkout; then
  R="$WORK/drag-checkout"; repo "$R"
  main=$(g rev-parse main)
  run fa-drag-checkout "$R" history
  cur=$(g branch --show-current)
  [ "$cur" = feature/login ] && g merge-base --is-ancestor "$main" feature/login \
    && ok fa-drag-checkout || bad fa-drag-checkout "current=$cur"
fi

# ------------------------------------------------------------------ keyboard
# History: select "Add f6 to lib", Ctrl+B -> branch "kb-branch"; Shift+F10 ->
# Create tag here… -> "kb-tag".
if want fa-keys-history; then
  R="$WORK/keys-history"; repo "$R"
  F6=$(id 'Add f6 to lib')
  run fa-keys-history "$R" history
  b=$(g rev-parse -q --verify refs/heads/kb-branch); t=$(g rev-parse -q --verify 'kb-tag^{commit}')
  [ "$b" = "$F6" ] && [ "$t" = "$F6" ] \
    && ok fa-keys-history || bad fa-keys-history "kb-branch=$b kb-tag=$t"
fi

# Sidebar: click bugfix/crash, Delete -> delete dialog -> Enter; click
# feature/login, Enter -> checked out.
if want fa-keys-sidebar; then
  R="$WORK/keys-sidebar"; repo "$R"
  run fa-keys-sidebar "$R" history
  b=$(g rev-parse -q --verify refs/heads/bugfix/crash); cur=$(g branch --show-current)
  [ -z "$b" ] && [ "$cur" = feature/login ] \
    && ok fa-keys-sidebar || bad fa-keys-sidebar "bugfix/crash=$b current=$cur"
fi

echo "passed $pass, failed $fail (screenshots: $OUT)"
[ "$fail" = 0 ]
