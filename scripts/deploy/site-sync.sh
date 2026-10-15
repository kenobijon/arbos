#!/usr/bin/env bash
# Deploy the landing page from the repo: fetch `site/` at origin/main of github.com/unarbos/arbos
# (public, no token) into /var/lib/arbos-site/repo and swap it into the Caddy web root atomically.
# Touches ONLY the site directory — never the relay, the Caddyfile or /etc/arbos/download.caddy.
# Installed as /usr/local/bin/arbos-site-sync by scripts/deploy/relay-server.sh and run every 5
# minutes by arbos-site-sync.timer; run it by hand to deploy immediately:
#   sudo arbos-site-sync            # deploy origin/main
#   sudo SITE_REF=cursor/site-xai-style arbos-site-sync   # preview another branch
set -euo pipefail
REPO_URL="${SITE_REPO_URL:-https://github.com/unarbos/arbos.git}"
REF="${SITE_REF:-main}"
WWW_USER="${WWW_USER:-const}"
WEB_ROOT="${SITE_WEB_ROOT:-/home/$WWW_USER/arbos-www/site}"
WORK=/var/lib/arbos-site
STAMP="$WORK/deployed-sha"

mkdir -p "$WORK"
if [[ ! -d "$WORK/repo/.git" ]]; then
  git clone -q --filter=blob:none --no-checkout "$REPO_URL" "$WORK/repo"
  git -C "$WORK/repo" sparse-checkout set --cone site
fi
git -C "$WORK/repo" fetch -q --depth 1 origin "$REF"
SHA=$(git -C "$WORK/repo" rev-parse FETCH_HEAD)
if [[ -f "$STAMP" && "$(cat "$STAMP")" == "$SHA" && -d "$WEB_ROOT" ]]; then
  exit 0
fi
git -C "$WORK/repo" checkout -q --detach FETCH_HEAD
[[ -f "$WORK/repo/site/index.html" ]] || { echo "site-sync: $REF has no site/index.html; not deploying" >&2; exit 1; }

NEW="$WEB_ROOT.new.$$"
rm -rf "$NEW"
cp -a "$WORK/repo/site" "$NEW"
chown -R "$WWW_USER:$WWW_USER" "$NEW"
if [[ -d "$WEB_ROOT" ]]; then mv "$WEB_ROOT" "$WEB_ROOT.prev.$$"; fi
mv "$NEW" "$WEB_ROOT"
rm -rf "$WEB_ROOT.prev.$$"
echo "$SHA" > "$STAMP"
logger -t arbos-site-sync "deployed site/ from $REF @ ${SHA:0:12}"
echo "deployed site/ from $REF @ ${SHA:0:12} → $WEB_ROOT"
