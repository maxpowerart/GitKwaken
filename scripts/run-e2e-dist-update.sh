#!/usr/bin/env bash
# End-to-end self-update of the SHIPPED build through its compiled-in channel.
# Copies dist/GitKwaken-<ver>-x64 to a temp install dir, temporarily replaces
# dist/update.json with a fake 99.0.0 release (restored on exit), lets the
# installed exe find it on its own (no GITKWAKEN_UPDATE_URL), clicks
# "Update" and "Restart now", and checks that the exe was replaced and the
# old one is removed on the next start.
# Works for builds made without a GitHub repo (channel = file:///…/dist/update.json).
# Usage: scripts/run-e2e-dist-update.sh [version]   (default: version in dist/update.json)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
VER="${1:-$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$DIST/update.json" | head -1)}"
SRC="$DIST/GitKwaken-$VER-x64"
[ -x "$SRC/gitkwaken.exe" ] || { echo "no $SRC/gitkwaken.exe (run packaging/build-release.ps1)"; exit 2; }
grep -aq "file:///$(cygpath -m "$DIST")/update.json" "$SRC/gitkwaken.exe" \
  || { echo "SKIP: this build's channel is not the local dist/update.json"; exit 0; }
W="$(cygpath -u "$TEMP")/gk-dist-update"
PW="$(cygpath -w "$W")"
rm -rf "$W"; mkdir -p "$W/install" "$W/pkg"
cp -r "$SRC"/* "$W/install/"
cp "$DIST/update.json" "$W/update.json.orig"
restore() { cp "$W/update.json.orig" "$DIST/update.json"; rm -f "$DIST/GitKwaken-99.0.0-x64.zip"; }
trap restore EXIT
cp "$SRC/gitkwaken.exe" "$W/pkg/gitkwaken.exe"
printf 'GK-DIST-UPDATE-MARKER' >> "$W/pkg/gitkwaken.exe"   # distinguishable "new" build
powershell -NoProfile -Command "Compress-Archive -Force -Path '$PW\\pkg\\gitkwaken.exe' -DestinationPath '$(cygpath -w "$DIST")\\GitKwaken-99.0.0-x64.zip'"
sha=$(sha256sum "$DIST/GitKwaken-99.0.0-x64.zip" | cut -d' ' -f1)
want=$(sha256sum "$W/pkg/gitkwaken.exe" | cut -d' ' -f1)
printf '{"version":"99.0.0","notes":"dist channel e2e","url":"./GitKwaken-99.0.0-x64.zip","sha256":"%s"}' "$sha" > "$DIST/update.json"
export GIT_CEILING_DIRECTORIES="$(cygpath -m "$W")"
bash "$ROOT/scripts/make-demo-repo.sh" "$W/repo" >/dev/null
M="$(cygpath -m "$W")"
cat > "$W/u.script" <<EOF
wait 4000
click 1336 14
wait 8000
shot $M/offer.png
click 1094 14
wait 3000
click 1094 14
wait 3000
EOF
GK_DX11_INPUT_SCRIPT="$M/u.script" GITKWAKEN_NO_RELAUNCH=1 GITKWAKEN_CONFIG_DIR="$PW\\cfg" \
  timeout 90 "$W/install/gitkwaken.exe" "$M/repo" || true
got=$(sha256sum "$W/install/gitkwaken.exe" | cut -d' ' -f1)
[ "$got" = "$want" ] || { echo "FAIL dist-update: executable not replaced"; exit 1; }
[ -f "$W/install/gitkwaken.exe.old" ] || { echo "FAIL dist-update: no .old kept"; exit 1; }
GITKWAKEN_NO_UPDATE_CHECK=1 GK_DX11_SCREENSHOT="$M/after.png" GK_DX11_SCREENSHOT_AFTER_MS=2000 GK_DX11_SCREENSHOT_EXIT=1 \
  GITKWAKEN_CONFIG_DIR="$PW\\cfg" timeout 60 "$W/install/gitkwaken.exe" "$M/repo" || true
[ ! -e "$W/install/gitkwaken.exe.old" ] || { echo "FAIL dist-update: .old not cleaned up"; exit 1; }
echo "PASS dist-update ($VER via compiled channel file:///…/dist/update.json; screenshots in $M)"
