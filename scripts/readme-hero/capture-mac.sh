#!/bin/bash
# Capture the Arbos Mac window as a 2x PNG for the README hero.
#
#   scripts/readme-hero/capture-mac.sh [out.png] [width height]
#
# Run it on a Mac where Arbos is open on a Retina (2x) display, from a terminal that has
# Screen Recording permission (System Settings → Privacy & Security). It finds the Arbos
# window, optionally resizes it (default 1160x720 points, the size the hero was made with),
# and saves it with `screencapture -l` — transparent rounded corners, no shadow, no cursor.
set -euo pipefail

out="${1:-mac.png}"
width="${2:-1160}"
height="${3:-720}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/winid.swift" <<'EOF'
import CoreGraphics
import Foundation
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowLayer as String] as? Int ?? 1) == 0 {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    if owner == "Arbos" || owner == "arbos-mac" {
        print(w[kCGWindowNumber as String] as? Int ?? 0, w[kCGWindowOwnerPID as String] as? Int ?? 0)
        break
    }
}
EOF
xcrun swiftc -O "$tmp/winid.swift" -o "$tmp/winid" 2>/dev/null
read -r wid pid < <("$tmp/winid")
if [ -z "${wid:-}" ] || [ "$wid" = "0" ]; then
  echo "no Arbos window on screen" >&2
  exit 1
fi

# Resize through Accessibility (needs the Accessibility permission for this terminal).
osascript -e "tell application \"System Events\" to tell (first process whose unix id is $pid) to set size of window 1 to {$width, $height}" >/dev/null 2>&1 || true
sleep 1

screencapture -l"$wid" -o -x "$out"
sips -g pixelWidth -g pixelHeight "$out" | tail -2
echo "wrote $out (expect 2x the window size; if not, move the window to a Retina display)"
