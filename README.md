# Arbos

Arbos is a messenger-style Mac app for talking to AI agents. Each Project has one project agent and its own cloud sandbox ("box"). This repository is the monorepo (design decision D33): the relay, the box daemon, the headless client, the pi extension, and the sandbox image.

## Layout

| Path | What |
| --- | --- |
| `crates/arbos-core` | Shared Rust types: ids, binary room frames, the Loro chat-doc schema, the `route` schema and validator, the pi/Prime RPC codec. No I/O. |
| `crates/arbos-relay` | The one service outside the boxes: project registry (SQLite), device pairing, sync rooms (opaque log + checkpoints), blob store, model proxy (OpenAI- and Anthropic-compatible passthrough with per-project metering and a daily spend cap), Daytona module (typed from the live OpenAPI), wake jobs, push queue. |
| `crates/arbos-agentd` | The box entrypoint: Loro doc host (a peer of the relay room, 120 ms commits, resume by cursor), SQLite+FTS5 history and memory tables, project MCP server on localhost, the pi main-agent driver (`route` tool validated with one retry, act limits 8 calls / 60 s with escalation), the `HarnessAdapter` registry (`PrimeAdapter` default, `PiAdapter`, `ClaudeCodeAdapter`), compaction and the memory write pass off the reply path. |
| `crates/arbosctl` | Headless client: pair, create project, open chat, send, stream, resume by cursor, stop/start box, usage, timings. |
| `crates/arbos-mac` | GPUI Mac client (separate branch). |
| `harness/arbos-pi-ext` | The pi extension (TypeScript, loaded by pi through jiti): `tool_call` deny+audit hook, act-call counting, `ask_user`, `route`, output offload to handles (> 4,000 chars), `peek`, `remember`/`recall`, working-set section. |
| `image/` | `Dockerfile` (the full `arbos-agent` image: pi + Prime + Claude Code + agent-browser/Chrome), `Dockerfile.dev-inline` (pi-only dev image built by Daytona's declarative builder), the D64 hook scripts (`arbos-shell`, Claude `PreToolUse`), Prime skills, pi seed config. |

## Prerequisites

- Rust stable (`rust-toolchain.toml` pins the channel; `rustup` installs it).
- Node 22.19+ (pi needs zstd in `zlib`; 22.14 is too old) and pnpm 9 for `harness/`.
- `pi` 1.2.0 (`npm i -g @earendil-works/pi-coding-agent@1.2.0`) for a local box; `prime-agent` 0.10.0 plus its kernel venv (see `image/Dockerfile`) for Prime subagents. Without Prime, set `ARBOS_DEFAULT_HARNESS=pi`.
- Secrets, only on the relay host: `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) for the model proxy and `DAYTONA_API_KEY` for real boxes. The box never sees them (D2).

## Build and check

```bash
cargo build --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd harness && pnpm install && pnpm -r typecheck
```

## Run locally (no Daytona, no Docker): the `process` box

The relay can run `arbos-agentd` as a child process on the same host. Every tool, the pi main agent, and the subagents run for real; only the sandbox boundary is missing.

```bash
# 1. relay (prints a one-time pairing code on first start)
export OPENROUTER_API_KEY=…
export ARBOS_AGENTD_BIN=target/debug/arbos-agentd ARBOS_PI_BIN=$(command -v pi) \
       ARBOS_PI_EXT=$PWD/harness/arbos-pi-ext/index.ts
cargo run -p arbos-relay -- --listen 127.0.0.1:7700 --data ./.arbos-local/relay

# 2. pair this machine
cargo run -p arbosctl -- pair <code>

# 3. a project with a local box
cargo run -p arbosctl -- project create --name demo --local-box

# 4. chat
cargo run -p arbosctl -- chat open demo                        # prints a chat id
cargo run -p arbosctl -- chat send demo <chat> "hello" --stream
cargo run -p arbosctl -- chat stream demo <chat> --until-done  # resumes by cursor after a kill
cargo run -p arbosctl -- box stop demo && cargo run -p arbosctl -- box start demo
cargo run -p arbosctl -- timings demo                          # boot, wake, TTFT, subagent timings
```

`arbosctl` keeps its device token and per-chat replicas under `~/.config/arbos/` (`ARBOSCTL_CONFIG` overrides). All commands: `cargo run -p arbosctl -- --help`.

## Run on Daytona

With `DAYTONA_API_KEY` set, `project create` (without `--local-box`) creates a real sandbox from the snapshot named by `ARBOS_DAYTONA_SNAPSHOT` (default `arbos-agent-0.1.0`). Until that snapshot is pushed, point the relay at the dev image instead:

```bash
export DAYTONA_API_KEY=… ARBOS_RELAY_PUBLIC_URL=https://<public-url-of-this-relay> \
       ARBOS_DAYTONA_DOCKERFILE=image/Dockerfile.dev-inline ARBOS_BOOTSTRAP_DIR=./dist ARBOS_DEFAULT_HARNESS=pi
# dist/ holds a static arbos-agentd (x86_64-unknown-linux-musl) and arbos-pi-ext.tar; the box fetches both at boot.
cargo run -p arbos-relay
cargo run -p arbosctl -- project create --name demo --provider daytona
```

`cargo run -p arbos-relay -- daytona-smoke` runs the D25 lifecycle check (`create → stop → start → archive → start → delete`) against the live API and prints a timing table.

## Image

```bash
cargo build --release -p arbos-agentd && mkdir -p dist && cp target/release/arbos-agentd dist/
docker buildx build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:dev .
```

## CI

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, and the harness typecheck.
