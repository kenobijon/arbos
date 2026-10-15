#!/usr/bin/env bash
# Deploy the Arbos relay + the arbos.life landing on Jacob's server (Ubuntu, systemd, Caddy).
# Idempotent: run it again to upgrade the relay, refresh the site, or re-render the env file.
#
#   sudo -E scripts/deploy/relay-server.sh                 # install/upgrade everything
#   sudo -E scripts/deploy/relay-server.sh --import-data /path/to/relay-data   # also import a relay data dir
#   RELAY_VERSION=0.3.0.189 sudo -E scripts/deploy/relay-server.sh             # pin a release instead of the newest edge
#   sudo -E scripts/deploy/relay-server.sh --from-source main   # build arbos-relay + arbosctl from a git ref on this box
#                                                               # (any ref of github.com/unarbos/arbos; needs ~2 min on 128 cores)
#
# Secrets never live in the repo. They are written to /etc/arbos/relay.env (root:root 0600) from
# the environment at deploy time; the usual way is:
#   export OP_SERVICE_ACCOUNT_TOKEN=…   (1Password service account for the Arbos vault)
#   sudo -E scripts/deploy/relay-server.sh      # pulls the keys with `op read`
# or export OPENROUTER_API_KEY / OPENAI_API_KEY / DAYTONA_API_KEY / ARBOS_RELAY_ADMIN_TOKEN /
# ARBOS_RELAY_KEY / ARBOS_GITHUB_CLIENT_ID / ARBOS_GITHUB_CLIENT_SECRET yourself.
#
# Layout on the server:
#   /usr/local/bin/arbos-relay           binary from the GitHub edge release (linux-x86_64)
#   /var/lib/arbos-relay                 data dir (SQLite + blobs), owned by the `arbos` system user
#   /etc/arbos/relay.env                 systemd EnvironmentFile (root only)
#   /etc/systemd/system/arbos-relay.service
#   /home/const/arbos-www/Caddyfile      the :443 Caddy (arbos-www.service) — site + relay vhost
#   /home/const/arbos-www/site           landing page (copy of site/ in this repo)
#   /etc/arbos/download.caddy            `/download/mac` redirect, rewritten by refresh-mac-download.sh
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
RELAY_VERSION="${RELAY_VERSION:-}"
PUBLIC_URL="${ARBOS_RELAY_PUBLIC_URL:-https://relay.arbos.life}"
LISTEN="${ARBOS_RELAY_LISTEN:-127.0.0.1:7700}"
DATA_DIR="${ARBOS_RELAY_DATA_DIR:-/var/lib/arbos-relay}"
ENV_FILE=/etc/arbos/relay.env
WWW_USER="${WWW_USER:-const}"
WWW_DIR="${WWW_DIR:-/home/$WWW_USER/arbos-www}"
IMPORT_DATA=""
FROM_SOURCE=""
BUILD_USER="${BUILD_USER:-${SUDO_USER:-const}}"
BUILD_DIR="${BUILD_DIR:-/home/$BUILD_USER/arbos-build}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --import-data) IMPORT_DATA="$2"; shift 2;;
    --from-source) FROM_SOURCE="$2"; shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[[ $EUID -eq 0 ]] || { echo "run as root (sudo -E)" >&2; exit 1; }

say() { printf '\n==> %s\n' "$*"; }

# ---- 1. relay binary: built from a git ref on this box, or from the newest edge release ----------
say "relay binary"
if [[ -n "$FROM_SOURCE" ]]; then
  # Build as $BUILD_USER (never root): rustup + the toolchain pinned by rust-toolchain.toml.
  id "$BUILD_USER" >/dev/null 2>&1 || { echo "build user $BUILD_USER does not exist" >&2; exit 1; }
  install -d -o "$BUILD_USER" -g "$BUILD_USER" "$BUILD_DIR"
  sudo -u "$BUILD_USER" -H bash -euo pipefail -c '
    export PATH="$HOME/.cargo/bin:$PATH"
    command -v rustup >/dev/null 2>&1 || curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
    cd "'"$BUILD_DIR"'"
    [[ -d repo/.git ]] || git clone -q https://github.com/unarbos/arbos.git repo
    # GitHub only serves fetch-by-SHA for full 40-char ids, so fetch everything and resolve locally
    # (works for branch names, tags, full and abbreviated commit ids).
    git -C repo fetch -q --tags origin "+refs/heads/*:refs/remotes/origin/*"
    REF="'"$FROM_SOURCE"'"
    COMMIT=$(git -C repo rev-parse -q --verify "origin/$REF^{commit}" 2>/dev/null \
          || git -C repo rev-parse -q --verify "$REF^{commit}" 2>/dev/null) \
      || { echo "cannot resolve $REF in unarbos/arbos" >&2; exit 1; }
    git -C repo checkout -q --detach "$COMMIT"
    echo "building $(git -C repo log --oneline -1)"
    cd repo && cargo build --release -p arbos-relay -p arbosctl 2>&1 | tail -3
  '
  install -m 0755 "$BUILD_DIR/repo/target/release/arbos-relay" /usr/local/bin/arbos-relay
  install -m 0755 "$BUILD_DIR/repo/target/release/arbosctl" /usr/local/bin/arbosctl
  echo "installed arbos-relay $(/usr/local/bin/arbos-relay --version | awk '{print $2}') from $FROM_SOURCE @ $(git -C "$BUILD_DIR/repo" rev-parse --short HEAD)"
else
if [[ -z "$RELAY_VERSION" ]]; then
  RELAY_VERSION=$(curl -fsSL "https://api.github.com/repos/unarbos/arbos/releases?per_page=20" \
    | python3 -c 'import sys,json; r=[x for x in json.load(sys.stdin) if x["tag_name"].startswith("edge-")]; print(r[0]["tag_name"][5:] if r else "")')
  [[ -n "$RELAY_VERSION" ]] || { echo "no edge release found" >&2; exit 1; }
fi
CURRENT=$(/usr/local/bin/arbos-relay --version 2>/dev/null | awk '{print $2}' || true)
if [[ "$CURRENT" == "$RELAY_VERSION" ]]; then
  echo "arbos-relay $RELAY_VERSION already installed"
else
  TMP=$(mktemp -d)
  curl -fsSL -o "$TMP/relay.tgz" \
    "https://github.com/unarbos/arbos/releases/download/edge-$RELAY_VERSION/arbos-relay-$RELAY_VERSION-linux-x86_64.tar.gz"
  tar -xzf "$TMP/relay.tgz" -C "$TMP"
  BIN=$(find "$TMP" -type f -name 'arbos-relay*' ! -name '*.tar.gz' | head -1)
  install -m 0755 "$BIN" /usr/local/bin/arbos-relay
  rm -rf "$TMP"
  echo "installed arbos-relay $RELAY_VERSION"
fi
# arbosctl beside it (handy for ops)
if ! /usr/local/bin/arbosctl --version 2>/dev/null | grep -q "$RELAY_VERSION"; then
  TMP=$(mktemp -d)
  if curl -fsSL -o "$TMP/ctl.tgz" "https://github.com/unarbos/arbos/releases/download/edge-$RELAY_VERSION/arbosctl-$RELAY_VERSION-linux-x86_64.tar.gz"; then
    tar -xzf "$TMP/ctl.tgz" -C "$TMP"
    BIN=$(find "$TMP" -type f -name 'arbosctl*' ! -name '*.tar.gz' | head -1)
    install -m 0755 "$BIN" /usr/local/bin/arbosctl
  fi
  rm -rf "$TMP"
fi
fi  # FROM_SOURCE

# ---- 2. system user + data dir ---------------------------------------------------------------------
say "user + data dir"
id arbos >/dev/null 2>&1 || useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin arbos
mkdir -p "$DATA_DIR" /etc/arbos
if [[ -n "$IMPORT_DATA" ]]; then
  [[ -f "$IMPORT_DATA/relay.db" ]] || { echo "no relay.db in $IMPORT_DATA" >&2; exit 1; }
  if [[ -f "$DATA_DIR/relay.db" ]]; then
    BK="$DATA_DIR.bak-$(date -u +%Y%m%dT%H%M%SZ)"; cp -a "$DATA_DIR" "$BK"; echo "backed up existing data to $BK"
  fi
  rsync -a --delete "$IMPORT_DATA/" "$DATA_DIR/"
  echo "imported relay data from $IMPORT_DATA"
fi
chown -R arbos:arbos "$DATA_DIR"; chmod 0750 "$DATA_DIR"

# ---- 3. env file (secrets) -------------------------------------------------------------------------
say "env file"
opread() { op read "$1" 2>/dev/null || true; }
if command -v op >/dev/null 2>&1 && [[ -n "${OP_SERVICE_ACCOUNT_TOKEN:-}" ]]; then
  : "${OPENROUTER_API_KEY:=$(opread 'op://Arbos/xbirrctuljw2m6aieoway2szom/credential')}"
  : "${OPENAI_API_KEY:=$(opread 'op://Arbos/6k3kuhvc3xxwpgao4mblskj45e/notesPlain' | tr -d '[:space:]')}"
  : "${DAYTONA_API_KEY:=$(opread 'op://Arbos/fywmj6vtq5delybw5c7a53l2qa/notesPlain' | tr -d '[:space:]')}"
  : "${ARBOS_RELAY_ADMIN_TOKEN:=$(opread 'op://Arbos/Arbos relay (relay.arbos.life) — admin token/credential')}"
  : "${ARBOS_RELAY_KEY:=$(opread 'op://Arbos/Arbos relay (relay.arbos.life) — connector key (ARBOS_RELAY_KEY)/credential')}"
  : "${ARBOS_JWT_KEY:=$(opread 'op://Arbos/Arbos relay (relay.arbos.life) — JWT key (ARBOS_JWT_KEY)/credential')}"
  : "${ARBOS_GITHUB_CLIENT_ID:=$(opread 'op://Arbos/Arbos GitHub OAuth app/ARBOS_GITHUB_CLIENT_ID')}"
  : "${ARBOS_GITHUB_CLIENT_SECRET:=$(opread 'op://Arbos/Arbos GitHub OAuth app/ARBOS_GITHUB_CLIENT_SECRET')}"
fi
# keep values already in the env file when nothing new was supplied
if [[ -f "$ENV_FILE" ]]; then
  keep() { local k=$1; [[ -n "${!k:-}" ]] || printf -v "$k" '%s' "$(sed -n "s/^$k=//p" "$ENV_FILE" | head -1)"; }
  for k in OPENROUTER_API_KEY OPENAI_API_KEY DAYTONA_API_KEY ARBOS_RELAY_ADMIN_TOKEN ARBOS_RELAY_KEY ARBOS_JWT_KEY ARBOS_GITHUB_CLIENT_ID ARBOS_GITHUB_CLIENT_SECRET; do keep "$k"; done
fi
[[ -n "${DAYTONA_API_KEY:-}" ]] || echo "WARNING: DAYTONA_API_KEY is empty — new projects will get process boxes"
[[ -n "${OPENROUTER_API_KEY:-}${OPENAI_API_KEY:-}" ]] || echo "WARNING: no model provider key"
: "${ARBOS_RELAY_ADMIN_TOKEN:=adm_$(openssl rand -hex 32)}"
: "${ARBOS_RELAY_KEY:=$(openssl rand -hex 32)}"
: "${ARBOS_JWT_KEY:=$(openssl rand -hex 32)}"
umask 077
cat > "$ENV_FILE" <<EOF
# Arbos relay — written by scripts/deploy/relay-server.sh on $(date -u +%FT%TZ). root only.
ARBOS_RELAY_LISTEN=$LISTEN
ARBOS_RELAY_PUBLIC_URL=$PUBLIC_URL
ARBOS_PUBLIC_URL=$PUBLIC_URL
ARBOS_RELAY_DATA=$DATA_DIR
ARBOS_RELAY_ADMIN_TOKEN=$ARBOS_RELAY_ADMIN_TOKEN
ARBOS_RELAY_KEY=$ARBOS_RELAY_KEY
ARBOS_JWT_KEY=$ARBOS_JWT_KEY
ARBOS_AUTH_MODE=${ARBOS_AUTH_MODE:-github}
OPENROUTER_API_KEY=${OPENROUTER_API_KEY:-}
OPENAI_API_KEY=${OPENAI_API_KEY:-}
DAYTONA_API_KEY=${DAYTONA_API_KEY:-}
ARBOS_GITHUB_CLIENT_ID=${ARBOS_GITHUB_CLIENT_ID:-}
ARBOS_GITHUB_CLIENT_SECRET=${ARBOS_GITHUB_CLIENT_SECRET:-}
ARBOS_GITHUB_OAUTH_CLIENT_ID=${ARBOS_GITHUB_CLIENT_ID:-}
ARBOS_GITHUB_OAUTH_CLIENT_SECRET=${ARBOS_GITHUB_CLIENT_SECRET:-}
ARBOS_OAUTH_REDIRECT_URL=$PUBLIC_URL/auth/github/callback
RUST_LOG=info,arbos_relay=info
EOF
chmod 0600 "$ENV_FILE"; chown root:root "$ENV_FILE"
umask 022

# ---- 4. systemd unit -------------------------------------------------------------------------------
say "systemd"
cat > /etc/systemd/system/arbos-relay.service <<EOF
[Unit]
Description=Arbos relay (registry, pairing, sync rooms, model proxy, Daytona)
After=network-online.target
Wants=network-online.target

[Service]
User=arbos
Group=arbos
EnvironmentFile=$ENV_FILE
WorkingDirectory=$DATA_DIR
ExecStart=/usr/local/bin/arbos-relay serve
Restart=always
RestartSec=2
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$DATA_DIR
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now arbos-relay.service >/dev/null
systemctl restart arbos-relay.service
sleep 2
systemctl is-active arbos-relay.service
curl -fsS "http://$LISTEN/healthz" >/dev/null && echo "relay healthz ok on $LISTEN"

# ---- 5. site + Caddy -------------------------------------------------------------------------------
say "site + caddy"
install -d -o "$WWW_USER" -g "$WWW_USER" "$WWW_DIR"
if [[ -d "$WWW_DIR/site" && ! -e "$WWW_DIR/site.pre-selfserve" && ! -f /var/lib/arbos-site/deployed-sha ]]; then
  mv "$WWW_DIR/site" "$WWW_DIR/site.pre-selfserve"; echo "kept previous site at $WWW_DIR/site.pre-selfserve"
fi
# The site itself is deployed from the repo's main by arbos-site-sync (timer below); seed it once
# from this checkout so a fresh server has a page before the first sync.
if [[ ! -d "$WWW_DIR/site" ]]; then
  cp -a "$REPO_DIR/site" "$WWW_DIR/site"; chown -R "$WWW_USER:$WWW_USER" "$WWW_DIR/site"
fi
install -m 0755 "$REPO_DIR/scripts/deploy/site-sync.sh" /usr/local/bin/arbos-site-sync
cat > /etc/systemd/system/arbos-site-sync.service <<'UNIT'
[Unit]
Description=Deploy site/ from github.com/unarbos/arbos main into the arbos.life web root
[Service]
Type=oneshot
ExecStart=/usr/local/bin/arbos-site-sync
UNIT
cat > /etc/systemd/system/arbos-site-sync.timer <<'UNIT'
[Unit]
Description=Sync the arbos.life site from the repo every 5 minutes
[Timer]
OnBootSec=1min
OnUnitActiveSec=5min
[Install]
WantedBy=timers.target
UNIT
systemctl daemon-reload
systemctl enable --now arbos-site-sync.timer >/dev/null
/usr/local/bin/arbos-site-sync || echo "WARNING: site sync failed; the seeded site stays"
install -m 0755 "$REPO_DIR/scripts/deploy/refresh-mac-download.sh" /usr/local/bin/arbos-refresh-mac-download
/usr/local/bin/arbos-refresh-mac-download --no-reload || true
if [[ -f "$WWW_DIR/Caddyfile" ]] && ! cmp -s "$REPO_DIR/scripts/deploy/Caddyfile" "$WWW_DIR/Caddyfile"; then
  cp -a "$WWW_DIR/Caddyfile" "$WWW_DIR/Caddyfile.bak-$(date -u +%Y%m%dT%H%M%SZ)"
fi
sed -e "s#@WWW_DIR@#$WWW_DIR#g" -e "s#@RELAY_LISTEN@#$LISTEN#g" "$REPO_DIR/scripts/deploy/Caddyfile" > "$WWW_DIR/Caddyfile"
chown "$WWW_USER:$WWW_USER" "$WWW_DIR/Caddyfile"
CADDY_BIN="$WWW_DIR/bin/caddy"; [[ -x "$CADDY_BIN" ]] || CADDY_BIN=$(command -v caddy)
"$CADDY_BIN" validate --config "$WWW_DIR/Caddyfile" --adapter caddyfile >/dev/null
if systemctl is-active --quiet arbos-www.service; then systemctl reload arbos-www.service || systemctl restart arbos-www.service; else systemctl restart arbos-www.service; fi
# refresh timer for the Mac download redirect
cat > /etc/systemd/system/arbos-refresh-mac-download.service <<'EOF'
[Unit]
Description=Refresh the arbos.life /download/mac redirect from the newest edge release
[Service]
Type=oneshot
ExecStart=/usr/local/bin/arbos-refresh-mac-download
EOF
cat > /etc/systemd/system/arbos-refresh-mac-download.timer <<'EOF'
[Unit]
Description=Refresh /download/mac every 15 minutes
[Timer]
OnBootSec=2min
OnUnitActiveSec=15min
[Install]
WantedBy=timers.target
EOF
systemctl daemon-reload
systemctl enable --now arbos-refresh-mac-download.timer >/dev/null

# ---- 6. GitHub OAuth secret watcher (vault -> relay.env -> restart) --------------------------------
say "oauth secret watcher"
install -m 0755 "$REPO_DIR/scripts/deploy/oauth-secret-watch.sh" /usr/local/bin/arbos-oauth-secret-watch
if [[ -n "${OP_SERVICE_ACCOUNT_TOKEN:-}" ]]; then
  umask 077; printf 'OP_SERVICE_ACCOUNT_TOKEN=%s\n' "$OP_SERVICE_ACCOUNT_TOKEN" > /etc/arbos/op.env; chmod 0600 /etc/arbos/op.env; umask 022
fi
cat > /etc/systemd/system/arbos-oauth-secret-watch.service <<'UNIT'
[Unit]
Description=Pull the GitHub OAuth client id/secret from the Arbos vault into relay.env
[Service]
Type=oneshot
ExecStart=/usr/local/bin/arbos-oauth-secret-watch
UNIT
cat > /etc/systemd/system/arbos-oauth-secret-watch.timer <<'UNIT'
[Unit]
Description=Check the vault for the GitHub OAuth secret every 2 minutes
[Timer]
OnBootSec=1min
OnUnitActiveSec=2min
[Install]
WantedBy=timers.target
UNIT
systemctl daemon-reload
systemctl enable --now arbos-oauth-secret-watch.timer >/dev/null
/usr/local/bin/arbos-oauth-secret-watch || true

say "done"
echo "relay: $PUBLIC_URL (listening on $LISTEN, data $DATA_DIR, env $ENV_FILE)"
echo "site:  https://arbos.life  (files $WWW_DIR/site, synced from origin/main site/ by arbos-site-sync.timer)"
echo "mac download: $PUBLIC_URL/download/mac  →  $(sed -n 's/.*redir [^ ]* \(https[^ ]*\).*/\1/p' /etc/arbos/download.caddy 2>/dev/null | head -1)"
