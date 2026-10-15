#!/usr/bin/env bash
# Build Arbos.app for macOS: universal binary (arm64 + x86_64), Info.plist,
# optional Developer ID signing and notarization (D42). Skips the steps it
# has no credentials for, so it runs on a machine with no certificates.
#
#   scripts/bundle-mac.sh                      # universal release .app in dist/
#   ARBOS_SIGN_IDENTITY="Developer ID Application: …" scripts/bundle-mac.sh
#   ARBOS_NOTARY_PROFILE=arbos-notary scripts/bundle-mac.sh   # notarytool keychain profile
#   ARBOS_TARGETS=aarch64-apple-darwin scripts/bundle-mac.sh  # single-arch (faster)
#
# Sparkle (auto-update) is a TODO: add the framework to Contents/Frameworks,
# SUFeedURL + SUPublicEDKey to Info.plist, and sign the appcast.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "bundle-mac.sh: macOS only (lipo, codesign, notarytool)." >&2
  exit 2
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="Arbos"
BIN="arbos-mac"
VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if p["name"]=="arbos-mac"))')"
BUILD="${ARBOS_BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
TARGETS="${ARBOS_TARGETS:-aarch64-apple-darwin x86_64-apple-darwin}"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

echo "==> building $BIN $VERSION ($BUILD) for: $TARGETS"
BINS=()
for t in $TARGETS; do
  rustup target add "$t" >/dev/null
  cargo build --release -p "$BIN" --target "$t"
  BINS+=("$ROOT/target/$t/release/$BIN")
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [[ ${#BINS[@]} -gt 1 ]]; then
  lipo -create "${BINS[@]}" -output "$APP/Contents/MacOS/$BIN"
else
  cp "${BINS[0]}" "$APP/Contents/MacOS/$BIN"
fi
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" \
  "crates/arbos-mac/macos/Info.plist" > "$APP/Contents/Info.plist"
echo -n "APPL????" > "$APP/Contents/PkgInfo"
if [[ -f crates/arbos-mac/macos/Arbos.icns ]]; then
  cp crates/arbos-mac/macos/Arbos.icns "$APP/Contents/Resources/"
fi
echo "==> $APP"

if [[ -n "${ARBOS_SIGN_IDENTITY:-}" ]]; then
  echo "==> signing with: $ARBOS_SIGN_IDENTITY"
  codesign --force --deep --options runtime --timestamp \
    --entitlements crates/arbos-mac/macos/entitlements.plist \
    --sign "$ARBOS_SIGN_IDENTITY" "$APP"
  codesign --verify --deep --strict --verbose=2 "$APP"
else
  echo "==> no ARBOS_SIGN_IDENTITY: ad-hoc signing (not distributable)"
  codesign --force --deep --sign - "$APP"
fi

DMG="$DIST/$APP_NAME-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$APP" -ov -format UDZO "$DMG"
echo "==> $DMG"

if [[ -n "${ARBOS_NOTARY_PROFILE:-}" && -n "${ARBOS_SIGN_IDENTITY:-}" ]]; then
  echo "==> notarizing with keychain profile: $ARBOS_NOTARY_PROFILE"
  xcrun notarytool submit "$DMG" --keychain-profile "$ARBOS_NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  xcrun stapler staple "$DMG"
else
  echo "==> no ARBOS_NOTARY_PROFILE (+ identity): notarization skipped"
fi
echo "done."
