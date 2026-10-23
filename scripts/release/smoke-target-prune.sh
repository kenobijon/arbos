#!/usr/bin/env bash
# Keep the release lane's smoke target dir small between releases (#1439).
#
# The smoke step (`cargo test -p arbos-mac enter_tests`, release.yml) builds the debug profile into a
# persistent CARGO_TARGET_DIR on the self-hosted Mac. Every release bumps the workspace `version`, and
# the package version is part of cargo's metadata hash, so each release writes a NEW set of artifacts
# for every workspace crate (rlibs, the GPUI test binary with full debug info, fingerprints) and
# never removes the previous set: ~1.7 GB per release, 36 GB a day. Third-party deps keep their
# hash and are reused, so they must stay.
#
# This keeps, per workspace crate, only the newest artifact set (by mtime) under <target>/debug and
# removes every older one; if the dir is still above MAX_GB afterwards (something unforeseen), it is
# removed whole and the next smoke rebuilds it (~10 min).
#
#   scripts/release/smoke-target-prune.sh <target-dir> [max-gb]
set -euo pipefail
T="${1:?target dir}"
MAX_GB="${2:-25}"
D="$T/debug"
[[ -d "$D" ]] || exit 0

# GiB, rounded down; portable (GNU du has no -g, BSD du has it).
size_gb() { local k; k=$(du -sk "$1" 2>/dev/null | cut -f1 || true); echo $(( ${k:-0} / 1048576 )); }
before=$(size_gb "$T")

# Workspace crate names, as they appear in artifact file names (hyphens become underscores).
crates=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c 'import json,sys; print("\n".join(sorted({p["name"].replace("-","_") for p in json.load(sys.stdin)["packages"]})))')
[[ -n "$crates" ]] || { echo "smoke-target-prune: no workspace crates found; nothing pruned" >&2; exit 0; }

removed=0
while IFS= read -r c; do
  [[ -n "$c" ]] || continue
  # Hashes present for this crate, newest first by the mtime of their fingerprint dir (fallback: deps file).
  # (`|| true`: a crate with no artifacts yet makes `ls` fail, which must not end the script.)
  hashes=$(ls -1dt "$D/.fingerprint/$c-"[0-9a-f]* "$D/deps/$c-"[0-9a-f]* "$D/deps/lib$c-"[0-9a-f]* 2>/dev/null \
    | sed -E 's#.*/(lib)?'"$c"'-([0-9a-f]{16})([^/]*)$#\2#' | awk '!seen[$0]++' || true)
  keep=$(printf '%s\n' "$hashes" | head -1)
  [[ -n "$keep" ]] || continue
  while IFS= read -r h; do
    [[ -n "$h" && "$h" != "$keep" ]] || continue
    rm -rf "$D/.fingerprint/$c-$h" "$D/build/$c-$h" "$D/incremental/$c-$h"* \
           "$D/deps/$c-$h" "$D/deps/$c-$h."* "$D/deps/lib$c-$h."* "$D/$c-$h"* 2>/dev/null || true
    removed=$((removed + 1))
  done <<< "$hashes"
done <<< "$crates"

after=$(size_gb "$T")
echo "smoke-target-prune: removed $removed stale workspace artifact sets; $T ${before:-?} -> ${after:-?} GB"
if [[ "${after:-0}" -gt "$MAX_GB" ]]; then
  echo "smoke-target-prune: still ${after} GB (> ${MAX_GB}); removing the whole dir, the next smoke rebuilds it"
  rm -rf "$T"
fi
