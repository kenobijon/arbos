#!/bin/bash
# Shared GUI command runner for the EC2 Mac rigs (Terminal identity: the one TCC client with Screen Recording,
# so captures work here; clicks go through the `ax` helper to ax-runner.sh). Executes
# /Users/Shared/arbos-run/cmd-<rig>-<step>.sh; protocol unchanged: stdout+stderr → cmd-….log, exit code →
# cmd-….rc, then the command file is renamed cmd-….done.
#
# v2 (2026-10-27 17:4x, coordinator): commands from DIFFERENT rigs run concurrently — one lane per rig, in
# order inside the lane, at most MAX_PAR lanes at once. Before, one rig command waiting 45 min for a heavy slot
# held every other rig's shots (11 queued; Jacob's evidence shots stuck). A command that needs a slot still
# waits inside arbos-heavy-slot.sh; it no longer blocks the shots of already-staged apps on other rigs.
# The rig is the first dash-separated token after `cmd-` (cmd-pd-before.sh → pd; cmd-chart-ios9.sh → chart).
# A command is claimed by renaming it to cmd-….sh.running (atomic), so a second runner instance never runs it
# too; a cmd-….sh that an older runner is already executing (`bash cmd-X.sh` in ps) is left alone.
#
# MUST be started from Terminal (the identity carries the Screen Recording grant):
#   cd /Users/Shared/arbos-run && exec bash runner.sh
cd /Users/Shared/arbos-run || exit 1
MAX_PAR=${RUNNER_MAX_PAR:-4}
export PATH=/Users/Shared/arbos-run/axbin:$PATH
echo "runner v2 up pid $$ user $(id -un) $(date -u) max_par=$MAX_PAR" > runner.status
declare -A lane_pid
rig_of() { local b=${1#cmd-}; b=${b%%.*}; echo "${b%%-*}"; }
run_one() {  # run_one <cmd-X.sh.running> <base>
  local f=$1 base=$2 rc
  bash "$f" > "$base.log" 2>&1 < /dev/null; rc=$?
  echo "$rc" > "$base.rc"; mv "$f" "$base.done"; rm -f "$base.seen"
}
while true; do
  for rig in "${!lane_pid[@]}"; do
    kill -0 "${lane_pid[$rig]}" 2>/dev/null || unset "lane_pid[$rig]"
  done
  # FIFO by enqueue time (file birth time), not by name: a rig that names its command cmd-00… does not jump
  # the queue; the rename is logged once for visibility (18:00 Oct 27), nothing else happens to it.
  for f in $(stat -f '%B %N' cmd-*.sh 2>/dev/null | sort -n | awk '{print $2}'); do
    [ -f "$f" ] || continue
    rig=$(rig_of "$f"); base=${f%.sh}
    if [ ! -f "$base.seen" ]; then
      : > "$base.seen"
      older=$(stat -f '%B %N' cmd-*.sh 2>/dev/null | awk -v b="$(stat -f %B "$f")" -v me="$f" '$1 < b && $2 > me' | wc -l | tr -d ' ')
      [ "$older" -gt 0 ] && echo "$(date -u +%FT%TZ) note: $f (rig $rig) sorts before $older older command(s) by name — FIFO is by enqueue time, it waits its turn" >> runner.status
    fi
    [ -n "${lane_pid[$rig]}" ] && continue                     # this rig's lane is busy: keep its order
    [ "${#lane_pid[@]}" -ge "$MAX_PAR" ] && break              # enough lanes running
    pgrep -f "bash $f\$" >/dev/null 2>&1 && continue           # an older runner is on it
    mv "$f" "$f.running" 2>/dev/null || continue               # claimed
    run_one "$f.running" "$base" &
    lane_pid[$rig]=$!
    echo "$(date -u +%FT%TZ) lane $rig ← $f (pid $!, ${#lane_pid[@]} lane(s) busy)" >> runner.status
  done
  sleep 1
done
