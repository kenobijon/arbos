#!/usr/bin/env bash
# Shared checks for the landing scripts. Sourced, not run.
#
# Both scripts land `cursor/m1-integration` on unarbos/arbos `main`. They never run without
# CONFIRM=yes, they always save the current main to a backup branch first, and they stop on any
# surprise (dirty tree, wrong remote, backup already there, main moved since the dry run).
#
# Environment:
#   CONFIRM=yes                  required; anything else prints the plan and exits 0
#   CANONICAL_REMOTE=canonical   git remote that points at https://github.com/unarbos/arbos.git
#   SOURCE_BRANCH=cursor/m1-integration   local branch (or remote ref) to land
#   BACKUP_BRANCH=backup/main-2026-10-15  where today's main goes first
#   EXPECTED_MAIN_SHA=<sha>      optional: refuse if canonical main is not at this commit
set -euo pipefail

CANONICAL_REMOTE=${CANONICAL_REMOTE:-canonical}
SOURCE_BRANCH=${SOURCE_BRANCH:-cursor/m1-integration}
BACKUP_BRANCH=${BACKUP_BRANCH:-backup/main-2026-10-15}
EXPECTED_MAIN_SHA=${EXPECTED_MAIN_SHA:-}

say() { printf '\n== %s\n' "$*"; }
die() { printf 'landing: %s\n' "$*" >&2; exit 1; }

preflight() {
  git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "run from inside the repo"
  [ -z "$(git status --porcelain)" ] || die "working tree is not clean"
  local url
  url=$(git remote get-url "$CANONICAL_REMOTE" 2>/dev/null) || die "remote $CANONICAL_REMOTE is missing (git remote add $CANONICAL_REMOTE https://github.com/unarbos/arbos.git)"
  case "$url" in
    *github.com[:/]unarbos/arbos*) ;;
    *) die "remote $CANONICAL_REMOTE points at $url, not unarbos/arbos" ;;
  esac
  say "fetching $CANONICAL_REMOTE"
  git fetch "$CANONICAL_REMOTE" --prune
  git rev-parse --verify -q "$SOURCE_BRANCH" >/dev/null || die "source branch $SOURCE_BRANCH not found"
  MAIN_SHA=$(git rev-parse "$CANONICAL_REMOTE/main")
  SOURCE_SHA=$(git rev-parse "$SOURCE_BRANCH")
  if [ -n "$EXPECTED_MAIN_SHA" ] && [ "$MAIN_SHA" != "$EXPECTED_MAIN_SHA" ]; then
    die "canonical main is $MAIN_SHA, expected $EXPECTED_MAIN_SHA; re-check before landing"
  fi
  if git ls-remote --exit-code --heads "$CANONICAL_REMOTE" "$BACKUP_BRANCH" >/dev/null 2>&1; then
    die "backup branch $BACKUP_BRANCH already exists on $CANONICAL_REMOTE; pick another BACKUP_BRANCH"
  fi
}

plan() {
  say "plan ($1)"
  printf '  canonical main : %s  %s\n' "$MAIN_SHA" "$(git log -1 --format='%s' "$MAIN_SHA")"
  printf '  source         : %s  %s (%s)\n' "$SOURCE_SHA" "$(git log -1 --format='%s' "$SOURCE_SHA")" "$SOURCE_BRANCH"
  printf '  backup         : %s/%s <- current main\n' "$CANONICAL_REMOTE" "$BACKUP_BRANCH"
  printf '  then           : %s\n' "$2"
}

require_confirm() {
  if [ "${CONFIRM:-}" != "yes" ]; then
    printf '\nDry run. Nothing was pushed. Re-run with CONFIRM=yes to land.\n'
    exit 0
  fi
}

backup_main() {
  say "saving current main as $BACKUP_BRANCH"
  git push "$CANONICAL_REMOTE" "$MAIN_SHA:refs/heads/$BACKUP_BRANCH"
  git ls-remote --exit-code --heads "$CANONICAL_REMOTE" "$BACKUP_BRANCH" >/dev/null || die "backup push did not land"
}
