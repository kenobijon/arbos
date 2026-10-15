#!/usr/bin/env bash
# Poll the 1Password item "Arbos GitHub OAuth app" (vault Arbos) for the GitHub OAuth client id and
# secret; when a value differs from /etc/arbos/relay.env, write it and restart the relay.
# Installed as /usr/local/bin/arbos-oauth-secret-watch and run by arbos-oauth-secret-watch.timer
# every 2 minutes. Needs OP_SERVICE_ACCOUNT_TOKEN, read from /etc/arbos/op.env (root 0600).
set -euo pipefail
ENV_FILE=/etc/arbos/relay.env
ITEM="${ARBOS_OAUTH_OP_ITEM:-Arbos GitHub OAuth app}"
[[ -f /etc/arbos/op.env ]] && set -a && . /etc/arbos/op.env && set +a
: "${OP_SERVICE_ACCOUNT_TOKEN:?no OP_SERVICE_ACCOUNT_TOKEN (put it in /etc/arbos/op.env)}"
export PATH=/usr/local/bin:$PATH

id=$(op read "op://Arbos/$ITEM/ARBOS_GITHUB_CLIENT_ID" 2>/dev/null || true)
secret=$(op read "op://Arbos/$ITEM/ARBOS_GITHUB_CLIENT_SECRET" 2>/dev/null || op read "op://Arbos/$ITEM/credential" 2>/dev/null || true)
[[ -n "$id" ]] || exit 0

setvar() { # setvar KEY VALUE -> 1 if changed
  local k=$1 v=$2 cur
  cur=$(sed -n "s/^$k=//p" "$ENV_FILE" | head -1)
  [[ "$cur" == "$v" ]] && return 1
  if grep -q "^$k=" "$ENV_FILE"; then sed -i "s|^$k=.*|$k=$v|" "$ENV_FILE"; else printf '%s=%s\n' "$k" "$v" >> "$ENV_FILE"; fi
  return 0
}
changed=0
setvar ARBOS_GITHUB_CLIENT_ID "$id" && changed=1
setvar ARBOS_GITHUB_OAUTH_CLIENT_ID "$id" && changed=1
if [[ -n "$secret" ]]; then
  setvar ARBOS_GITHUB_CLIENT_SECRET "$secret" && changed=1
  setvar ARBOS_GITHUB_OAUTH_CLIENT_SECRET "$secret" && changed=1
fi
if [[ $changed -eq 1 ]]; then
  chmod 0600 "$ENV_FILE"
  systemctl restart arbos-relay.service
  logger -t arbos-oauth-secret-watch "relay.env updated (client id${secret:+ + secret}); arbos-relay restarted"
  echo "updated relay.env and restarted arbos-relay"
fi
