#!/bin/bash
# merge.sh <pr> <pinned-head-sha> [accept-red=<run-id>[,<run-id>]]
# Runs a fresh GO and, only on GO, merges <pr> as a merge commit pinned to <pinned-head-sha>.
# The PAT lives in one shell variable of this process (sent to curl over stdin, never argv or a file)
# and is unset as soon as the merge call returns.
# Run from the repo checkout. MERGE_KIT (default /tmp/k) holds this session's files: merge-allow (the PR numbers
# the coordinator cleared, `#` starts a comment), watch-solo (merge-window.sh) and op-last.err.
set -u
D=$(cd "$(dirname "$0")" && pwd); K=${MERGE_KIT:-/tmp/k}
source "$D/opread.sh"
ALLOW=$(sed 's/#.*//' "$K/merge-allow" 2>/dev/null | tr -s ' \t\n' ' ' | sed 's/^ //; s/ $//')
[ -n "$ALLOW" ] || { echo "REFUSED: no merge list at $K/merge-allow"; exit 2; }
PR=$1; SHA=$2; FLAKE=""; OVER=""; PEND=""
for a in "${@:3}"; do case "$a" in accept-red=*) FLAKE=$a ;; over-serialize=*) OVER=${a#over-serialize=} ;; accept-pending=*) PEND=${a#accept-pending=} ;; *) echo "REFUSED: unknown argument $a"; exit 2;; esac; done
R=unarbos/arbos
case " $ALLOW " in *" $PR "*) ;; *) echo "REFUSED: #$PR is not in the coordinator's merge list ($ALLOW)"; exit 2;; esac
[ ${#SHA} -eq 40 ] || { echo "REFUSED: pin the full 40-char head sha"; exit 2; }

head=$(gh pr view "$PR" -R $R --json headRefOid --jq .headRefOid)
[ "$head" = "$SHA" ] || { echo "REFUSED: #$PR head is $head, not the pinned $SHA"; exit 2; }
base=$(gh pr view "$PR" -R $R --json baseRefName --jq .baseRefName)
[ "$base" = main ] || { echo "REFUSED: #$PR targets '$base', not main (a stacked PR is retargeted only after its parent lands)"; exit 2; }
# Every check must be terminal: a running Swift/xcframework lane cannot be inferred from mergeStateStatus.
# accept-pending=<run-id>[,<run-id>]: a coordinator ruling for ONE merge. Each pending check must be named by its run id;
# any other pending check, or one that cannot be traced to a run, still refuses.
pend=$(gh pr view "$PR" -R $R --json statusCheckRollup --jq '.statusCheckRollup[] | select((.status // "COMPLETED") != "COMPLETED" or (.state // "") == "PENDING" or (.state // "") == "EXPECTED")
  | "\(((.detailsUrl // .targetUrl // "") | [capture("runs/(?<id>[0-9]+)").id] | first) // "untraceable") \(.name // .context)"') \
  || { echo "REFUSED: cannot read the check rollup"; exit 2; }
pacc=",$PEND,"
while read -r id name; do
  [ -n "$id" ] || continue
  case "$pacc" in *",$id,"*) echo "    pending check accepted by run id (coordinator ruling, this merge only): $id $name" ;;
    *) echo "REFUSED: checks not terminal yet: run $id $name"; exit 2 ;; esac
done <<< "$pend"

out=$(bash "$D/merge-window.sh" go "$PR" 2>&1)
echo "$out" | grep -vE '^\s+(release:|dmg|feed \(|latest release|download/mac)'
v=$(echo "$out" | grep '^VERDICT' | tail -1)
# over-serialize=<fid>@<head8>: a coordinator ruling for one merge. It lifts exactly one NO-GO reason, and only
# when the sole releasable loop item is that item, at that head, still under a serialize-on-release flag.
if [ -n "$OVER" ] && [[ "$v" == *": NO-GO — a loop item the timer can release is gating" ]]; then
  rel=$(echo "$out" | grep -E '^\s+RELEASABLE ')
  [ "$(echo "$rel" | wc -l)" -eq 1 ] && [[ "$(echo "$rel" | awk '{print $2"@"$4" "$5}')" == "${OVER} serialize-on-release:"* ]] \
    || { echo "NOT MERGED: override ${OVER} does not match the releasable item(s): $rel"; exit 3; }
  echo "    NO-GO lifted by coordinator ruling for this merge only: $OVER under serialize-on-release"
else
  case "$v" in *": GO —"*) ;; *) echo "NOT MERGED: verdict is not GO"; exit 3;; esac
fi
# Every non-passing check run must be named by run id in accept-red=<id>[,<id>] (rule 9 flake, or a
# run that did not test this PR); anything else red, or a red check that cannot be traced to a run, stops the merge.
red=$(gh pr view "$PR" -R $R --json statusCheckRollup --jq '.statusCheckRollup[]
  | select((.conclusion // .state) as $c | ["FAILURE","CANCELLED","TIMED_OUT","ACTION_REQUIRED","STARTUP_FAILURE","ERROR","STALE"] | index($c))
  | "\(((.detailsUrl // .targetUrl // "") | [capture("runs/(?<id>[0-9]+)").id] | first) // "untraceable") \(.name // .context) \(.conclusion // .state)"') || { echo "NOT MERGED: cannot read the check rollup"; exit 3; }
acc=",${FLAKE#accept-red=},"
while read -r id name; do
  [ -n "$id" ] || continue
  case "$acc" in *",$id,"*) echo "    red check accepted by run id: $id $name" ;;
    *) echo "NOT MERGED: red check not accepted: run $id $name"; exit 3 ;; esac
done <<< "$red"
head=$(gh pr view "$PR" -R $R --json headRefOid --jq .headRefOid)
[ "$head" = "$SHA" ] || { echo "NOT MERGED: head moved to $head during GO"; exit 3; }

rl0=$(oprl)
TOKEN=$(opread 'op://Arbos/gefompyaklwbv2iuyyb4z5rvbm/credential') && [ -n "$TOKEN" ] \
  || { echo "NOT MERGED: PAT read failed at $(date -u +%FT%TZ) (cache off, so it reached 1Password; refused requests are not counted): $(head -c 300 "$K/op-last.err")"; exit 4; }
rl1=$(oprl)
echo "PAT read $(date -u +%FT%TZ): served by 1Password with the cache off. The client cannot see its cost in account requests; the account counter (shared by every lane) went from $rl0 to $rl1 across the read."
body=$(printf '{"sha":"%s","merge_method":"merge"}' "$SHA")
hdr=$(mktemp); chmod 600 "$hdr"
resp=$(printf 'header = "Authorization: Bearer %s"\nheader = "Accept: application/vnd.github+json"\nheader = "X-GitHub-Api-Version: 2022-11-28"\n' "$TOKEN" \
  | curl -sS -K - -D "$hdr" -X PUT --max-time 60 -d "$body" "https://api.github.com/repos/$R/pulls/$PR/merge")
unset TOKEN
echo "merge call $(date -u +%FT%TZ): $(head -1 "$hdr" | tr -d '\r')"
grep -i '^x-oauth-scopes:' "$hdr" | tr -d '\r' || echo "x-oauth-scopes: (header absent — fine-grained token)"
rm -f "$hdr"
echo "$resp" | jq -c '{merged, sha, message}' 2>/dev/null || echo "$resp"
