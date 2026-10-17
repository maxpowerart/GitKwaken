#!/usr/bin/env bash
# Fetch / generate the large-repo benchmark corpus into bench-data/.
#
#   scripts/fetch-corpus.sh [linux] [chromium] [lfs] [all]
#
# Default (no args): linux + lfs.
#
#   linux     Linux kernel, blobless partial clone, no checkout.
#             ~1.3M commits, ~95k files at HEAD. Download ~2.5-3 GB
#             (commits + trees only), 20-60 min depending on bandwidth.
#             Afterwards writes a commit-graph with changed-path Bloom filters
#             (~2-5 min) so history/log benches measure GitKwaken, not git's
#             object parsing.
#   chromium  Chromium src, blobless, no checkout (OPTIONAL, very large:
#             ~1.4M commits, ~450k files at HEAD; download ~10-15 GB of
#             commits+trees, 1-3 h). Only fetched when explicitly requested.
#   lfs       Small local repo with N LFS-tracked binaries for LFS UI tests
#             (requires `git lfs`; skipped with a message otherwise).
#
# Re-running is idempotent: existing clones are fetched/updated instead.
# Env: GK_CORPUS_DIR (default <repo>/bench-data), GK_LFS_FILES (default 200).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="${GK_CORPUS_DIR:-$ROOT/bench-data}"
mkdir -p "$DATA"
# Safety: git must never walk up from a corpus folder into the project repo.
export GIT_CEILING_DIRECTORIES="$(cd "$DATA" && pwd -W 2>/dev/null || pwd)"

log() { printf '[fetch-corpus %s] %s\n' "$(date +%H:%M:%S)" "$*"; }

# Refuses to run git in a folder that is not its own repository.
own_repo() {
    if [ ! -d "$1/.git" ] && [ ! -f "$1/HEAD" ]; then
        echo "refusing: $1 is not a git repository" >&2
        exit 4
    fi
}

# Blobless clone + commit-graph. $1 = name, $2 = url.
blobless() {
    local name="$1" url="$2" dir="$DATA/$1"
    local t0=$SECONDS
    if [ -d "$dir/.git" ] || [ -f "$dir/HEAD" ]; then
        log "$name: exists, fetching updates"
        git -C "$dir" fetch --filter=blob:none origin
    else
        log "$name: cloning $url (blob:none, no checkout)"
        git clone --filter=blob:none --no-checkout "$url" "$dir"
    fi
    log "$name: clone/fetch took $((SECONDS - t0)) s"
    own_repo "$dir"
    local t1=$SECONDS
    # Never lazily fetch blobs from the promisor behind our back during benches.
    git -C "$dir" config core.commitGraph true
    git -C "$dir" config fetch.writeCommitGraph true
    git -C "$dir" commit-graph write --reachable --changed-paths
    log "$name: commit-graph took $((SECONDS - t1)) s"
    log "$name: commits=$(git -C "$dir" rev-list --count --all) size=$(du -sh "$dir/.git" | cut -f1)"
}

lfs_repo() {
    if ! git lfs version >/dev/null 2>&1; then
        log "lfs: git-lfs not installed - skipping LFS test repo"
        return 0
    fi
    local dir="$DATA/lfs-test" n="${GK_LFS_FILES:-200}"
    rm -rf "$dir"
    git init -q -b main "$dir"
    own_repo "$dir"
    (
        cd "$dir"
        git config user.name bench
        git config user.email bench@example.com
        git lfs install --local >/dev/null
        git lfs track '*.bin' >/dev/null
        mkdir -p assets
        local i
        for i in $(seq 1 "$n"); do
            head -c 65536 /dev/urandom >"assets/blob-$i.bin"
        done
        echo "lfs corpus" >README.md
        git add .gitattributes README.md assets
        git commit -q -m "LFS corpus: $n x 64 KiB binaries"
        # second revision of a few files so history/diff has LFS pointer changes
        for i in 1 2 3; do head -c 65536 /dev/urandom >"assets/blob-$i.bin"; done
        git commit -qam "Update 3 LFS binaries"
    )
    log "lfs: $dir ($n files, $(git -C "$dir" lfs ls-files | wc -l) tracked)"
}

targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(linux lfs)
for t in "${targets[@]}"; do
    case "$t" in
    linux) blobless linux https://github.com/torvalds/linux.git ;;
    chromium) blobless chromium https://chromium.googlesource.com/chromium/src.git ;;
    lfs) lfs_repo ;;
    all) blobless linux https://github.com/torvalds/linux.git; lfs_repo ;;
    *) echo "unknown target: $t" >&2; exit 2 ;;
    esac
done
