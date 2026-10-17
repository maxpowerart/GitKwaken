#!/usr/bin/env bash
# Type-checks the whole workspace for Linux and macOS from Windows.
# C dependencies (ring) are compiled with `zig cc` through a small wrapper
# that drops rustc-style --target flags; see docs in this script's history.
# Requires: rustup targets x86_64-unknown-linux-gnu aarch64-apple-darwin,
# zig 0.14 and zcc-linux.exe / zcc-mac.exe / zar.exe in $TOOLS.
set -euo pipefail
TOOLS="${TOOLS:-$HOME/.tools}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/cross}"
export CC_x86_64_unknown_linux_gnu="$(cygpath -w "$TOOLS/zcc-linux.exe")"
export AR_x86_64_unknown_linux_gnu="$(cygpath -w "$TOOLS/zar.exe")"
export CC_aarch64_apple_darwin="$(cygpath -w "$TOOLS/zcc-mac.exe")"
export AR_aarch64_apple_darwin="$(cygpath -w "$TOOLS/zar.exe")"
for t in x86_64-unknown-linux-gnu aarch64-apple-darwin; do
  echo "== $t"
  cargo check --workspace --all-targets --target "$t"
done
