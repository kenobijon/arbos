#!/bin/bash
# iOS pre-merge gate, run on the EC2 Mac by the feedback poller over ssh:
#   ios-gate.sh <branch> <sha>
# Fetches the branch into ~/arbos-gate (its own clone, never the audit clone), checks out <sha>
# detached, and builds the iPhone app (Debug, generic simulator, no signing). Prints the compiler
# errors; exit 0 = compiles, 1 = does not compile, 2 = infrastructure trouble (fetch/checkout).
set -uo pipefail
branch=$1; sha=$2
cd ~/arbos-gate || exit 2
git fetch -q origin "+refs/heads/${branch}:refs/remotes/origin/${branch}" || { echo "fetch of ${branch} failed"; exit 2; }
git checkout -q --detach "$sha" || { echo "checkout of ${sha} failed"; exit 2; }
cd apps/ios/Arbos || exit 2
log=$(mktemp -t ios-gate)
xcodebuild -project Arbos.xcodeproj -scheme Arbos -configuration Debug \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath "$HOME/arbos-gate-dd" \
  CODE_SIGNING_ALLOWED=NO build >"$log" 2>&1
rc=$?
grep -E "error:|BUILD (SUCCEEDED|FAILED)" "$log" | grep -v "warning:" | sed 's#/Users/[^/]*/arbos-gate/##' | head -40
rm -f "$log"
[ "$rc" -eq 0 ] && exit 0 || exit 1
