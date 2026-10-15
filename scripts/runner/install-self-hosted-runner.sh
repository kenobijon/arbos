#!/usr/bin/env bash
# Install the GitHub Actions self-hosted runner on the EC2 Mac
# (h-0f61194598b26ed71, user `arbosgui`) as a LaunchDaemon with the label
# `arbos-mac`, which .github/workflows/release.yml selects through the
# repository variable ARBOS_MAC_RUNNER=arbos-mac.
#
#   sudo scripts/runner/install-self-hosted-runner.sh \
#       --url https://github.com/unarbos/arbos --token <registration token>
#   gh api -X POST repos/unarbos/arbos/actions/runners/registration-token --jq .token |
#       sudo scripts/runner/install-self-hosted-runner.sh --url https://github.com/unarbos/arbos --token -
#
# The registration token comes from the repo: Settings → Actions → Runners →
# New self-hosted runner (or `gh api -X POST repos/unarbos/arbos/actions/runners/registration-token`
# with an admin token). It is valid for one hour and is used once, here.
#
# Options:
#   --user NAME      account the runner runs as (default arbosgui)
#   --labels LIST    extra labels, comma-separated (always includes arbos-mac)
#   --name NAME      runner name (default arbos-ec2-mac)
#   --uninstall      stop the daemon, unregister (needs --token), remove files
#
# Why a LaunchDaemon and not `svc.sh install` (a LaunchAgent): the daemon
# starts at boot with no one logged in and survives GUI logouts. The trade-off
# is no access to the WindowServer session, which the release build does not
# need (codesign, notarytool, cargo, hdiutil are all headless).
#
# Security note for a public repository: a self-hosted runner executes the
# workflow files of whatever is pushed to it. release.yml runs only on push,
# tag, workflow_run, and workflow_dispatch — never pull_request — and the repo
# should keep "Require approval for all outside collaborators" on for Actions
# so fork PRs cannot reach this machine.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS only" >&2
  exit 2
fi
if [[ "$(id -u)" -ne 0 ]]; then
  echo "run with sudo (writes /Library/LaunchDaemons)" >&2
  exit 2
fi

URL="" TOKEN="" RUNNER_USER="arbosgui" EXTRA_LABELS="" NAME="arbos-ec2-mac" UNINSTALL=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --user) RUNNER_USER="$2"; shift 2 ;;
    --labels) EXTRA_LABELS="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --uninstall) UNINSTALL=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

HOME_DIR="$(dscl . -read "/Users/$RUNNER_USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
if [[ -z "$HOME_DIR" ]]; then
  echo "no such user: $RUNNER_USER" >&2
  exit 2
fi
RUNNER_DIR="$HOME_DIR/actions-runner"
LABEL="sh.arbos.actions-runner"
PLIST="/Library/LaunchDaemons/$LABEL.plist"
LOG_DIR="/var/log/arbos-runner"

if $UNINSTALL; then
  launchctl bootout system "$PLIST" 2>/dev/null || true
  rm -f "$PLIST"
  if [[ -x "$RUNNER_DIR/config.sh" ]]; then
    if [[ -n "$TOKEN" ]]; then
      sudo -u "$RUNNER_USER" "$RUNNER_DIR/config.sh" remove --token "$TOKEN" || true
    else
      echo "no --token: the runner stays registered on GitHub (remove it in Settings → Actions → Runners)" >&2
    fi
  fi
  rm -rf "$RUNNER_DIR"
  echo "runner removed"
  exit 0
fi

: "${URL:?--url https://github.com/<owner>/<repo>}" "${TOKEN:?--token <registration token> (or - to read it from stdin)}"
if [[ "$TOKEN" == "-" ]]; then
  # Keeps the token out of `ps` and shell history on a shared machine.
  read -r TOKEN
fi

ARCH="$(uname -m)"
case "$ARCH" in
  arm64) RUNNER_ARCH="osx-arm64" ;;
  x86_64) RUNNER_ARCH="osx-x64" ;;
  *) echo "unsupported arch $ARCH" >&2; exit 2 ;;
esac

# Latest runner release, pinned by the checksum GitHub publishes with it.
echo "==> resolving the latest actions/runner"
RELEASE_JSON="$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest)"
VERSION="$(printf '%s' "$RELEASE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"].lstrip("v"))')"
ASSET="actions-runner-$RUNNER_ARCH-$VERSION.tar.gz"
ASSET_URL="https://github.com/actions/runner/releases/download/v$VERSION/$ASSET"
EXPECTED_SHA="$(printf '%s' "$RELEASE_JSON" | python3 -c '
import json,sys,re
body=json.load(sys.stdin)["body"]
asset=sys.argv[1]
m=re.search(r"([0-9a-f]{64})\s*(?:\*|\s)?\S*"+re.escape(asset), body) or re.search(re.escape(asset)+r"[^0-9a-f]*([0-9a-f]{64})", body)
print(m.group(1) if m else "")' "$ASSET")"

echo "==> installing runner $VERSION for $RUNNER_USER in $RUNNER_DIR"
sudo -u "$RUNNER_USER" mkdir -p "$RUNNER_DIR"
sudo -u "$RUNNER_USER" curl -fsSL -o "$RUNNER_DIR/$ASSET" "$ASSET_URL"
if [[ -n "$EXPECTED_SHA" ]]; then
  echo "$EXPECTED_SHA  $RUNNER_DIR/$ASSET" | shasum -a 256 -c -
else
  echo "warning: could not find the published checksum for $ASSET; continuing without it" >&2
fi
sudo -u "$RUNNER_USER" tar -xzf "$RUNNER_DIR/$ASSET" -C "$RUNNER_DIR"
rm -f "$RUNNER_DIR/$ASSET"

LABELS="arbos-mac"
if [[ -n "$EXTRA_LABELS" ]]; then LABELS="$LABELS,$EXTRA_LABELS"; fi
echo "==> registering with $URL (labels: $LABELS)"
sudo -u "$RUNNER_USER" env -i HOME="$HOME_DIR" PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
  "$RUNNER_DIR/config.sh" --unattended --replace \
  --url "$URL" --token "$TOKEN" \
  --name "$NAME" --labels "$LABELS" --work "_work" \
  --disableupdate

# Toolchains the release job expects on the runner user's PATH. The workflow
# installs rustup if it is missing, but a warm toolchain saves 10 minutes.
if ! sudo -u "$RUNNER_USER" bash -lc 'command -v rustup' >/dev/null 2>&1; then
  echo "==> installing rustup for $RUNNER_USER"
  sudo -u "$RUNNER_USER" bash -lc 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal'
fi
sudo -u "$RUNNER_USER" bash -lc 'rustup target add aarch64-apple-darwin x86_64-apple-darwin' || true
xcode-select -p >/dev/null 2>&1 || echo "warning: no Xcode command line tools (xcode-select --install)" >&2

# What `svc.sh install` would do: the service wrapper lives in bin/ and is
# copied to the root so the daemon has a stable path.
sudo -u "$RUNNER_USER" cp "$RUNNER_DIR/bin/runsvc.sh" "$RUNNER_DIR/runsvc.sh"
chmod 755 "$RUNNER_DIR/runsvc.sh"
mkdir -p "$LOG_DIR"
chown "$RUNNER_USER" "$LOG_DIR"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <key>UserName</key>
    <string>$RUNNER_USER</string>
    <key>WorkingDirectory</key>
    <string>$RUNNER_DIR</string>
    <key>ProgramArguments</key>
    <array>
        <string>$RUNNER_DIR/runsvc.sh</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>$HOME_DIR</string>
        <key>PATH</key>
        <string>$HOME_DIR/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>ACTIONS_RUNNER_SVC</key>
        <string>1</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>SessionCreate</key>
    <true/>
    <key>StandardOutPath</key>
    <string>$LOG_DIR/runner.log</string>
    <key>StandardErrorPath</key>
    <string>$LOG_DIR/runner.err.log</string>
</dict>
</plist>
EOF
chmod 644 "$PLIST"
chown root:wheel "$PLIST"
launchctl bootout system "$PLIST" 2>/dev/null || true
launchctl bootstrap system "$PLIST"
launchctl kickstart -k "system/$LABEL"
sleep 3
launchctl print "system/$LABEL" | grep -E "state|pid" | head -3 || true
echo "==> runner '$NAME' (labels $LABELS) installed; logs in $LOG_DIR"
echo "    set the repository variable ARBOS_MAC_RUNNER=arbos-mac so release.yml uses it:"
echo "    gh variable set ARBOS_MAC_RUNNER --body arbos-mac --repo <owner>/<repo>"
