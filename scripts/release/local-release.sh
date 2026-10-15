#!/usr/bin/env bash
# The whole Mac release by hand, from a Mac with `op` access to the 1Password
# vault "Arbos": build the universal app, sign with the Developer ID, notarize
# and staple, sign the payload for the in-app updater, and publish the GitHub
# release + appcast. It is the fallback for .github/workflows/release.yml
# until GitHub Actions is live, and does exactly what the workflow does.
#
#   scripts/release/local-release.sh                 # edge build of HEAD, published
#   scripts/release/local-release.sh --dry-run       # build + sign + notarize, publish printed only
#   scripts/release/local-release.sh --channel stable --version 0.1.0
#   scripts/release/local-release.sh --no-notarize   # skip Apple (local check only)
#   ARBOS_TARGETS=aarch64-apple-darwin scripts/release/local-release.sh --dry-run --no-notarize   # fastest
#
# Needs: macOS 14+, Xcode command line tools, rustup, `op` signed in
# (OP_SERVICE_ACCOUNT_TOKEN or `op signin`), `gh` (installed by this script if
# brew is there). Everything secret is read into $TMPDIR with mode 0600 and
# removed on exit, whatever happens.
set -euo pipefail
set +x

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "local-release.sh: macOS only" >&2
  exit 2
fi
command -v cargo >/dev/null 2>&1 || export PATH="$HOME/.cargo/bin:$PATH"

CHANNEL="edge" VERSION="" REPO="${ARBOS_REPO:-unarbos/arbos}" DRY_RUN=false NOTARIZE=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    --no-notarize) NOTARIZE=false; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# 1Password items (vault "Arbos"). IDs, not titles: titles carry notes.
OP_VAULT="Arbos"
OP_APPLE_ITEM="phsrnmu3qfpqx6lbumafjo3uom"      # "Appe Cert Apple Developer": IDApple.p12, AuthKey_*.p8, passwords
OP_SPARKLE_ITEM="kaioxc67zr5cu25fuu3nw5qwdi"    # Sparkle/updater Ed25519 private key
OP_GITHUB_ITEM="vvnyarkwampjl3diocn7n6vcqe"     # GitHub PAT (unarbos)
NOTARY_KEY_ID="${NOTARY_KEY_ID:-XANFG7S7YS}"    # App Store Connect key "arbos ci" (notarytool)
NOTARY_ISSUER_ID="${NOTARY_ISSUER_ID:-4623d589-1653-444a-b497-93116044e272}"

for tool in op xcrun codesign hdiutil lipo; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing: $tool" >&2; exit 2; }
done
if ! command -v gh >/dev/null 2>&1; then
  if command -v brew >/dev/null 2>&1; then brew install gh; else echo "missing: gh" >&2; exit 2; fi
fi
op whoami >/dev/null 2>&1 || { echo "op is not signed in (set OP_SERVICE_ACCOUNT_TOKEN or run op signin)" >&2; exit 2; }

SECRETS="$(mktemp -d "${TMPDIR:-/tmp}/arbos-release.XXXXXX")"
chmod 700 "$SECRETS"
KEYCHAIN="" KEYCHAINS_WERE=""
cleanup() {
  set +e
  if [[ -n "$KEYCHAIN" ]]; then
    scripts/release/signing-keychain.sh remove "$KEYCHAIN" "$KEYCHAINS_WERE" >/dev/null 2>&1
  fi
  rm -rf "$SECRETS"
}
trap cleanup EXIT

# ---- version --------------------------------------------------------------
BASE="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
SHA="$(git rev-parse HEAD)"
if [[ -n "$(git status --porcelain)" ]]; then
  echo "warning: the working tree is dirty; the release will say it is $SHA" >&2
fi
case "$CHANNEL" in
  edge)
    BUILD="$(git rev-list --count HEAD)"
    VERSION="${VERSION:-$BASE.$BUILD}"
    ;;
  stable)
    VERSION="${VERSION:-$BASE}"
    if [[ "$VERSION" != "$BASE"* ]]; then
      echo "stable version $VERSION does not start with Cargo.toml's $BASE" >&2
      exit 1
    fi
    BUILD="$(git rev-list --count HEAD)"
    ;;
  *) echo "--channel must be edge or stable" >&2; exit 2 ;;
esac
echo "==> Arbos $VERSION ($CHANNEL) from ${SHA:0:7}"

# ---- credentials -----------------------------------------------------------
echo "==> reading credentials from 1Password"
umask 077
op read "op://$OP_VAULT/$OP_APPLE_ITEM/IDApple.p12" --out-file "$SECRETS/developer-id.p12" --force >/dev/null
# The item holds two concealed passwords; the first opens both .p12 files.
P12_PASSWORD="$(op item get "$OP_APPLE_ITEM" --vault "$OP_VAULT" --format json \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(f["value"] for f in d["fields"] if f.get("type")=="CONCEALED"))')"
if $NOTARIZE; then
  op read "op://$OP_VAULT/$OP_APPLE_ITEM/AuthKey_$NOTARY_KEY_ID.p8" --out-file "$SECRETS/notary.p8" --force >/dev/null
fi
op read "op://$OP_VAULT/$OP_SPARKLE_ITEM/credential" > "$SECRETS/sparkle.key"
if ! $DRY_RUN; then
  GH_TOKEN="$(op read "op://$OP_VAULT/$OP_GITHUB_ITEM/credential")"
  export GH_TOKEN
fi

eval "$(scripts/release/signing-keychain.sh import "$SECRETS/developer-id.p12" "$P12_PASSWORD")"
unset P12_PASSWORD
KEYCHAIN="$ARBOS_SIGNING_KEYCHAIN"
KEYCHAINS_WERE="$ARBOS_KEYCHAINS_WERE"
rm -f "$SECRETS/developer-id.p12"
echo "==> signing identity: $ARBOS_SIGN_IDENTITY"

# ---- build, sign, notarize, staple ------------------------------------------
DIST="$ROOT/dist"
rm -rf "$DIST"
export ARBOS_BUILD="$BUILD" ARBOS_SIGN_IDENTITY ARBOS_DIST="$DIST"
if $NOTARIZE; then
  export NOTARY_KEY_PATH="$SECRETS/notary.p8" NOTARY_KEY_ID NOTARY_ISSUER_ID
fi
scripts/bundle-mac.sh
# shellcheck disable=SC1091
source "$DIST/build-info.env"
if $NOTARIZE; then
  echo "==> Gatekeeper check"
  spctl -a -vv -t install "$ARBOS_DMG" || { echo "spctl rejected the dmg" >&2; exit 1; }
  spctl -a -vv -t exec "$DIST/Arbos.app" || { echo "spctl rejected the app" >&2; exit 1; }
fi

# ---- publish ---------------------------------------------------------------
NOTES="$DIST/notes.md"
git log -1 --format=%s > "$NOTES"
PUBLISH_ARGS=(--channel "$CHANNEL" --version "$VERSION" --sha "$SHA" --dist "$DIST" --repo "$REPO" --notes-file "$NOTES" --key-file "$SECRETS/sparkle.key")
if $DRY_RUN; then PUBLISH_ARGS+=(--dry-run); fi
scripts/release/publish-release.sh "${PUBLISH_ARGS[@]}"
echo "done: $VERSION ($CHANNEL) — dmg $ARBOS_DMG, payload $ARBOS_TARBALL"
