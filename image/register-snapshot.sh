#!/usr/bin/env bash
# Register an image as a Daytona snapshot and wait until it is active (sandbox-image.md §6 step 6).
#
#   image/register-snapshot.sh <snapshot-name> <image-ref> [cpu] [memory-gb] [disk-gb]
#
# Env: DAYTONA_API_KEY (required), DAYTONA_API_URL (default https://app.daytona.io/api).
# For a private GHCR image also set GHCR_USER + GHCR_READ_TOKEN: the script registers GHCR as a Daytona
# registry once (name "ghcr") so runners can pull.
#
# Without CI (repo not published), build on a Docker-capable host or a Daytona Docker-in-Docker sandbox,
# push to Daytona's transient registry, and call this script with that image ref:
#   creds=$(curl -s -H "Authorization: Bearer $DAYTONA_API_KEY" \
#     "$DAYTONA_API_URL/docker-registry/registry-push-access?snapshotName=<name>")   # username, secret, registryUrl, project
#   docker login cr.app.daytona.io -u <username> -p <secret>
#   docker tag arbos-agent:<v> cr.app.daytona.io/<project>/arbos-agent:<v> && docker push cr.app.daytona.io/<project>/arbos-agent:<v>
#   image/register-snapshot.sh arbos-agent-<v> cr.app.daytona.io/<project>/arbos-agent:<v>
set -euo pipefail

NAME=${1:?snapshot name}
IMAGE=${2:?image ref}
CPU=${3:-2}
MEM=${4:-4}
DISK=${5:-10}
API=${DAYTONA_API_URL:-https://app.daytona.io/api}
: "${DAYTONA_API_KEY:?DAYTONA_API_KEY is required}"
auth=(-H "Authorization: Bearer $DAYTONA_API_KEY" -H "content-type: application/json")

if [[ "$IMAGE" == ghcr.io/* && -n "${GHCR_READ_TOKEN:-}" ]]; then
  if ! curl -fsS "${auth[@]}" "$API/docker-registry" | jq -e '.[] | select(.name == "ghcr")' >/dev/null; then
    curl -fsS "${auth[@]}" -X POST "$API/docker-registry" \
      -d "$(jq -n --arg u "${GHCR_USER:-token}" --arg p "$GHCR_READ_TOKEN" '{name:"ghcr", url:"https://ghcr.io", username:$u, password:$p, project:""}')" >/dev/null
    echo "registered GHCR as a Daytona registry"
  fi
fi

existing=$(curl -fsS "${auth[@]}" "$API/snapshots/$NAME" 2>/dev/null || true)
if [ -n "$existing" ] && [ "$(echo "$existing" | jq -r .state)" = "active" ] && [ "$(echo "$existing" | jq -r .imageName)" = "$IMAGE" ]; then
  echo "snapshot $NAME already active for $IMAGE"; exit 0
fi
if [ -n "$existing" ]; then
  id=$(echo "$existing" | jq -r .id)
  curl -fsS "${auth[@]}" -X DELETE "$API/snapshots/$id" >/dev/null && echo "removed stale snapshot $NAME ($id)"
fi

created=$(curl -fsS "${auth[@]}" -X POST "$API/snapshots" \
  -d "$(jq -n --arg n "$NAME" --arg i "$IMAGE" --argjson c "$CPU" --argjson m "$MEM" --argjson d "$DISK" '{name:$n, imageName:$i, cpu:$c, memory:$m, disk:$d}')")
id=$(echo "$created" | jq -r .id)
echo "snapshot $NAME = $id ($(echo "$created" | jq -r .state))"

for _ in $(seq 1 180); do
  s=$(curl -fsS "${auth[@]}" "$API/snapshots/$id")
  state=$(echo "$s" | jq -r .state)
  case "$state" in
    active) echo "snapshot $NAME active (size $(echo "$s" | jq -r .size) GB)"; exit 0 ;;
    error|build_failed) echo "snapshot $NAME failed: $(echo "$s" | jq -r .errorReason)"; exit 1 ;;
  esac
  sleep 10
done
echo "timed out waiting for snapshot $NAME"; exit 1
