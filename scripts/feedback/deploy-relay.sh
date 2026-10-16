#!/usr/bin/env bash
# Deploy the relay on this server from a commit of unarbos/arbos main, always with the deploy
# script of that same commit (never a stale copy), env kept (keep-from-env in relay-server.sh),
# and never older than what runs now (relay-server.sh refuses; ALLOW_OLDER=1 to roll back).
#
#   sudo scripts/feedback/deploy-relay.sh            # main head
#   sudo scripts/feedback/deploy-relay.sh <sha|ref>  # a specific commit of main
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "run as root (sudo)" >&2; exit 1; }
REF="${1:-main}"
BUILD_USER="${BUILD_USER:-${SUDO_USER:-const}}"
REPO="/home/$BUILD_USER/arbos-build/repo"
if [[ ! -d "$REPO/.git" ]]; then
  sudo -u "$BUILD_USER" -H git clone -q https://github.com/unarbos/arbos.git "$REPO"
fi
sudo -u "$BUILD_USER" -H git -C "$REPO" fetch -q --tags --force origin "+refs/heads/*:refs/remotes/origin/*"
COMMIT=$(sudo -u "$BUILD_USER" -H git -C "$REPO" rev-parse -q --verify "origin/$REF^{commit}" 2>/dev/null \
      || sudo -u "$BUILD_USER" -H git -C "$REPO" rev-parse -q --verify "$REF^{commit}")
if ! sudo -u "$BUILD_USER" -H git -C "$REPO" merge-base --is-ancestor "$COMMIT" origin/main; then
  echo "refusing: $COMMIT is not on main" >&2
  exit 1
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/scripts/deploy"
# The deploy script, Caddyfile and helpers of the commit being deployed, copied out of the checkout
# (bash reads a script as it runs; the checkout below changes under it otherwise).
sudo -u "$BUILD_USER" -H git -C "$REPO" archive "$COMMIT" scripts/deploy site 2>/dev/null | tar -x -C "$TMP"
if [[ -f /etc/arbos/op.env && -z "${OP_SERVICE_ACCOUNT_TOKEN:-}" ]]; then
  set -a; . /etc/arbos/op.env; set +a
fi
export OP_SERVICE_ACCOUNT_TOKEN
exec bash "$TMP/scripts/deploy/relay-server.sh" --from-source "$COMMIT"
