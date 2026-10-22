#!/bin/bash
# One GUI app at a time on the EC2 Mac. The main display is shared by every worker's screenshot rig and
# by the release smoke; on 2026-10-22 03:08 two VNC clicks meant for one fixture app landed in another
# worker's app sitting at the same window position. Every script that launches an app on the display,
# clicks, types or captures takes this lock first and releases it when its app is gone.
#
#   gui-lock.sh acquire <owner> [timeout-s]   wait up to timeout-s (default 300) for the display; 0 = got it,
#                                              1 = still busy (owner and age printed), 2 = bad arguments
#   gui-lock.sh release <owner>                release if <owner> holds it (anyone may release a stale lock)
#   gui-lock.sh status                         print the holder, or "free"
#
# Lock file: /Users/Shared/arbos-run/gui.lock — "owner=<name> pid=<pid> since=<ISO>". A lock is stale when
# its pid (the calling script, $PPID — or GUI_LOCK_PID when set) is gone or it is older than GUI_LOCK_STALE_S
# (default 900 s); stale locks are taken over.
# Installed as /Users/Shared/arbos-run/gui-lock.sh (keep in step with scripts/runner/gui-lock.sh).
set -uo pipefail
LOCK=${GUI_LOCK_FILE:-/Users/Shared/arbos-run/gui.lock}
STALE_S=${GUI_LOCK_STALE_S:-900}
cmd=${1:-}; owner=${2:-}

holder() { sed -n 's/.*owner=\([^ ]*\).*/\1/p' "$LOCK" 2>/dev/null | head -n 1; }
holder_pid() { sed -n 's/.*pid=\([0-9]*\).*/\1/p' "$LOCK" 2>/dev/null | head -n 1; }
age_s() {
  local m
  m=$(stat -f %m "$LOCK" 2>/dev/null) || { echo 0; return; }
  echo $(( $(date +%s) - m ))
}
stale() {
  [ -f "$LOCK" ] || return 0
  local p; p=$(holder_pid)
  # ps, not kill -0: kill -0 answers EPERM for another user's live pid (ec2-user vs arbosgui), which would
  # make a live lock look stale and let it be taken over.
  if [ -n "$p" ] && ! ps -p "$p" >/dev/null 2>&1; then return 0; fi
  [ "$(age_s)" -gt "$STALE_S" ]
}
take() {
  mkdir -p "$(dirname "$LOCK")"
  printf 'owner=%s pid=%s since=%s\n' "$owner" "${GUI_LOCK_PID:-$PPID}" "$(date -u +%FT%TZ)" > "$LOCK.$$" && mv -f "$LOCK.$$" "$LOCK"
  chmod 666 "$LOCK" 2>/dev/null
}

case "$cmd" in
  acquire)
    [ -n "$owner" ] || { echo "usage: gui-lock.sh acquire <owner> [timeout-s]" >&2; exit 2; }
    timeout=${3:-300}; t0=$(date +%s)
    while :; do
      if [ ! -f "$LOCK" ] || stale || [ "$(holder)" = "$owner" ]; then
        take; echo "gui: locked by $owner"; exit 0
      fi
      if [ $(( $(date +%s) - t0 )) -ge "$timeout" ]; then
        echo "gui: busy — held by $(holder) (pid $(holder_pid), $(age_s)s)"; exit 1
      fi
      sleep 5
    done ;;
  release)
    [ -n "$owner" ] || { echo "usage: gui-lock.sh release <owner>" >&2; exit 2; }
    if [ ! -f "$LOCK" ]; then exit 0; fi
    if [ "$(holder)" = "$owner" ] || stale; then rm -f "$LOCK"; echo "gui: released by $owner"; exit 0; fi
    echo "gui: not yours — held by $(holder)"; exit 1 ;;
  status)
    if [ -f "$LOCK" ]; then
      if stale; then echo "stale: $(cat "$LOCK") ($(age_s)s)"; else echo "held: $(cat "$LOCK") ($(age_s)s)"; fi
    else echo free; fi ;;
  *)
    echo "usage: gui-lock.sh acquire <owner> [timeout-s] | release <owner> | status" >&2; exit 2 ;;
esac
