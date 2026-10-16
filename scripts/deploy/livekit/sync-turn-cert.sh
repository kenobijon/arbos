#!/usr/bin/env bash
# Copy Caddy's certificate for the TURN hostname into /etc/livekit/certs so LiveKit's embedded TURN
# can serve TLS on 5349, and bounce LiveKit when the certificate changed (Let's Encrypt renews it
# every ~60 days; Caddy does that on its own). Installed as /usr/local/bin/arbos-livekit-sync-cert
# and run daily by arbos-livekit-sync-cert.timer. Env (from /etc/livekit/livekit.env):
#   TURN_DOMAIN, CADDY_DATA (the :443 Caddy's storage root), LIVEKIT_DIR (default /etc/livekit)
set -euo pipefail
[[ -f /etc/livekit/livekit.env ]] && . /etc/livekit/livekit.env
: "${TURN_DOMAIN:?TURN_DOMAIN}" "${CADDY_DATA:?CADDY_DATA}"
LIVEKIT_DIR=${LIVEKIT_DIR:-/etc/livekit}
src=$(find "$CADDY_DATA/certificates" -type d -name "$TURN_DOMAIN" 2>/dev/null | head -1)
[[ -n "$src" && -f "$src/$TURN_DOMAIN.crt" && -f "$src/$TURN_DOMAIN.key" ]] || {
  echo "sync-turn-cert: no certificate for $TURN_DOMAIN under $CADDY_DATA yet (Caddy issues it on first start)" >&2
  exit 0
}
install -d -m 0750 "$LIVEKIT_DIR/certs"
changed=0
for ext in crt key; do
  if ! cmp -s "$src/$TURN_DOMAIN.$ext" "$LIVEKIT_DIR/certs/$TURN_DOMAIN.$ext"; then
    install -m 0640 "$src/$TURN_DOMAIN.$ext" "$LIVEKIT_DIR/certs/$TURN_DOMAIN.$ext"; changed=1
  fi
done
if [[ $changed -eq 1 ]]; then
  echo "sync-turn-cert: certificate for $TURN_DOMAIN updated ($(openssl x509 -in "$LIVEKIT_DIR/certs/$TURN_DOMAIN.crt" -noout -enddate))"
  if systemctl is-active --quiet livekit.service; then
    systemctl restart livekit.service && echo "sync-turn-cert: livekit restarted"
  fi
fi
