#!/usr/bin/env bash
# Publish one Arbos build as a GitHub release and update the Sparkle appcast
# the app polls. Shared by .github/workflows/release.yml (publish job) and
# scripts/release/local-release.sh, so the two paths cannot drift.
#
#   scripts/release/publish-release.sh --channel edge --version 0.1.0.212 \
#       --sha <commit> --dist dist --repo unarbos/arbos [--notes-file notes.md] [--dry-run]
#
# Channels (D42, D44):
#   edge    every green push to main. Release `edge-<version>` (pre-release)
#           holds the payloads; the rolling release `edge` holds only
#           appcast.xml + latest.txt, so the feed URL never changes:
#             https://github.com/<repo>/releases/download/edge/appcast.xml
#   stable  a `v*` tag. Release `v<version>` (marked latest) holds payloads
#           and its own appcast.xml, reachable at the stable feed URL:
#             https://github.com/<repo>/releases/latest/download/appcast.xml
#
# Inputs in --dist (from scripts/bundle-mac.sh and the Linux job):
#   Arbos-<version>-macos-*.tar.gz        required: the updater payload
#   Arbos-<version>-macos-*.dmg           the download
#   arbos-mac-<version>-linux-*.tar.gz    optional: Linux desktop payload
#   arbos-{relay,agentd,arbosctl}-<version>-linux-*.tar.gz   optional
#   notarization-*.log                    optional: attached when unstapled
#
# Secrets: GH_TOKEN (contents: write) and the Ed25519 private key in
# ARBOS_SPARKLE_PRIVATE_KEY (or --key-file). Nothing secret is written to --dist.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CHANNEL="" VERSION="" SHA="" DIST="" REPO="" NOTES_FILE="" KEY_FILE="" DRY_RUN=false KEEP_EDGE=20
while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --sha) SHA="$2"; shift 2 ;;
    --dist) DIST="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --notes-file) NOTES_FILE="$2"; shift 2 ;;
    --key-file) KEY_FILE="$2"; shift 2 ;;
    --keep-edge) KEEP_EDGE="$2"; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
: "${CHANNEL:?--channel edge|stable}" "${VERSION:?--version}" "${SHA:?--sha}" "${DIST:?--dist}" "${REPO:?--repo owner/name}"
case "$CHANNEL" in edge|stable) ;; *) echo "--channel must be edge or stable" >&2; exit 2 ;; esac
if [[ -z "$KEY_FILE" && -z "${ARBOS_SPARKLE_PRIVATE_KEY:-}" ]]; then
  echo "no signing key: set ARBOS_SPARKLE_PRIVATE_KEY or pass --key-file" >&2
  exit 2
fi
DIST="$(cd "$DIST" && pwd)"

UPDATECTL="${ARBOS_UPDATECTL:-}"
if [[ -z "$UPDATECTL" ]]; then
  command -v cargo >/dev/null 2>&1 || export PATH="$HOME/.cargo/bin:$PATH"
  (cd "$ROOT" && cargo build --quiet --locked -p arbos-update --bin arbos-updatectl)
  UPDATECTL="$ROOT/target/debug/arbos-updatectl"
fi

gh_run() {
  if $DRY_RUN; then
    printf '[dry-run] gh'; printf ' %q' "$@"; printf '\n'
  else
    gh "$@"
  fi
}

case "$CHANNEL" in
  edge)   TAG="edge-$VERSION"; TITLE="Arbos $VERSION (edge)"; FEED_TAG="edge" ;;
  stable) TAG="v$VERSION";     TITLE="Arbos $VERSION";        FEED_TAG="" ;;
esac
DOWNLOAD="https://github.com/$REPO/releases/download/$TAG"
RELEASE_URL="https://github.com/$REPO/releases/tag/$TAG"

cd "$DIST"
shopt -s nullglob
mac_tar=(Arbos-"$VERSION"-macos-*.tar.gz)
mac_dmg=(Arbos-"$VERSION"-macos-*.dmg)
linux_desktop=(arbos-mac-"$VERSION"-linux-*.tar.gz)
linux_tools=(arbos-relay-"$VERSION"-linux-*.tar.gz arbos-agentd-"$VERSION"-linux-*.tar.gz arbosctl-"$VERSION"-linux-*.tar.gz)
notary_logs=(notarization-*.log)
if [[ ${#mac_tar[@]} -ne 1 ]]; then
  echo "expected exactly one Arbos-$VERSION-macos-*.tar.gz in $DIST, found ${#mac_tar[@]}" >&2
  exit 1
fi

# The appcast this build joins: the channel's current feed, if any.
PREVIOUS="$DIST/.previous-appcast.xml"
rm -f "$PREVIOUS"
if ! $DRY_RUN; then
  if [[ "$CHANNEL" == "edge" ]]; then
    gh release download edge --repo "$REPO" --pattern appcast.xml --output "$PREVIOUS" 2>/dev/null || true
  else
    latest="$(gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null || true)"
    if [[ -n "$latest" ]]; then
      gh release download "$latest" --repo "$REPO" --pattern appcast.xml --output "$PREVIOUS" 2>/dev/null || true
    fi
  fi
fi

NOTES_ARG=()
if [[ -n "$NOTES_FILE" ]]; then
  NOTES_ARG=(--notes-file "$NOTES_FILE")
fi
KEY_ARG=()
if [[ -n "$KEY_FILE" ]]; then
  KEY_ARG=(--key-file "$KEY_FILE")
fi
ENCLOSURES=(--enclosure "$DOWNLOAD/${mac_tar[0]}=${mac_tar[0]}=macos")
for f in "${linux_desktop[@]}"; do
  ENCLOSURES+=(--enclosure "$DOWNLOAD/$f=$f=linux")
done
PREV_ARG=()
if [[ -f "$PREVIOUS" ]]; then
  PREV_ARG=(--previous "$PREVIOUS")
fi
echo "==> appcast for $VERSION ($CHANNEL)"
"$UPDATECTL" appcast --version "$VERSION" --link "$RELEASE_URL" --title "Arbos ($CHANNEL)" \
  "${NOTES_ARG[@]}" "${KEY_ARG[@]}" "${ENCLOSURES[@]}" "${PREV_ARG[@]}" --out appcast.xml
rm -f "$PREVIOUS"
echo "==> verifying the appcast against the compiled-in public key"
"$UPDATECTL" verify-appcast --appcast appcast.xml --dir .
printf '%s\n' "$VERSION" > latest.txt

# Release notes: what, from where, how it is signed.
BODY="$DIST/.release-notes.md"
{
  printf '**Arbos %s** (%s) — built from [`%s`](https://github.com/%s/commit/%s).\n\n' "$VERSION" "$CHANNEL" "${SHA:0:7}" "$REPO" "$SHA"
  if [[ -n "$NOTES_FILE" ]]; then cat "$NOTES_FILE"; printf '\n\n'; fi
  if [[ -f build-info.env ]]; then
    # shellcheck disable=SC1091
    source build-info.env
    printf 'macOS: signing `%s`, notarized `%s`, stapled `%s`, %s.\n\n' "${ARBOS_SIGNING:-?}" "${ARBOS_NOTARIZED:-?}" "${ARBOS_STAPLED:-?}" "${ARBOS_ARCH:-universal}"
  fi
  if [[ "$CHANNEL" == "edge" ]]; then
    printf 'Edge channel: every green push to `main`. The app updates itself from <https://github.com/%s/releases/download/edge/appcast.xml>.\n' "$REPO"
  else
    printf 'Stable channel. The app updates itself from <https://github.com/%s/releases/latest/download/appcast.xml>.\n' "$REPO"
  fi
} > "$BODY"

ASSETS=("${mac_tar[@]}" "${mac_dmg[@]}" "${linux_desktop[@]}" "${linux_tools[@]}" "${notary_logs[@]}" appcast.xml latest.txt)
echo "==> release $TAG with: ${ASSETS[*]}"
FLAGS=(--repo "$REPO" --target "$SHA" --title "$TITLE" --notes-file "$BODY")
if [[ "$CHANNEL" == "edge" ]]; then FLAGS+=(--prerelease); else FLAGS+=(--latest); fi
if ! $DRY_RUN && gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "    (release exists; replacing assets)"
  gh_run release upload "$TAG" --repo "$REPO" --clobber "${ASSETS[@]}"
  gh_run release edit "$TAG" --repo "$REPO" --title "$TITLE" --notes-file "$BODY"
else
  gh_run release create "$TAG" "${FLAGS[@]}" "${ASSETS[@]}"
fi

if [[ -n "$FEED_TAG" ]]; then
  echo "==> feed: $FEED_TAG/appcast.xml"
  if ! $DRY_RUN && ! gh release view "$FEED_TAG" --repo "$REPO" >/dev/null 2>&1; then
    gh_run release create "$FEED_TAG" --repo "$REPO" --target "$SHA" --prerelease \
      --title "Arbos edge feed" \
      --notes "The in-app updater's feed for the edge channel. Only \`appcast.xml\` and \`latest.txt\` live here; the builds are the \`edge-*\` releases. Rewritten on every green push to \`main\`."
  fi
  gh_run release upload "$FEED_TAG" --repo "$REPO" --clobber appcast.xml latest.txt
  # Keep the newest edge builds; the appcast only names the last 10 anyway.
  if ! $DRY_RUN; then
    gh api --paginate "repos/$REPO/releases?per_page=100" \
      --jq '.[] | select(.tag_name | startswith("edge-")) | .tag_name' \
      | sort -t- -k2 -V | head -n -"$KEEP_EDGE" | while read -r old; do
        [[ -n "$old" ]] || continue
        echo "    pruning $old"
        gh release delete "$old" --repo "$REPO" --yes --cleanup-tag || true
      done
  fi
fi
rm -f "$BODY"
echo "published $TAG -> $RELEASE_URL"
