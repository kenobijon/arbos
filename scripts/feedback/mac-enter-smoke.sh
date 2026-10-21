#!/bin/bash
# Type-and-Enter smoke for the built Mac app (P0 #1208: every Enter crashed v0.6.89–v0.6.92).
#   mac-enter-smoke.sh <arbos-mac binary>
# Launches the binary with the fixture, brings it to front, ⌘T (new chat, composer focused), types a
# line, presses Enter and fails when the process is gone afterwards. It first proves it can drive the
# app at all (⌘A ⌘C → the typed text must be on the clipboard); when it cannot — the GUI session
# lacks Accessibility / Automation for this process — it says so and exits 0 with a warning, so the
# deterministic `cargo test -p arbos-mac enter_tests` stays the hard gate and this one adds coverage
# where it can run. Exit 0 = alive (or could not drive), 1 = CRASHED, 2 = could not launch.
set -uo pipefail
BIN=${1:?arbos-mac binary}
export ARBOS_FIXTURE=1 ARBOS_FIXTURE_OPEN=${ARBOS_FIXTURE_OPEN:-Affine} ARBOS_SURFACE=matte ARBOS_UPDATE_AUTO=0
LOG=$(mktemp -t enter-smoke)
TEXT="enter smoke $(date +%s)"
"$BIN" >"$LOG" 2>&1 &
PID=$!
sleep 8
if ! kill -0 "$PID" 2>/dev/null; then echo "== enter smoke: the app did not stay up after launch"; tail -n 20 "$LOG"; exit 2; fi
cannot_drive() {
  echo "::warning title=enter smoke::could not drive the app ($1: this session lacks Accessibility/Automation for the runner); the GPUI enter_tests remain the gate"
  echo "== enter smoke: could not drive the app ($1); skipped"
  kill "$PID" 2>/dev/null; exit 0
}
# osascript blocks forever on a TCC prompt nobody can answer (v0.6.96 release, 22:31: `keystroke`
# hung the job until the process was killed by hand) — every call gets a 10-second cap.
key() {
  osascript -e "tell application \"System Events\" to $1" >/dev/null 2>&1 &
  local p=$! i
  for ((i = 0; i < 100; i++)); do
    kill -0 "$p" 2>/dev/null || { wait "$p" 2>/dev/null; return $?; }
    sleep 0.1
  done
  kill "$p" 2>/dev/null; wait "$p" 2>/dev/null
  return 124
}
key "count processes" || cannot_drive "System Events did not answer within 10 s"
key "tell (first process whose unix id is $PID) to set frontmost to true"; sleep 1
key 'keystroke "t" using command down' || cannot_drive "keystroke blocked"
sleep 1
printf '' | pbcopy
key "keystroke \"$TEXT\"" || cannot_drive "keystroke blocked"
sleep 1
key 'keystroke "a" using command down'; sleep 0.3
key 'keystroke "c" using command down'; sleep 1
if [[ "$(pbpaste 2>/dev/null)" != *"$TEXT"* ]]; then
  cannot_drive "keystrokes did not reach the composer"
fi
key 'key code 36'   # Enter
sleep 4
if kill -0 "$PID" 2>/dev/null; then
  echo "== enter smoke: ALIVE after typing + Enter"
  kill "$PID" 2>/dev/null; sleep 1; kill -9 "$PID" 2>/dev/null
  exit 0
fi
wait "$PID"; code=$?
echo "::error title=enter smoke::the Mac app CRASHED after typing + Enter (exit $code)"
grep -i "panic\|thread 'main'\|already being updated" "$LOG" | head -5
exit 1
