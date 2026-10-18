# Arbos secrets (D83 decision 2): the real values the owner switched on live in the workspace's
# secrets.env, written by agentd and rewritten within seconds of any switch, Pause all, delete or
# disconnect. `bash -c` reads it through BASH_ENV; this loader covers login shells (profile.d)
# and interactive ones (bash.bashrc), which re-read it before every prompt so the next command
# sees the current state.
__arbos_secrets_env() {
  local f="${ARBOS_SECRETS_ENV:-}"
  if [ -z "$f" ]; then
    for g in /work/*/.arbos/secrets.env; do
      [ -r "$g" ] && f=$g
    done
  fi
  [ -n "$f" ] && [ -r "$f" ] && . "$f"
  return 0
}
__arbos_secrets_env
case $- in
  *i*) PROMPT_COMMAND="__arbos_secrets_env${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
