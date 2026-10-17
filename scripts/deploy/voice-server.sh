#!/usr/bin/env bash
# Deploy the Arbos voice worker (Calls Phase 2, D75) next to the relay and LiveKit on the relay host.
# Idempotent: run it again to upgrade the code or re-render the unit.
#
#   sudo -E scripts/deploy/voice-server.sh            # install/upgrade from this checkout's voice/
#   sudo -E scripts/deploy/voice-server.sh --env-only # only rewrite /etc/arbos/voice.env + unit
#
# Secrets: the worker reads /etc/arbos/relay.env (OPENAI_API_KEY, LIVEKIT_*, ARBOS_RELAY_ADMIN_TOKEN,
# written by relay-server.sh) and /etc/arbos/voice.env (worker knobs, no secrets). Nothing is
# copied into the repo.
#
# Layout on the server:
#   /opt/arbos-voice                 the voice/ package + a uv-managed venv (.venv), owned by `arbos`
#   /etc/arbos/voice.env             worker knobs (model, agent name, STT model, log level)
#   /etc/systemd/system/arbos-voice.service
# Logs: journalctl -u arbos-voice -f   (one line per judge decision, TTFA, barge-in, usage, summary)
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
APP_DIR="${ARBOS_VOICE_DIR:-/opt/arbos-voice}"
ENV_FILE=/etc/arbos/voice.env
RELAY_ENV=/etc/arbos/relay.env
RELAY_URL="${ARBOS_RELAY_URL:-http://127.0.0.1:7700}"
ENV_ONLY=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --env-only) ENV_ONLY=1; shift;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[[ $EUID -eq 0 ]] || { echo "run as root (sudo -E)" >&2; exit 1; }
[[ -f "$RELAY_ENV" ]] || { echo "$RELAY_ENV missing: deploy the relay first" >&2; exit 1; }
grep -q '^OPENAI_API_KEY=.\+' "$RELAY_ENV" || { echo "OPENAI_API_KEY is empty in $RELAY_ENV: the voice worker needs it" >&2; exit 1; }
grep -q '^LIVEKIT_API_SECRET=.\+' "$RELAY_ENV" || { echo "LIVEKIT_* missing in $RELAY_ENV: deploy LiveKit first" >&2; exit 1; }

say() { printf '\n==> %s\n' "$*"; }
id arbos >/dev/null 2>&1 || useradd --system --home-dir /var/lib/arbos-relay --shell /usr/sbin/nologin arbos

# ---- 1. code + venv ---------------------------------------------------------------------------------
if [[ -z "$ENV_ONLY" ]]; then
  say "code → $APP_DIR"
  install -d -o arbos -g arbos "$APP_DIR"
  rsync -a --delete --exclude .venv --exclude '__pycache__' --exclude 'tests/fixtures/judge-results.json' "$REPO_DIR/voice/" "$APP_DIR/"
  chown -R arbos:arbos "$APP_DIR"
  say "venv (uv)"
  if ! command -v uv >/dev/null 2>&1; then
    curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh >/dev/null
  fi
  install -d -o arbos -g arbos /var/lib/arbos-voice
  sudo -u arbos -H env HOME=/var/lib/arbos-voice UV_CACHE_DIR=/var/lib/arbos-voice/uv-cache bash -euo pipefail -c '
    cd "'"$APP_DIR"'"
    [[ -d .venv ]] || uv venv -q .venv -p 3.12
    uv pip install -q --python .venv/bin/python -e .
    .venv/bin/python -c "import arbos_voice, livekit.agents as a; print(\"arbos-voice\", arbos_voice.__version__, \"livekit-agents\", a.__version__)"
  '
  git -C "$REPO_DIR" rev-parse HEAD > /etc/arbos/voice.deployed-sha 2>/dev/null && chmod 0644 /etc/arbos/voice.deployed-sha || true
fi

# ---- 2. knobs ----------------------------------------------------------------------------------------
say "env → $ENV_FILE"
if [[ -f "$ENV_FILE" ]]; then
  keep() { local k=$1; [[ -n "${!k:-}" ]] || printf -v "$k" '%s' "$(sed -n "s/^$k=//p" "$ENV_FILE" | head -1)"; }
  for k in ARBOS_VOICE_MODEL ARBOS_VOICE_VOICE ARBOS_VOICE_STT_MODEL ARBOS_VOICE_JUDGE_MODEL ARBOS_VOICE_AGENT_NAME ARBOS_VOICE_LOG_LEVEL; do keep "$k"; done
fi
umask 022
cat > "$ENV_FILE" <<EOF
# Arbos voice worker — written by scripts/deploy/voice-server.sh on $(date -u +%FT%TZ). No secrets here.
ARBOS_RELAY_URL=$RELAY_URL
ARBOS_VOICE_AGENT_NAME=${ARBOS_VOICE_AGENT_NAME:-arbos-voice}
ARBOS_VOICE_MODEL=${ARBOS_VOICE_MODEL:-gpt-realtime-2.1}
ARBOS_VOICE_VOICE=${ARBOS_VOICE_VOICE:-marin}
ARBOS_VOICE_STT_MODEL=${ARBOS_VOICE_STT_MODEL:-gpt-4o-mini-transcribe}
ARBOS_VOICE_JUDGE_MODEL=${ARBOS_VOICE_JUDGE_MODEL:-typesafe/jev-1.14}
ARBOS_VOICE_LOG_LEVEL=${ARBOS_VOICE_LOG_LEVEL:-INFO}
LIVEKIT_AGENTS_PROD=1
HOME=/var/lib/arbos-voice
EOF

# ---- 3. systemd ---------------------------------------------------------------------------------------
say "systemd"
cat > /etc/systemd/system/arbos-voice.service <<EOF
[Unit]
Description=Arbos voice worker (LiveKit Agents + OpenAI Realtime; agent voice calls)
After=network-online.target arbos-relay.service livekit.service
Wants=network-online.target

[Service]
User=arbos
Group=arbos
EnvironmentFile=$RELAY_ENV
EnvironmentFile=$ENV_FILE
WorkingDirectory=$APP_DIR
ExecStart=$APP_DIR/.venv/bin/arbos-voice start
Restart=always
RestartSec=3
KillSignal=SIGINT
TimeoutStopSec=90
NoNewPrivileges=true
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now arbos-voice.service >/dev/null
systemctl restart arbos-voice.service
sleep 3
systemctl is-active arbos-voice.service
journalctl -u arbos-voice -n 5 --no-pager
echo "voice worker deployed from $(cat /etc/arbos/voice.deployed-sha 2>/dev/null | cut -c1-8)"
