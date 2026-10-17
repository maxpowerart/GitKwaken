#!/usr/bin/env bash
# End-to-end self-update test: serves a fake newer release from a local HTTP
# server, drives the app to click "Update to x" and "Restart now", and checks
# that the executable was replaced and the old one is cleaned up next start.
# Usage: scripts/run-e2e-update.sh <gitkwaken.exe> <gk-bench.exe>
set -euo pipefail
EXE="$1"; BENCH="$2"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
W="$ROOT/bench-data/e2e-update"
export GIT_CEILING_DIRECTORIES="$(cygpath -m "$ROOT/bench-data")"
rm -rf "$W"; mkdir -p "$W/install" "$W/srv" "$W/pkg"
cp "$EXE" "$W/install/gitkwaken.exe"
cp "$EXE" "$W/pkg/gitkwaken.exe"
printf 'GK-UPDATE-MARKER' >> "$W/pkg/gitkwaken.exe"   # makes the new build distinguishable
PS_W="$(cygpath -w "$W")"
powershell -NoProfile -Command "Compress-Archive -Path '$PS_W\\pkg\\gitkwaken.exe' -DestinationPath '$PS_W\\srv\\GitKwaken-99.0.0-x64.zip'"
sha=$(sha256sum "$W/srv/GitKwaken-99.0.0-x64.zip" | cut -d' ' -f1)
want=$(sha256sum "$W/pkg/gitkwaken.exe" | cut -d' ' -f1)
printf '{"version":"99.0.0","notes":"e2e","url":"http://127.0.0.1:8799/GitKwaken-99.0.0-x64.zip","sha256":"%s"}' "$sha" > "$W/srv/update.json"
"$BENCH" serve "$W/srv" 8799 & SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
bash "$ROOT/scripts/make-demo-repo.sh" "$W/repo" >/dev/null
cat > "$W/update.script" <<'EOF'
# Help menu, update slot (row 2, same place in every state): "Check for updates" →
# "Install update 99.0.0" → (downloading, disabled) → "Restart to finish update 99.0.0".
# "Install update" opens the changelog dialog; Enter there installs. Escape closes the menu
# when the entry was disabled. Extra rounds are no-ops.
wait 5000
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
click 282 12
wait 400
click 333 59.2
wait 500
key Enter
wait 200
key Escape
wait 3500
EOF
GK_DX11_INPUT_SCRIPT="$(cygpath -m "$W/update.script")" GITKWAKEN_NO_RELAUNCH=1 \
GITKWAKEN_UPDATE_URL=http://127.0.0.1:8799/update.json \
GITKWAKEN_CONFIG_DIR="$PS_W\\cfg" timeout 90 "$W/install/gitkwaken.exe" "$W/repo" || true
got=$(sha256sum "$W/install/gitkwaken.exe" | cut -d' ' -f1)
[ "$got" = "$want" ] || { echo "FAIL update: executable not replaced"; exit 1; }
[ -f "$W/install/gitkwaken.exe.old" ] || { echo "FAIL update: no .old kept"; exit 1; }
GK_DX11_SCREENSHOT="$(cygpath -m "$W/after.png")" GK_DX11_SCREENSHOT_AFTER_MS=2000 GK_DX11_SCREENSHOT_EXIT=1 \
GITKWAKEN_NO_UPDATE_CHECK=1 GITKWAKEN_CONFIG_DIR="$PS_W\\cfg" timeout 60 "$W/install/gitkwaken.exe" "$W/repo" || true
[ ! -f "$W/install/gitkwaken.exe.old" ] || { echo "FAIL update: .old not cleaned up"; exit 1; }
echo "PASS self-update (download, SHA-256 check, replace on exit, cleanup)"
