#!/usr/bin/env bash
# Land `cursor/m1-integration` on unarbos/arbos main WITH history: back up main, create a merge
# commit whose tree is exactly the integration tree and whose parents are (old main, integration),
# push it to main. Old main stays reachable through the merge and through the backup branch.
#
#   CONFIRM=yes scripts/landing/land-merge.sh
#
# Without CONFIRM=yes it only prints the plan. See common.sh for the knobs.
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=common.sh
. scripts/landing/common.sh

preflight
plan "merge" "merge commit (tree = $SOURCE_BRANCH, parents = old main + $SOURCE_BRANCH), push main as a fast-forward"
require_confirm

backup_main

say "building the merge commit on a temporary branch"
TMP_BRANCH="landing/merge-$(date -u +%Y%m%d-%H%M%S)"
ORIG_BRANCH=$(git rev-parse --abbrev-ref HEAD)
git checkout -q -b "$TMP_BRANCH" "$SOURCE_SHA"
# `-s ours` from the integration side: the result keeps our tree byte for byte and records old main
# as the second parent, so `git log main` still walks the whole previous history.
git merge -s ours --no-ff --allow-unrelated-histories -m "Land Arbos v2 (relay + agentd + Mac app): merge $SOURCE_BRANCH

The tree is the Arbos v2 monorepo. The previous main (app/, fx/, ios/) stays reachable here as the
second parent and as branch $BACKUP_BRANCH." "$MAIN_SHA"
MERGE_SHA=$(git rev-parse HEAD)
say "merge commit $MERGE_SHA"
[ -z "$(git diff --stat "$SOURCE_SHA" "$MERGE_SHA")" ] || die "merge tree differs from $SOURCE_BRANCH; refusing"

say "pushing main (fast-forward from $MAIN_SHA)"
git push "$CANONICAL_REMOTE" "$MERGE_SHA:refs/heads/main"
git checkout -q "$ORIG_BRANCH"
git branch -D "$TMP_BRANCH" >/dev/null
printf '\nLanded. main = %s, backup = %s/%s\n' "$MERGE_SHA" "$CANONICAL_REMOTE" "$BACKUP_BRANCH"
