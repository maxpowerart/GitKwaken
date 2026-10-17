#!/usr/bin/env bash
# Runs every end-to-end UI suite against one build and summarises the result.
# Used by .github/workflows/e2e.yml; works locally too.
#
#   scripts/ci-e2e.sh <gitkwaken.exe> <gk-bench.exe> [suite...]
#
# Suites: core history remote update features-a features-b (default: all that
# exist). Each suite runs even if an earlier one failed; the exit code is
# non-zero when any failed. Logs go to bench-data/ci-e2e/<suite>.log,
# screenshots stay where each suite writes them (bench-data/e2e-*/).
#
# The app needs a desktop session (GitHub's windows runners have one). Without
# a GPU set GK_DX11_WARP=1 to force the D3D11 WARP software rasterizer.
set -uo pipefail
EXE="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
BENCH="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
shift 2
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOGS="$ROOT/bench-data/ci-e2e"
mkdir -p "$LOGS"
# Safety (the suites set this too): git and the app must never discover the
# project repository above bench-data when a scenario repo is missing.
export GIT_CEILING_DIRECTORIES="$(cygpath -m "$ROOT/bench-data" 2>/dev/null || echo "$ROOT/bench-data")"
# One wrapper run at a time; the suites lock their own work dirs as well.
if ! mkdir "$LOGS.lock" 2>/dev/null; then echo "another ci-e2e run is active ($LOGS.lock)"; exit 3; fi
trap 'rmdir "$LOGS.lock" 2>/dev/null' EXIT

declare -A SCRIPT=(
  [core]=run-e2e.sh
  [history]=run-e2e-history.sh
  [remote]=run-e2e-remote.sh
  [update]=run-e2e-update.sh
  [features-a]=run-e2e-features-a.sh
  [features-b]=run-e2e-features-b.sh
)
ORDER=(core history remote update features-a features-b)
suites=("$@")
[ ${#suites[@]} -eq 0 ] && suites=("${ORDER[@]}")

summary="$LOGS/summary.md"
printf '### E2E UI suites\n\n| suite | result | passed | failed | time |\n|---|---|---:|---:|---:|\n' >"$summary"
failed_suites=()
for s in "${suites[@]}"; do
  script="$ROOT/scripts/${SCRIPT[$s]:-}"
  if [ -z "${SCRIPT[$s]:-}" ] || [ ! -f "$script" ]; then
    echo "== $s: not present, skipped"
    printf '| %s | skipped (no script) | | | |\n' "$s" >>"$summary"
    continue
  fi
  echo "== $s ($script)"
  start=$SECONDS
  if [ "$s" = update ]; then
    bash "$script" "$EXE" "$BENCH" 2>&1 | tee "$LOGS/$s.log"
  else
    bash "$script" "$EXE" 2>&1 | tee "$LOGS/$s.log"
  fi
  rc=${PIPESTATUS[0]}
  p=$(grep -c '^PASS' "$LOGS/$s.log")
  f=$(grep -c '^FAIL' "$LOGS/$s.log")
  if [ "$rc" = 0 ]; then res=pass; else res="**FAIL** (exit $rc)"; failed_suites+=("$s"); fi
  printf '| %s | %s | %s | %s | %ss |\n' "$s" "$res" "$p" "$f" "$((SECONDS - start))" >>"$summary"
  grep '^FAIL' "$LOGS/$s.log" | sed 's/^/  /' >>"$LOGS/failures.txt" || true
done

cat "$summary"
[ -n "${GITHUB_STEP_SUMMARY:-}" ] && cat "$summary" >>"$GITHUB_STEP_SUMMARY"
if [ ${#failed_suites[@]} -ne 0 ]; then
  echo "failed suites: ${failed_suites[*]}"
  exit 1
fi
