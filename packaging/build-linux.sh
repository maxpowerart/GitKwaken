#!/usr/bin/env bash
# Builds the Linux x86_64 release package.
#
#   packaging/build-linux.sh [--no-build]
#
# Output in dist/:
#   GitKwaken-<ver>-x86_64-linux/              staging folder
#   GitKwaken-<ver>-x86_64-linux.tar.gz        the folder above, as released
#   GitKwaken-<ver>-x86_64-linux.tar.gz.sha256
#   PKGBUILD                                   packaging/arch/PKGBUILD pinned to this tarball
#
# The tarball holds gitkwaken (debug info stripped, symbols kept), README.md,
# LICENSE-MIT, LICENSE-APACHE, THIRD-PARTY-LICENSES.txt, gitkwaken.desktop and
# icons/hicolor/<n>x<n>/apps/gitkwaken.png. Build on the oldest glibc you want
# to support (the release uses ubuntu-22.04, glibc 2.35).
#
# Needs: cargo, pwsh (scripts/third-party-licenses.ps1), ImageMagick
# (`magick` or `convert`), strip, sha256sum.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
TARGET=x86_64-unknown-linux-gnu

BUILD=1
for a in "$@"; do
  case "$a" in
    --no-build) BUILD=0 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

# [workspace.package] version, which gk-app inherits.
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
NAME="GitKwaken-$VERSION-x86_64-linux"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"
BIN="$ROOT/target/$TARGET/release/gitkwaken"

if [ "$BUILD" = 1 ]; then
  echo "==> cargo build --release ($TARGET)"
  cargo build --release --locked -p gk-app --target "$TARGET"
fi
[ -x "$BIN" ] || { echo "$BIN not found (build first or drop --no-build)" >&2; exit 1; }

if command -v magick >/dev/null; then IM=(magick); else IM=(convert); fi

echo "==> staging $STAGE"
rm -rf "$STAGE" "$DIST/$NAME.tar.gz" "$DIST/$NAME.tar.gz.sha256"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/gitkwaken"
strip --strip-debug "$STAGE/gitkwaken"
cp README.md LICENSE-MIT LICENSE-APACHE "$STAGE/"
cp packaging/linux/gitkwaken.desktop "$STAGE/"
for s in 16 24 32 48 64 128 256 512; do
  d="$STAGE/icons/hicolor/${s}x${s}/apps"
  mkdir -p "$d"
  if [ "$s" = 256 ]; then
    cp assets/icon-256.png "$d/gitkwaken.png"
  else
    "${IM[@]}" assets/icon-1024.png -filter Lanczos -resize "${s}x${s}" "PNG32:$d/gitkwaken.png"
  fi
done
pwsh -NoProfile -File scripts/third-party-licenses.ps1 \
  -Target "$TARGET" -Out "$STAGE/THIRD-PARTY-LICENSES.txt"
for f in gitkwaken README.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY-LICENSES.txt gitkwaken.desktop; do
  [ -s "$STAGE/$f" ] || { echo "$f missing from the package" >&2; exit 1; }
done

echo "==> $NAME.tar.gz"
tar -C "$DIST" --owner=0 --group=0 --numeric-owner --sort=name -czf "$DIST/$NAME.tar.gz" "$NAME"
(cd "$DIST" && sha256sum "$NAME.tar.gz" >"$NAME.tar.gz.sha256")
SHA="$(cut -d' ' -f1 "$DIST/$NAME.tar.gz.sha256")"

echo "==> PKGBUILD"
sed -e "s/^_ver=.*/_ver=$VERSION/" \
    -e "s/^sha256sums=.*/sha256sums=('$SHA')/" \
    -e '/^# This template keeps/d' \
    packaging/arch/PKGBUILD >"$DIST/PKGBUILD"
grep -q "^_ver=$VERSION\$" "$DIST/PKGBUILD" && grep -q "$SHA" "$DIST/PKGBUILD" ||
  { echo "PKGBUILD substitution failed" >&2; exit 1; }

tar -tzvf "$DIST/$NAME.tar.gz"
echo "==> done: $DIST/$NAME.tar.gz ($SHA)"
