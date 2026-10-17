#!/usr/bin/env bash
# End-to-end UI tests: drives the real app through scripted input
# (GK_DX11_INPUT_SCRIPT) and checks the resulting repository with git.
# Usage: scripts/run-e2e.sh <path to gitkwaken.exe>
set -uo pipefail
EXE="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench-data/e2e-run"
OUT="$WORK/shots"
rm -rf "$WORK"; mkdir -p "$OUT"
pass=0; fail=0
ok() { echo "PASS $1"; pass=$((pass + 1)); }
bad() { echo "FAIL $1: $2"; fail=$((fail + 1)); }

run() { # name repo view
  local name="$1" repo="$2" view="$3"
  sed "s#\$OUT#$(cygpath -m "$OUT")#g" "$ROOT/scripts/e2e/$name.script" > "$WORK/$name.script"
  GK_DX11_INPUT_SCRIPT="$(cygpath -m "$WORK/$name.script")" \
  GITKWAKEN_CONFIG_DIR="$(cygpath -w "$WORK/cfg-$name")" \
  GITKWAKEN_START_VIEW="$view" GITKWAKEN_NO_UPDATE_CHECK=1 \
    timeout 90 "$EXE" "$repo"
}

# 1. commit through the UI
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r1" >/dev/null
run commit "$WORK/r1" status
msg=$(git -C "$WORK/r1" log -1 --format=%s)
files=$(git -C "$WORK/r1" show --name-only --format= HEAD | tr '\n' ' ')
[ "$msg" = "E2E commit from input script" ] && [[ "$files" == *README.md* ]] && [[ "$files" == *src/lib.rs* ]] \
  && ok commit || bad commit "msg='$msg' files='$files'"

# 2. stage a single line of README.md
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r2" >/dev/null
run stage-line "$WORK/r2" status
cached=$(git -C "$WORK/r2" diff --cached -- README.md)
rest=$(git -C "$WORK/r2" diff -- README.md)
[[ "$cached" == *"+More docs."* ]] && [[ "$rest" == *"+"* ]] && [[ "$rest" != *"+More docs."* ]] \
  && ok stage-line || bad stage-line "cached='$cached' unstaged='$rest'"

# 3. create and check out a branch
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r3" >/dev/null
run branch "$WORK/r3" status
cur=$(git -C "$WORK/r3" branch --show-current)
[ "$cur" = "e2e-branch" ] && ok branch || bad branch "current='$cur'"

# 4. resolve a merge conflict in the merge editor
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r4" --conflict >/dev/null
run merge "$WORK/r4" "merge:src/lib.rs"
unmerged=$(git -C "$WORK/r4" diff --name-only --diff-filter=U)
markers=$(grep -c '^<<<<<<<' "$WORK/r4/src/lib.rs")
grep -q 'first' "$WORK/r4/src/lib.rs" && [ -z "$unmerged" ] && [ "$markers" = "0" ] \
  && ok merge-editor || bad merge-editor "unmerged='$unmerged' markers=$markers"

# 5. command palette switches the view (checked on the screenshot)
run palette "$WORK/r1" status
[ -s "$OUT/palette.png" ] && ok palette-ran || bad palette-ran "no screenshot"

# 6. drag README.md onto the staged list
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r6" >/dev/null
run drag "$WORK/r6" status
staged=$(git -C "$WORK/r6" diff --cached --name-only | tr "
" " ")
unst=$(git -C "$WORK/r6" diff --name-only -- README.md)
[[ "$staged" == *README.md* ]] && [ -z "$unst" ] && ok drag-to-stage || bad drag-to-stage "staged='$staged'"

# 7. staging a file that still has conflict markers asks first
bash "$ROOT/scripts/make-demo-repo.sh" "$WORK/r7" --conflict >/dev/null
run conflict-guard "$WORK/r7" status
unmerged=$(git -C "$WORK/r7" diff --name-only --diff-filter=U)
[ "$unmerged" = "src/lib.rs" ] && ok conflict-guard || bad conflict-guard "file was staged with markers"

echo "passed $pass, failed $fail (screenshots: $OUT)"
[ "$fail" = 0 ]
