#!/usr/bin/env bash
# Launch smoke test for the Linux build (eframe/wgpu host of gk-dx11).
#
#   scripts/linux-smoke.sh <gitkwaken binary> [screenshot.png]
#
# Without a display it re-runs itself under xvfb-run (X11, 1280x800x24) with
# Mesa's software rasterizer, so it works on headless CI runners. Checks:
#   1. the app opens a demo repository (scripts/make-demo-repo.sh) and renders
#      its first frame with history rows (GITKWAKEN_STARTUP_LOG milestones);
#   2. it stays alive for 10 s after that;
#   3. closing the window the way a window manager does (WM_DELETE_WINDOW)
#      exits with status 0 and saves settings.toml with the open tab;
#   4. a second start restores that tab, stays alive 5 s, and SIGTERM ends it
#      within 10 s (status 143, no panic, no core dump).
# Needs: xvfb-run (Xvfb), xdotool, python3 (ctypes + libX11), git; Mesa GL/EGL
# or a Vulkan ICD (lavapipe). Optional: ImageMagick (`import`) for a screenshot.
# Everything the app writes stays in a temporary directory.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <gitkwaken binary> [screenshot.png]" >&2
  exit 2
fi
BIN="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
SHOT="${2:-}"
[ -n "$SHOT" ] && SHOT="$(cd "$(dirname "$SHOT")" && pwd)/$(basename "$SHOT")"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -z "${GK_SMOKE_INNER:-}" ]; then
  # Always a private X server: never touch the developer's session.
  unset WAYLAND_DISPLAY
  export GK_SMOKE_INNER=1 LIBGL_ALWAYS_SOFTWARE=1
  exec xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp" bash "$0" "$BIN" ${SHOT:+"$SHOT"}
fi

W="$(mktemp -d)"
APP_PID=""
cleanup() {
  [ -n "$APP_PID" ] && kill -9 "$APP_PID" 2>/dev/null || true
  rm -rf "$W"
}
trap cleanup EXIT

export GITKWAKEN_CONFIG_DIR="$W/config"
export XDG_CONFIG_HOME="$W/xdg-config" XDG_CACHE_HOME="$W/xdg-cache" XDG_DATA_HOME="$W/xdg-data"
export GITKWAKEN_NO_UPDATE_CHECK=1 GK_DX11_PRINT_BACKEND=1
# The app and git must never walk up into whatever contains the work dir.
export GIT_CEILING_DIRECTORIES="$W"
mkdir -p "$GITKWAKEN_CONFIG_DIR"

bash "$ROOT/scripts/make-demo-repo.sh" "$W/repo" >/dev/null

fail() {
  echo "FAIL: $*" >&2
  for f in "$W"/run*.stderr "$W"/startup*.log; do
    [ -f "$f" ] && { echo "--- $(basename "$f")"; cat "$f"; } >&2
  done
  exit 1
}

# start <run name> [args...]: launches the app in the background.
start() {
  local name="$1"
  shift
  GITKWAKEN_STARTUP_LOG="$W/startup-$name.log" "$BIN" "$@" \
    >"$W/run-$name.stdout" 2>"$W/run-$name.stderr" &
  APP_PID=$!
}

# Running and not a zombie (kill -0 also succeeds for an exited, unreaped child).
alive() {
  local state
  state="$(sed 's/.*) //; s/ .*//' "/proc/$APP_PID/stat" 2>/dev/null || true)"
  [ -n "$state" ] && [ "$state" != Z ]
}

# wait_exit <seconds>: waits for the app to exit; sets STATUS to its exit
# status, or to "timeout".
wait_exit() {
  local i
  STATUS=timeout
  for ((i = 0; i < $1 * 10; i++)); do
    if ! alive; then
      STATUS=0
      wait "$APP_PID" || STATUS=$?
      APP_PID=""
      return 0
    fi
    sleep 0.1
  done
}

# wait_mark <run name> <label prefix> <seconds>
wait_mark() {
  local i
  for ((i = 0; i < $3 * 10; i++)); do
    grep -q "^$2" "$W/startup-$1.log" 2>/dev/null && return 0
    if ! alive; then
      wait_exit 1
      fail "$1: the app exited before '$2' (status $STATUS)"
    fi
    sleep 0.1
  done
  fail "$1: no '$2' within $3 s"
}

# stay_alive <run name> <seconds>
stay_alive() {
  local i
  for ((i = 0; i < $2; i++)); do
    if ! alive; then
      wait_exit 1
      fail "$1: the app exited after $i s (status $STATUS)"
    fi
    sleep 1
  done
}

window_id() {
  xdotool search --onlyvisible --name '^GitKwaken$' 2>/dev/null | head -n 1
}

# Sends WM_DELETE_WINDOW, as a window manager's close button does.
wm_close() {
  python3 - "$1" <<'EOF'
import ctypes, sys
x = ctypes.CDLL("libX11.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
x.XOpenDisplay.argtypes = [ctypes.c_char_p]
x.XInternAtom.restype = ctypes.c_ulong
x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
x.XFlush.argtypes = [ctypes.c_void_p]
x.XCloseDisplay.argtypes = [ctypes.c_void_p]

class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p), ("window", ctypes.c_ulong),
                ("message_type", ctypes.c_ulong), ("format", ctypes.c_int),
                ("l", ctypes.c_long * 5)]

class XEvent(ctypes.Union):
    _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]

d = x.XOpenDisplay(None)
if not d:
    sys.exit("cannot open display")
win = int(sys.argv[1])
e = XEvent()
e.xclient.type = 33  # ClientMessage
e.xclient.window = win
e.xclient.message_type = x.XInternAtom(d, b"WM_PROTOCOLS", 0)
e.xclient.format = 32
e.xclient.l[0] = x.XInternAtom(d, b"WM_DELETE_WINDOW", 0)
e.xclient.l[1] = 0  # CurrentTime
if not x.XSendEvent(d, win, 0, 0, ctypes.byref(e)):
    sys.exit("XSendEvent failed")
x.XFlush(d)
x.XCloseDisplay(d)
EOF
}

no_panic() {
  if grep -q "panicked" "$W/run-$1.stderr"; then
    fail "$1: panic on stderr"
  fi
}

echo "binary:  $BIN"
echo "display: $DISPLAY"

# --- run 1: open the demo repository, close like a window manager ----------
start first "$W/repo"
wait_mark first first_frame 60
wait_mark first history_rows 30
echo "first frame: $(grep '^first_frame ' "$W/startup-first.log" | cut -d' ' -f2) ms"
stay_alive first 10
grep "gk-dx11 backend" "$W/run-first.stderr" || echo "(renderer not reported)"
if [ -n "$SHOT" ] && command -v import >/dev/null; then
  import -window root "$SHOT" && echo "screenshot: $SHOT"
fi
WID="$(window_id)"
[ -n "$WID" ] || fail "first: no visible window named GitKwaken"
wm_close "$WID" || fail "first: could not send WM_DELETE_WINDOW"
wait_exit 10
[ "$STATUS" = 0 ] || fail "first: exit status after WM close: $STATUS (expected 0)"
no_panic first
[ -f "$GITKWAKEN_CONFIG_DIR/settings.toml" ] || fail "first: settings.toml not written on close"
grep -qF "$W/repo" "$GITKWAKEN_CONFIG_DIR/settings.toml" || fail "first: the open tab was not saved"
echo "run 1 ok: rendered, alive 10 s, WM close -> exit 0, settings saved"

# --- run 2: restore the saved tab, then SIGTERM -----------------------------
start second
wait_mark second first_frame 60
# history_rows or history_rows_from_cache: only marked when a tab is open.
wait_mark second history_rows 30
stay_alive second 5
kill -TERM "$APP_PID"
wait_exit 10
case "$STATUS" in
  143 | 0) ;;
  timeout) fail "second: still running 10 s after SIGTERM" ;;
  *) fail "second: exit status after SIGTERM: $STATUS (expected 143)" ;;
esac
no_panic second
echo "run 2 ok: tab restored, alive 5 s, SIGTERM -> exit $STATUS"
echo "smoke test passed"
