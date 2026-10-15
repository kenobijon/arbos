#!/usr/bin/env bash
# Mac-host demo: launch Arbos in Glass, run one scripted turn against the
# relay, and record the window. Needs a paired device token (first run shows
# the pairing screen; pass ARBOS_RELAY_TOKEN or pair once by hand) and a
# running relay (ARBOS_RELAY_URL). Recording uses `screencapture -v`
# (macOS 10.15+); set ARBOS_RECORDER=ffmpeg to use avfoundation instead.
#
#   ARBOS_RELAY_URL=https://relay.example scripts/demo-mac.sh out/demo.mov
set -euo pipefail

OUT="${1:-$PWD/arbos-mac-demo.mov}"
SECS="${ARBOS_DEMO_SECONDS:-45}"
PROMPT="${ARBOS_DEMO_PROMPT:-In two sentences, what is Arbos and what does the rail on the left do?}"
RECORDER="${ARBOS_RECORDER:-screencapture}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "demo-mac.sh: macOS only. On Linux use Xvfb + ffmpeg x11grab (see README)." >&2
  exit 2
fi
: "${ARBOS_RELAY_URL:?set ARBOS_RELAY_URL}"

APP_BIN="${ARBOS_APP_BIN:-}"
if [[ -z "$APP_BIN" ]]; then
  if [[ -x "$ROOT/dist/Arbos.app/Contents/MacOS/arbos-mac" ]]; then
    APP_BIN="$ROOT/dist/Arbos.app/Contents/MacOS/arbos-mac"
  else
    cargo build --release -p arbos-mac
    APP_BIN="$ROOT/target/release/arbos-mac"
  fi
fi

echo "==> launching $APP_BIN (Glass)"
ARBOS_SURFACE=glass "$APP_BIN" &
APP_PID=$!
trap 'kill $APP_PID 2>/dev/null || true' EXIT
sleep 4

echo "==> recording $SECS s to $OUT"
mkdir -p "$(dirname "$OUT")"
case "$RECORDER" in
  screencapture)
    screencapture -v -V "$SECS" "$OUT" &
    ;;
  ffmpeg)
    ffmpeg -loglevel error -y -f avfoundation -framerate 30 -i "${ARBOS_AVF_DEVICE:-1}:none" -t "$SECS" \
      -c:v libx264 -preset veryfast -pix_fmt yuv420p "$OUT" &
    ;;
  *)
    echo "unknown ARBOS_RECORDER=$RECORDER" >&2; exit 2;;
esac
REC_PID=$!
sleep 2

# Scripted turn: focus the app, open a new tab, type, send, wait for the
# streamed reply, open the Details drawer on the reply.
osascript <<EOF
tell application "System Events"
  set frontmost of (first process whose unix id is $APP_PID) to true
  delay 0.8
  keystroke "t" using command down
  delay 0.8
  keystroke "$PROMPT"
  delay 0.4
  key code 36
end tell
EOF
sleep 18
osascript -e 'tell application "System Events" to keystroke "b" using {command down, shift down}'   # browser panel
sleep 3

wait "$REC_PID" || true
echo "==> saved $OUT"
