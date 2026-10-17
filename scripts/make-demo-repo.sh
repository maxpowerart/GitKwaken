#!/usr/bin/env bash
# Creates a small repository exercising most UI states: several branches,
# merges, tags, a stash, staged + unstaged + untracked changes.
# Usage: scripts/make-demo-repo.sh <dir> [--conflict]
set -euo pipefail
dir="$1"
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q -b main
git config user.name "Demo User"
git config user.email "demo@example.com"
git config core.autocrlf false

n=0
commit() { # message, date offset
  n=$((n + 1))
  GIT_AUTHOR_DATE="2026-0$(( (n / 28) % 9 + 1 ))-$(( n % 28 + 1 ))T10:00:00" \
  GIT_COMMITTER_DATE="2026-0$(( (n / 28) % 9 + 1 ))-$(( n % 28 + 1 ))T10:00:00" \
  git commit -q -m "$1"
}

mkdir -p src docs
printf 'fn main() {\n    println!("hello");\n}\n' > src/main.rs
printf '# Demo\n\nA demo repository.\n' > README.md
git add -A && commit "Initial commit"
for i in 1 2 3; do printf 'pub fn f%d() {}\n' "$i" >> src/lib.rs; git add -A; commit "Add f$i to lib"; done
git tag -a v0.1.0 -m "First release"

git checkout -q -b feature/login
printf 'pub fn login() -> bool {\n    true\n}\n' > src/login.rs; git add -A; commit "Add login module"
printf 'pub fn logout() {}\n' >> src/login.rs; git add -A; commit "Add logout"
git checkout -q main
printf 'Usage: run it.\n' >> README.md; git add -A; commit "Document usage"
git merge -q --no-ff feature/login -m "Merge branch 'feature/login'"

git checkout -q -b feature/search
printf 'pub fn search(q: &str) -> Vec<String> {\n    vec![q.to_string()]\n}\n' > src/search.rs; git add -A; commit "Add search"
git checkout -q -b bugfix/crash main
sed -i 's/hello/hello, world/' src/main.rs; git add -A; commit "Fix greeting crash"
git checkout -q main
git merge -q --no-ff bugfix/crash -m "Merge branch 'bugfix/crash'"
for i in 4 5 6 7; do printf 'pub fn f%d() {}\n' "$i" >> src/lib.rs; git add -A; commit "Add f$i to lib"; done
git tag v0.2.0
git checkout -q feature/search
printf '// tuned\n' >> src/search.rs; git add -A; commit "Tune search ranking"
git checkout -q main

# a stash
printf 'experimental\n' >> docs/notes.md; git add -A; git stash push -q -m "WIP: notes"

# working copy changes
printf 'pub fn f8() {}\n' >> src/lib.rs; git add src/lib.rs           # staged
printf '\nMore docs.\n' >> README.md                                  # unstaged
sed -i 's/println/eprintln/' src/main.rs                              # unstaged
printf 'scratch\n' > TODO.txt                                         # untracked
mkdir -p assets && printf 'x' > assets/logo.svg                       # untracked dir

if [ "${2:-}" = "--conflict" ]; then
  git stash push -q -u -m "pre-conflict"
  git checkout -q -b topic HEAD~1
  sed -i 's/f1/first/' src/lib.rs; git add -A; commit "Rename f1 on topic"
  git checkout -q main
  sed -i 's/f1/one/' src/lib.rs; git add -A; commit "Rename f1 on main"
  git merge topic -m "Merge topic" >/dev/null 2>&1 || true
fi
echo "demo repo ready: $dir"
