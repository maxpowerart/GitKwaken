#!/usr/bin/env bash
# End-to-end UI tests for remotes and working-copy operations: clone, fetch,
# pull, push, remotes, stash, discard, amend, settings, blame, merge editor,
# tag push. Drives the real app through scripted input (GK_DX11_INPUT_SCRIPT)
# and checks the resulting repositories with git. Remotes are local bare
# repositories (no credentials needed).
# Usage: scripts/run-e2e-remote.sh <path to gitkwaken.exe> [scenario filter]
set -uo pipefail
EXE="$1"
FILTER="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench-data/e2e-remote"
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

# Config dir with auto-fetch off, so only the UI fetches.
mkcfg() { # dir
  mkdir -p "$1"
  [ -f "$1/settings.toml" ] || printf 'auto_fetch_minutes = 0\n' > "$1/settings.toml"
}

# run <script name> <repo> <view> [config dir]
run() {
  local name="$1" repo="$2" view="$3" cfg="${4:-$WORK/cfg-$1}"
  if [ ! -e "$repo/.git" ]; then echo "ABORT $name: $repo is not a repository"; exit 2; fi
  mkcfg "$cfg"
  sed -e "s#\$OUT#$(cygpath -m "$OUT")#g" -e "s#\$WORK#$(cygpath -m "$WORK")#g" \
    "$ROOT/scripts/e2e/$name.script" > "$WORK/$name.script"
  GK_DX11_INPUT_SCRIPT="$(cygpath -m "$WORK/$name.script")" \
  GITKWAKEN_CONFIG_DIR="$(cygpath -w "$cfg")" \
  GITKWAKEN_START_VIEW="$view" GITKWAKEN_NO_UPDATE_CHECK=1 \
    timeout 90 "$EXE" "$(cygpath -m "$repo")"
}

# A seed repository (demo history on main, tags v0.1.0 and v0.2.0).
SEED="$WORK/seed"
bash "$ROOT/scripts/make-demo-repo.sh" "$SEED" >/dev/null

ident() { # repo
  g "$1" config user.name "E2E User"
  g "$1" config user.email "e2e@example.com"
  g "$1" config core.autocrlf false
}

# remote <name>: $WORK/<name>/bare.git (main only) with two clones, a and b.
remote() {
  local d="$WORK/$1"
  mkdir -p "$d"
  git clone -q --bare --single-branch -b main "$SEED" "$d/bare.git"
  for c in a b; do
    git clone -q -c core.autocrlf=false "$d/bare.git" "$d/$c"
    ident "$d/$c"
  done
  echo "$d"
}

commit_file() { # repo file text message
  printf '%s\n' "$3" >> "$1/$2"
  g "$1" add -- "$2"
  g "$1" commit -q -m "$4"
}

# ---------------------------------------------------------------- clone
if want clone; then
  d=$(remote clone)
  mkdir -p "$WORK/clones"
  run r-clone "$d/a" status
  c="$WORK/clones/bare"
  head=$(g "$c" rev-parse HEAD 2>/dev/null)
  url=$(g "$c" remote get-url origin 2>/dev/null)
  recent=$(grep -c 'clones' "$WORK/cfg-r-clone/settings.toml")
  [ "$head" = "$(g "$d/bare.git" rev-parse main)" ] && [[ "$url" == *bare.git ]] && [ "$recent" -ge 1 ] \
    && ok clone || bad clone "head='$head' url='$url' opened-as-tab=$recent"
fi

# ---------------------------------------------------------------- fetch / pull / push
if want fetch; then
  d=$(remote fetch)
  commit_file "$d/b" b.txt "from b" "Commit from B"; g "$d/b" push -q
  run r-fetch "$d/a" status
  [ "$(g "$d/a" rev-parse origin/main)" = "$(g "$d/bare.git" rev-parse main)" ] \
    && [ "$(g "$d/a" rev-list --count main..origin/main)" = 1 ] \
    && ok fetch || bad fetch "origin/main not updated"
fi

for mode in merge rebase; do
  want "pull-$mode" || continue
  d=$(remote "pull-$mode")
  commit_file "$d/b" b.txt "from b" "Commit from B"; g "$d/b" push -q
  commit_file "$d/a" a.txt "from a" "Local commit in A"
  run "r-pull-$mode" "$d/a" status
  remote_tip=$(g "$d/bare.git" rev-parse main)
  parents=$(g "$d/a" rev-list --parents -n1 HEAD | wc -w)
  subj=$(g "$d/a" log -1 --format=%s)
  if g "$d/a" merge-base --is-ancestor "$remote_tip" HEAD 2>/dev/null; then
    if [ "$mode" = merge ]; then
      [ "$parents" = 3 ] && ok pull-merge || bad pull-merge "HEAD is not a merge commit"
    else
      [ "$parents" = 2 ] && [ "$subj" = "Local commit in A" ] && ok pull-rebase \
        || bad pull-rebase "not rebased: parents=$parents subject='$subj'"
    fi
  else
    bad "pull-$mode" "HEAD does not contain the remote commit"
  fi
done

if want push-commit; then
  d=$(remote push-commit)
  commit_file "$d/a" a.txt "from a" "Pushed from A"
  run r-push "$d/a" status
  [ "$(g "$d/bare.git" rev-parse main)" = "$(g "$d/a" rev-parse HEAD)" ] \
    && ok push || bad push "bare main=$(g "$d/bare.git" log -1 --format=%s main)"
fi

if want push-branch; then
  d=$(remote push-branch)
  g "$d/a" checkout -q -b topic
  commit_file "$d/a" t.txt "topic" "Topic commit"
  run r-push-branch "$d/a" status
  up=$(g "$d/a" rev-parse --abbrev-ref '@{u}' 2>/dev/null)
  [ "$(g "$d/bare.git" rev-parse topic 2>/dev/null)" = "$(g "$d/a" rev-parse HEAD)" ] && [ "$up" = origin/topic ] \
    && ok push-branch || bad push-branch "upstream='$up'"
fi

if want force-push; then
  d=$(remote force-push)
  commit_file "$d/a" a.txt "v1" "Before amend"; g "$d/a" push -q
  g "$d/a" commit -q --amend -m "After amend"
  run r-force-push "$d/a" status
  [ "$(g "$d/bare.git" log -1 --format=%s main)" = "After amend" ] \
    && ok force-push || bad force-push "remote: $(g "$d/bare.git" log -1 --format=%s main)"
fi

# ---------------------------------------------------------------- remotes
if want remote; then
  d=$(remote remotes)
  git clone -q --bare "$d/bare.git" "$d/other.git"
  run r-remote-add "$d/a" status
  url=$(g "$d/a" remote get-url upstream 2>/dev/null)
  fetched=$(g "$d/a" rev-parse --verify -q upstream/main)
  [[ "$url" == *other.git ]] && [ -n "$fetched" ] && ok remote-add || bad remote-add "url='$url' fetched='$fetched'"
  run r-remote-edit "$d/a" status
  url=$(g "$d/a" remote get-url upstream 2>/dev/null)
  [[ "$url" == *edited.git ]] && ok remote-edit || bad remote-edit "url='$url'"
  run r-remote-remove "$d/a" status
  remotes=$(g "$d/a" remote | tr '\n' ' ')
  [ "$remotes" = "origin " ] && ok remote-remove || bad remote-remove "remotes='$remotes'"
fi

# ---------------------------------------------------------------- stash
if want stash; then
  d=$(remote stash)
  r="$d/a"
  printf 'changed\n' >> "$r/README.md"; printf 'new\n' > "$r/new.txt"
  run r-stash "$r" status
  list=$(g "$r" stash list)
  dirty=$(g "$r" status --porcelain)
  untracked=$(g "$r" show --name-only --format= 'stash@{0}^3' 2>/dev/null)
  [[ "$list" == *"E2E stash"* ]] && [ -z "$dirty" ] && [ "$untracked" = new.txt ] \
    && ok stash-push || bad stash-push "list='$list' dirty='$dirty' untracked='$untracked'"

  run r-stash-apply "$r" status
  n=$(g "$r" stash list | wc -l)
  grep -q changed "$r/README.md" && [ -f "$r/new.txt" ] && [ "$n" = 1 ] \
    && ok stash-apply || bad stash-apply "stashes=$n status=$(g "$r" status --porcelain | tr '\n' ' ')"

  g "$r" checkout -q -- . && rm -f "$r/new.txt"
  run r-stash-pop "$r" status
  n=$(g "$r" stash list | wc -l)
  grep -q changed "$r/README.md" && [ "$n" = 0 ] \
    && ok stash-pop || bad stash-pop "stashes=$n"

  g "$r" stash push -q -u -m "to drop"
  run r-stash-drop "$r" status
  n=$(g "$r" stash list | wc -l)
  [ "$n" = 0 ] && [ -z "$(g "$r" status --porcelain)" ] && ok stash-drop || bad stash-drop "stashes=$n"
fi

# ---------------------------------------------------------------- discard
if want discard; then
  d=$(remote discard)
  r="$d/a"
  printf 'changed\n' >> "$r/README.md"; printf 'new\n' > "$r/new.txt"
  run r-discard-file "$r" status
  [ -z "$(g "$r" status --porcelain -- README.md)" ] && [ -f "$r/new.txt" ] \
    && ok discard-file || bad discard-file "$(g "$r" status --porcelain | tr '\n' ' ')"
  run r-discard-untracked "$r" status
  [ ! -e "$r/new.txt" ] && ok discard-untracked || bad discard-untracked "new.txt still there"

  printf 'changed\n' >> "$r/README.md"; printf 'x\n' >> "$r/src/main.rs"
  g "$r" add src/main.rs; printf 'new\n' > "$r/new.txt"
  run r-discard-all "$r" status
  left=$(g "$r" status --porcelain)
  [ -z "$left" ] && ok discard-all || bad discard-all "left='$left'"
fi

# ---------------------------------------------------------------- commit
if want amend; then
  d=$(remote amend)
  r="$d/a"
  before=$(g "$r" rev-list --count HEAD)
  run r-amend "$r" status
  msg=$(g "$r" log -1 --format=%s)
  [ "$msg" = "Amended by E2E" ] && [ "$(g "$r" rev-list --count HEAD)" = "$before" ] \
    && ok amend || bad amend "msg='$msg' count $before -> $(g "$r" rev-list --count HEAD)"
fi

if want commit-push; then
  d=$(remote commit-push)
  r="$d/a"
  printf 'changed\n' >> "$r/README.md"
  run r-commit-push "$r" status
  [ "$(g "$d/bare.git" log -1 --format=%s main)" = "Commit and push" ] \
    && [ "$(g "$d/bare.git" rev-parse main)" = "$(g "$r" rev-parse HEAD)" ] \
    && ok commit-push || bad commit-push "remote: $(g "$d/bare.git" log -1 --format=%s main)"
fi

# ---------------------------------------------------------------- settings
if want settings; then
  d=$(remote settings)
  r="$d/a"
  printf 'changed\n' >> "$r/README.md"
  cfg="$WORK/cfg-settings"
  run r-settings "$r" status "$cfg"
  s="$cfg/settings.toml"
  grep -q '^theme = "light"' "$s" && grep -q '^context_lines = 6' "$s" \
    && ok settings-save || bad settings-save "$(grep -E 'theme|context_lines' "$s" | tr '\n' ' ')"
  run r-settings-reopen "$r" status "$cfg"
  grep -q '^theme = "light"' "$s" && grep -q '^context_lines = 6' "$s" && [ -s "$OUT/settings-reopen.png" ] \
    && ok settings-persist || bad settings-persist "$(grep -E 'theme|context_lines' "$s" | tr '\n' ' ')"
fi

# ---------------------------------------------------------------- blame
if want blame; then
  d=$(remote blame)
  r="$d/a"
  printf 'changed\n' >> "$r/README.md"
  run r-blame "$r" status
  [ -s "$OUT/blame.png" ] && [ -s "$OUT/blame-previous.png" ] && ! cmp -s "$OUT/blame.png" "$OUT/blame-previous.png" \
    && ok blame || bad blame "screenshots missing or identical"
fi

# ---------------------------------------------------------------- merge editor
for pick in ours both; do
  want "merge-$pick" || continue
  r="$WORK/merge-$pick"
  bash "$ROOT/scripts/make-demo-repo.sh" "$r" --conflict >/dev/null
  run "r-merge-$pick" "$r" "merge:src/lib.rs"
  unmerged=$(g "$r" diff --name-only --diff-filter=U)
  body=$(head -2 "$r/src/lib.rs" | tr '\n' '|')
  if [ "$pick" = ours ]; then want_body="pub fn one() {}|pub fn f2() {}|"; else want_body="pub fn one() {}|pub fn first() {}|"; fi
  [ -z "$unmerged" ] && [ "$body" = "$want_body" ] && ! grep -q '^<<<<<<<' "$r/src/lib.rs" \
    && ok "merge-$pick" || bad "merge-$pick" "unmerged='$unmerged' head='$body'"
done

# ---------------------------------------------------------------- tag push
if want tag; then
  d=$(remote tag)
  run r-tag-push "$d/a" status
  [ "$(g "$d/bare.git" rev-parse -q --verify 'refs/tags/v9.9')" = "$(g "$d/a" rev-parse HEAD)" ] \
    && ok tag-push || bad tag-push "tags on remote: $(g "$d/bare.git" tag | tr '\n' ' ')"
fi

echo "passed $pass, failed $fail (screenshots: $OUT)"
[ "$fail" = 0 ]
