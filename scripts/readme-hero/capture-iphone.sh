#!/bin/bash
# Capture the iPhone app in the simulator as a 3x PNG for the README hero.
#
#   ARBOS_RELAY_TOKEN=… scripts/readme-hero/capture-iphone.sh <Arbos.app> [out.png]
#
# Environment (all optional except the token for a signed-in screen):
#   ARBOS_RELAY_URL      relay to talk to (default https://relay.arbos.life)
#   ARBOS_RELAY_TOKEN    a device access token for the demo account (never commit one)
#   ARBOS_OPEN_PROJECT   project id to open at launch; with ARBOS_OPEN_CHAT, that chat
#   ARBOS_OPEN_CHAT      chat id to open at launch
#   SIM_DEVICE           simulator device name to create/reuse (default "Arbos Hero")
#   SIM_TYPE             device type for a new simulator (default "iPhone Air")
#
# <Arbos.app> is a Debug simulator build (the launch hooks are compiled out of Release):
#   xcodebuild build -project apps/ios/Arbos/Arbos.xcodeproj -scheme Arbos -configuration Debug \
#     -destination 'platform=iOS Simulator,name=iPhone Air' -derivedDataPath /tmp/arbos-dd CODE_SIGNING_ALLOWED=NO
#   → /tmp/arbos-dd/Build/Products/Debug-iphonesimulator/Arbos.app
set -euo pipefail

app="${1:?path to a Debug-iphonesimulator Arbos.app}"
out="${2:-iphone.png}"
bundle=com.unarbos.arbos.ios
name="${SIM_DEVICE:-Arbos Hero}"
type="${SIM_TYPE:-iPhone Air}"

udid="$(xcrun simctl list devices -j | python3 -c "
import json, sys
devs = json.load(sys.stdin)['devices']
print(next((d['udid'] for r in devs for d in devs[r] if d['name'] == '$name'), ''))")"
if [ -z "$udid" ]; then
  runtime="$(xcrun simctl list runtimes -j | python3 -c "import json,sys; print([r['identifier'] for r in json.load(sys.stdin)['runtimes'] if r['platform']=='iOS'][-1])")"
  udid="$(xcrun simctl create "$name" "$type" "$runtime")"
fi
xcrun simctl bootstatus "$udid" -b >/dev/null 2>&1 || true
xcrun simctl ui "$udid" appearance dark
xcrun simctl status_bar "$udid" override --time "9:41" --batteryState charged --batteryLevel 100 \
  --cellularMode active --cellularBars 4 --wifiBars 3 --operatorName ""
xcrun simctl install "$udid" "$app"
# The notifications primer would cover the screen on a fresh simulator.
xcrun simctl spawn "$udid" defaults write "$bundle" pushPrimerShown -bool true
xcrun simctl terminate "$udid" "$bundle" 2>/dev/null || true

SIMCTL_CHILD_ARBOS_RELAY_URL="${ARBOS_RELAY_URL:-https://relay.arbos.life}" \
SIMCTL_CHILD_ARBOS_RELAY_TOKEN="${ARBOS_RELAY_TOKEN:-}" \
SIMCTL_CHILD_ARBOS_OPEN_PROJECT="${ARBOS_OPEN_PROJECT:-}" \
SIMCTL_CHILD_ARBOS_OPEN_CHAT="${ARBOS_OPEN_CHAT:-}" \
  xcrun simctl launch "$udid" "$bundle" >/dev/null
sleep "${SIM_SETTLE_S:-12}"
xcrun simctl io "$udid" screenshot --type png "$out" >/dev/null
sips -g pixelWidth -g pixelHeight "$out" | tail -2
echo "wrote $out"
