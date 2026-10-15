#!/usr/bin/env bash
# Import a Developer ID .p12 into a throwaway keychain for one build, and
# remove it after. Used by .github/workflows/release.yml and
# scripts/release/local-release.sh so a certificate never lands in the login
# keychain of a shared machine (the EC2 Mac is shared with the iOS lane).
#
#   eval "$(scripts/release/signing-keychain.sh import cert.p12 "$P12_PASSWORD")"
#     -> prints `ARBOS_SIGN_IDENTITY=…` and `ARBOS_SIGNING_KEYCHAIN=…` lines
#   scripts/release/signing-keychain.sh remove "$ARBOS_SIGNING_KEYCHAIN"
#
# No `set -x` anywhere here: it would print the password.
set -euo pipefail
set +x

cmd="${1:-}"
case "$cmd" in
  import)
    p12="${2:?path to .p12}"
    password="${3:?p12 password}"
    umask 077
    keychain="${TMPDIR:-/tmp}/arbos-signing-$(uuidgen).keychain-db"
    keychain_password="$(openssl rand -hex 32)"
    security create-keychain -p "$keychain_password" "$keychain"
    # No auto-lock, no timeout: codesign runs minutes after the import.
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$keychain_password" "$keychain"
    security import "$p12" -P "$password" -A -t cert -f pkcs12 -k "$keychain" >/dev/null
    # Without this, codesign stops on a GUI prompt nobody can answer.
    security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null
    were="$(security list-keychains -d user | xargs)"
    # shellcheck disable=SC2086
    security list-keychains -d user -s "$keychain" $were
    identities="$(security find-identity -v -p codesigning "$keychain" | sed -n 's/.*"\(.*\)".*/\1/p')"
    identity="$(printf '%s\n' "$identities" | grep '^Developer ID Application:' | head -1 || true)"
    if [[ -z "$identity" ]]; then
      identity="$(printf '%s\n' "$identities" | head -1 || true)"
    fi
    if [[ -z "$identity" ]]; then
      echo "the .p12 imported but holds no code-signing identity" >&2
      security delete-keychain "$keychain" || true
      exit 1
    fi
    printf 'ARBOS_SIGN_IDENTITY=%q\n' "$identity"
    printf 'ARBOS_SIGNING_KEYCHAIN=%q\n' "$keychain"
    printf 'ARBOS_KEYCHAINS_WERE=%q\n' "$were"
    ;;
  remove)
    keychain="${2:-}"
    were="${3:-${ARBOS_KEYCHAINS_WERE:-}}"
    if [[ -n "$were" ]]; then
      # shellcheck disable=SC2086
      security list-keychains -d user -s $were || true
    fi
    if [[ -n "$keychain" && -e "$keychain" ]]; then
      security delete-keychain "$keychain" || true
      rm -f "$keychain"
      echo "signing keychain removed"
    fi
    ;;
  *)
    echo "usage: $0 import <cert.p12> <password> | remove <keychain> [previous keychain list]" >&2
    exit 2
    ;;
esac
