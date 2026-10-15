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
cargo run -p arbosctl -- chat stop demo <chat>                 # aborts the running turn at once
cargo run -p arbosctl -- box stop demo && cargo run -p arbosctl -- box start demo
cargo run -p arbosctl -- timings demo                          # boot, wake, TTFT, subagent timings
```

`arbosctl` keeps its device token and per-chat replicas under `~/.config/arbos/` (`ARBOSCTL_CONFIG` overrides). All commands: `cargo run -p arbosctl -- --help`.

What a chat shows: the user only ever sees prose from the main agent, written like a colleague in chat (what got done and the outcome, never how the tools were called; agentd also strips leading "Called N tools"/"Running …" lines and ```json blocks before text enters the doc). A hand-off (`new`/`wake`) is one plain sentence from the model, then a progress message with status events only (`working`, then `done`/`failed`), then a prose result turn (`turn.kind = "subagent_result"`) written by the main agent from the subagent's output. Status codes are `working`, `done`, `failed`, `stopped`, `waiting_user`; tool calls and raw subagent output are parts with `hidden: true` (Details drawer, not the transcript) and in T3. Up to 4 subagents run per box at once; more wait in FIFO order, and the chat stays responsive meanwhile.

Relay environment (all optional unless noted):

| Variable | Meaning |
| --- | --- |
| `ARBOS_RELAY_LISTEN` | bind address, default `127.0.0.1:7700`; use `0.0.0.0:7700` on a server |
| `ARBOS_RELAY_PUBLIC_URL` | the URL boxes and clients dial back (**required for Daytona boxes**), e.g. `https://relay.example.com` or a cloudflared/ngrok URL in front of the plain-HTTP relay; WebSockets pass through such tunnels |
| `ARBOS_RELAY_DATA` | SQLite + blobs + local boxes, default `./.arbos-local/relay` |
| `ARBOS_RELAY_ADMIN_TOKEN` | token for `POST /pair/new`; generated and logged when unset |
| `OPENROUTER_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | provider keys for the model proxy (never reach a box) |
| `ARBOS_OPENAI_BASE`, `ARBOS_ANTHROPIC_BASE` | upstreams, default OpenRouter |
| `DAYTONA_API_KEY`, `DAYTONA_API_URL`, `DAYTONA_ORG_ID` | Daytona access; without a key new projects get `process` boxes |
| `ARBOS_DAYTONA_SNAPSHOT` | snapshot for new boxes, default `arbos-agent-0.1.5` |
| `ARBOS_DAYTONA_DOCKERFILE`, `ARBOS_BOOTSTRAP_DIR` | dev-image path (see below) |
| `ARBOS_AGENTD_BIN`, `ARBOS_PI_BIN`, `ARBOS_PI_EXT`, `ARBOS_PRIME_BIN`, `ARBOS_DEFAULT_HARNESS` | process-box binaries and default subagent harness |
| `RUST_LOG` | e.g. `info,arbos_relay=debug` |

Example for a public relay on one machine (the Daytona boxes dial `https://…` back): `ARBOS_RELAY_LISTEN=0.0.0.0:7700 ARBOS_RELAY_PUBLIC_URL=https://relay.example.com cargo run --release -p arbos-relay`, with TLS terminated by a reverse proxy or a cloudflared tunnel. The relay itself speaks plain HTTP/WS.

`ARBOS_RELAY_PUBLIC_URL` must be stable for persistent projects. A cloudflared *quick* tunnel (`cloudflared tunnel --url …`, a random `*.trycloudflare.com` name) changes on every restart and is dev-only; use a named tunnel or the machine's own IP/hostname. The relay tolerates a change: at every wake it writes the current URL into the box (`/work/.arbos/relay-url`, Daytona toolbox API) and agentd prefers that file over the URL baked into the sandbox env; a box that still cannot say hello within 60 s is bounced once and then shown as `error · box cannot reach relay` instead of waking forever (an explicit wake/start tries again).

`scripts/relay-start.sh` starts the relay with the tunnel beside it:

```bash
ARBOS_TUNNEL=named scripts/relay-start.sh   # https://relay.arbos.life, stable; token from ARBOS_TUNNEL_TOKEN or 1Password
ARBOS_TUNNEL=quick scripts/relay-start.sh   # dev only: a fresh *.trycloudflare.com URL
ARBOS_TUNNEL=none  ARBOS_RELAY_PUBLIC_URL=https://relay.example scripts/relay-start.sh
```

The named tunnel is the Cloudflare Zero Trust tunnel `arbos-relay` (remotely managed: ingress `relay.arbos.life → http://127.0.0.1:7700`). Its connector token lives in the Arbos vault ("Cloudflare — relay named tunnel token … env ARBOS_TUNNEL_TOKEN"); `ARBOS_TUNNEL_HOSTNAME` changes the hostname, `ARBOS_TUNNEL_OP_ITEM` the vault item. Creating a tunnel and its DNS record needs a Cloudflare API token with `Account → Cloudflare Tunnel → Edit` and `Zone → DNS → Edit` on the zone.

## Run on Daytona

With `DAYTONA_API_KEY` set, `project create` (without `--local-box`) creates a real sandbox from the snapshot named by `ARBOS_DAYTONA_SNAPSHOT` (default `arbos-agent-0.1.5`, the current registered build). The relay must be reachable from the box (`ARBOS_RELAY_PUBLIC_URL`). If no production snapshot is registered yet, point the relay at the dev image instead:

```bash
export DAYTONA_API_KEY=… ARBOS_RELAY_PUBLIC_URL=https://<public-url-of-this-relay> \
       ARBOS_DAYTONA_DOCKERFILE=image/Dockerfile.dev-inline ARBOS_BOOTSTRAP_DIR=./dist ARBOS_DEFAULT_HARNESS=pi
# dist/ holds a static arbos-agentd (x86_64-unknown-linux-musl) and arbos-pi-ext.tar; the box fetches both at boot.
cargo run -p arbos-relay
cargo run -p arbosctl -- project create --name demo --provider daytona
```

`cargo run -p arbos-relay -- daytona-smoke` runs the D25 lifecycle check (`create → stop → start → archive → start → delete`) against the live API and prints a timing table.

## Image

`.github/workflows/image.yml` builds `image/Dockerfile` on an image tag (`image-v0.1.5`) or by hand, pushes it to GHCR, smoke-tests it, and registers the Daytona snapshot `arbos-agent-<version>` with `image/register-snapshot.sh` (secrets `DAYTONA_API_KEY`, `GHCR_READ_TOKEN`).

Without CI (unpublished repo) the same build runs on any Docker host, including a Daytona Docker-in-Docker sandbox (`FROM docker:27-dind`, start `dockerd-entrypoint.sh`, upload the build context over SSH), then goes to Daytona's transient registry:

```bash
# static agentd for the image
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release -p arbos-agentd --target x86_64-unknown-linux-musl
mkdir -p dist && cp target/x86_64-unknown-linux-musl/release/arbos-agentd dist/
docker build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:0.1.5 .
# temporary push credentials: GET $DAYTONA_API_URL/docker-registry/registry-push-access?snapshotName=arbos-agent-0.1.5
docker tag arbos-agent:0.1.5 cr.app.daytona.io/<project>/arbos-agent:0.1.5 && docker push cr.app.daytona.io/<project>/arbos-agent:0.1.5
image/register-snapshot.sh arbos-agent-0.1.5 cr.app.daytona.io/<project>/arbos-agent:0.1.5
```

Transient-registry tags are immutable: bump the version for every rebuild. The Arbos layers are last in the Dockerfile, so a code change rebuilds in seconds against the cached toolchain layers.

## CI

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, and the harness typecheck.
