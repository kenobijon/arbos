#!/usr/bin/env bash
# Land `cursor/m1-integration` on unarbos/arbos main WITHOUT history: back up main, then make
# main point at the integration head (force push with a lease on the backed-up sha).
#
#   CONFIRM=yes scripts/landing/land-force.sh
#
# Without CONFIRM=yes it only prints the plan. See common.sh for the knobs.
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=common.sh
. scripts/landing/common.sh

preflight
plan "force" "force-push $SOURCE_BRANCH as main (--force-with-lease on the current main sha)"
require_confirm

backup_main

say "force-pushing main"
# The lease makes the push fail if main moved since preflight read it.
git push --force-with-lease="refs/heads/main:$MAIN_SHA" "$CANONICAL_REMOTE" "$SOURCE_SHA:refs/heads/main"
printf '\nLanded. main = %s (was %s), backup = %s/%s\n' "$SOURCE_SHA" "$MAIN_SHA" "$CANONICAL_REMOTE" "$BACKUP_BRANCH"
