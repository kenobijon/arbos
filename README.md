# Arbos

Arbos is a messenger-style Mac app for talking to AI agents. Each Project has one project agent and its own cloud sandbox ("box"). This repository is the monorepo (design decision D33): the relay, the box daemon, the headless client, the pi extension, and the sandbox image.

## Layout

| Path | What |
| --- | --- |
| `crates/arbos-core` | Shared Rust types: ids, binary room frames, the Loro chat-doc schema, the `route` schema and validator, the pi/Prime RPC codec. No I/O. |
| `crates/arbos-relay` | The one service outside the boxes: project registry (SQLite), device pairing, sync rooms (opaque log + checkpoints), blob store, model proxy (OpenAI- and Anthropic-compatible passthrough with per-project metering), Daytona module, push queue for sleeping boxes. |
| `crates/arbos-agentd` | The box entrypoint: Loro doc host (a peer of the relay room), SQLite+FTS5 history and memory tables, project MCP server on localhost, the pi main-agent driver, the `HarnessAdapter` registry (`PrimeAdapter` default), compaction and the memory write pass. |
| `crates/arbosctl` | Headless client: pair, create project, open chat, send, stream, resume by cursor, stop/start box. |
| `crates/arbos-mac` | GPUI Mac client (separate worker; not in this branch). |
| `harness/arbos-pi-ext` | The pi extension (TypeScript): `tool_call` deny+audit hook, `ask_user`, `route`, output offload to handles, `peek`, working-set injection. |
| `image/` | The `arbos-agent` Dockerfile, the D64 hook scripts (`arbos-shell`, Claude Code `PreToolUse`), Prime skills, pi seed config. |

## Prerequisites

- Rust stable (`rust-toolchain.toml` pins the channel; `rustup` installs it).
- Node 22+ and pnpm 9 for `harness/`.
- Docker, only for `--local-box` mode and image builds.
- Secrets, only for real model calls: `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY`) and `DAYTONA_API_KEY` in the relay's environment. The box never sees them.

## Build

```bash
cargo build --workspace
cd harness && pnpm install && pnpm -r typecheck
```

## Run locally (no Daytona)

```bash
# 1. relay
OPENROUTER_API_KEY=... cargo run -p arbos-relay -- --listen 127.0.0.1:7700 --data ./.arbos-local/relay

# 2. pair a device (prints a one-time code on the relay side)
cargo run -p arbos-relay -- --data ./.arbos-local/relay pair-code
cargo run -p arbosctl -- pair <code>

# 3. create a project; --local-box runs the agentd image with Docker instead of Daytona
cargo run -p arbosctl -- project create --name demo --local-box

# 4. chat
cargo run -p arbosctl -- chat open <project>
cargo run -p arbosctl -- chat send <project> <chat> "hello"
cargo run -p arbosctl -- chat stream <project> <chat>     # resumes by cursor after a kill
cargo run -p arbosctl -- box stop <project> ; cargo run -p arbosctl -- box start <project>
```

Details for each command: `cargo run -p arbosctl -- --help`.

## Image

```bash
cargo build --release -p arbos-agentd && mkdir -p dist && cp target/release/arbos-agentd dist/
docker buildx build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:dev .
```

## CI

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, and the harness typecheck.
