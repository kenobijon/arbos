#!/bin/bash
# Rust pre-merge gate, run on the EC2 Mac by the feedback poller over ssh. Asynchronous, because a
# release build of arbos-mac takes longer than a poller tick:
#   rust-gate.sh start  <branch> <sha> <crates: comma list | workspace> <release-mac: 0|1>
#   rust-gate.sh status <sha>        -> first line running|ok|fail|conflict|infra, then the filtered log
#   rust-gate.sh abort  <sha>
# `start` fetches the branch into ~/arbos-rust-gate (its own clone; ~/arbos-gate belongs to the iOS gate),
# checks out <sha> detached, merges current origin/main into it (what will actually land), then runs
# what CI runs for the touched crates: cargo fmt --check, cargo clippy --all-targets -D warnings,
# cargo test, and — when crates/arbos-mac is touched — the release build of arbos-mac. One run at a
# time; the poller serialises items.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
runs=$HOME/arbos-rust-gate-runs
repo=$HOME/arbos-rust-gate
mkdir -p "$runs"
cmd=${1:-}; shift || true

case "$cmd" in
  start)
    branch=$1; sha=$2; crates=${3:-workspace}; mac=${4:-0}
    log=$runs/$sha.log; rc=$runs/$sha.rc
    rm -f "$log" "$rc"
    # Not nohup: BSD nohup exits with "can't detach from console" when ssh gives it no tty. Job control
    # gives the run its own process group, so `abort` can kill cargo and every rustc under it.
    set -m
    (trap '' HUP; exec bash "$0" _run "$branch" "$sha" "$crates" "$mac") >"$log" 2>&1 </dev/null &
    echo "$!" >"$runs/$sha.pid"
    disown
    set +m
    echo started
    ;;
  status)
    sha=$1; log=$runs/$sha.log; rc=$runs/$sha.rc
    if [ ! -f "$log" ]; then echo infra; echo "no run for $sha"; exit 0; fi
    if [ ! -f "$rc" ]; then echo running; tail -n 3 "$log"; exit 0; fi
    case "$(cat "$rc")" in 0) echo ok;; 1) echo fail;; 3) echo conflict;; *) echo infra;; esac
    # Compiler/fmt/test diagnostics only; paths relative to the repo.
    grep -E "^(error|warning)(\[|:)|^\s+--> |^Diff in |^test .* FAILED|^failures:|panicked at|^== " "$log" \
      | sed "s#$repo/##g" | head -n 150
    ;;
  abort)
    sha=$1
    if [ -f "$runs/$sha.pid" ]; then
      kill -TERM -- "-$(cat "$runs/$sha.pid")" 2>/dev/null
      sleep 2
      kill -KILL -- "-$(cat "$runs/$sha.pid")" 2>/dev/null
    fi
    echo 2 >"$runs/$sha.rc"
    echo aborted
    ;;
  _run)
    branch=$1; sha=$2; crates=$3; mac=$4
    rc=$runs/$sha.rc
    finish() { echo "$1" >"$rc"; exit "$1"; }
    cd "$repo" || finish 2
    git merge --abort 2>/dev/null
    git checkout -q -f --detach HEAD 2>/dev/null
    git fetch -q origin "+refs/heads/main:refs/remotes/origin/main" "+refs/heads/${branch}:refs/remotes/origin/${branch}" \
      || { echo "fetch of ${branch} failed"; finish 2; }
    git checkout -q -f --detach "$sha" || { echo "checkout of ${sha} failed"; finish 2; }
    echo "== merging origin/main ($(git rev-parse --short origin/main)) into ${sha:0:8}"
    if ! git -c user.name=gate -c user.email=gate@arbos.local merge -q --no-edit origin/main; then
      git diff --name-only --diff-filter=U | sed 's/^/conflict: /'
      git merge --abort 2>/dev/null
      finish 3
    fi
    pkgs=()
    if [ "$crates" = "workspace" ]; then
      pkgs=(--workspace)
    else
      IFS=, read -r -a names <<<"$crates"
      for n in "${names[@]}"; do pkgs+=(-p "$n"); done
    fi
    status=0
    echo "== cargo fmt --all --check"
    cargo fmt --all --check || { echo "== fmt: FAILS"; status=1; }
    echo "== cargo clippy ${pkgs[*]} --all-targets --locked -- -D warnings"
    if cargo clippy "${pkgs[@]}" --all-targets --locked -- -D warnings; then
      echo "== clippy: passes"
    else
      echo "== clippy: FAILS"; status=1
    fi
    if [ "$status" -eq 0 ]; then
      echo "== cargo test ${pkgs[*]} --locked"
      if cargo test "${pkgs[@]}" --locked; then echo "== test: passes"; else echo "== test: FAILS"; status=1; fi
    fi
    if [ "$status" -eq 0 ] && [ "$mac" = "1" ]; then
      echo "== cargo build --release --locked -p arbos-mac"
      if cargo build --release --locked -p arbos-mac; then echo "== release build: passes"; else echo "== release build: FAILS"; status=1; fi
    fi
    finish "$status"
    ;;
  *)
    echo "usage: rust-gate.sh start <branch> <sha> <crates|workspace> <0|1> | status <sha> | abort <sha>" >&2
    exit 2
    ;;
esac
