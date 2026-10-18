# shellcheck shell=bash
# Arbos secrets (D83 decision 2): the real values the owner switched on live in the workspace's
# secrets.env, written by agentd and rewritten within seconds of any switch, Pause all, delete or
# disconnect. `bash -c` reads it through BASH_ENV; this loader covers login shells (profile.d)
# and interactive ones (bash.bashrc), which re-read it before every prompt so the next command
# sees the current state.
__arbos_secrets_env() {
  local f="${ARBOS_SECRETS_ENV:-}"
  # A workspace user's HOME is <state>/home, so its file is one level up; root (and a box without
  # per-workspace users) can list /work and takes every workspace's file.
  if [ -z "$f" ] && [ -r "${HOME:-/nonexistent}/../secrets.env" ]; then
    f="$HOME/../secrets.env"
  fi
  if [ -z "$f" ]; then
    for g in /work/*/.arbos/secrets.env; do
      [ -r "$g" ] && f=$g
    done
  fi
  # shellcheck source=/dev/null
  [ -n "$f" ] && [ -r "$f" ] && . "$f"
  return 0
}
__arbos_secrets_env
case $- in
  *i*) PROMPT_COMMAND="__arbos_secrets_env${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
