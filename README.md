# Arbos

Arbos is a messenger-style Mac app for talking to AI agents. Each Project has one project agent and its own cloud sandbox ("box"). This repository is the monorepo (design decision D33): the relay, the box daemon, the headless client, the pi extension, and the sandbox image.

## Layout

| Path | What |
| --- | --- |
| `crates/arbos-core` | Shared Rust types: ids, binary room frames, the Loro chat-doc schema, the `route` schema and validator, the pi/Prime RPC codec. No I/O. |
| `crates/arbos-relay` | The one service outside the boxes: project registry (SQLite), device pairing, sync rooms (opaque log + checkpoints), blob store, model proxy (OpenAI- and Anthropic-compatible passthrough with per-project metering and a daily spend cap), Daytona module (typed from the live OpenAPI), wake jobs, push queue. |
| `crates/arbos-agentd` | The box entrypoint: Loro doc host (a peer of the relay room, 120 ms commits, resume by cursor), SQLite+FTS5 history and memory tables, project MCP server on localhost, the pi main-agent driver (`route` tool validated with one retry, act limits 8 calls / 60 s with escalation), the `HarnessAdapter` registry (`PrimeAdapter` default, `PiAdapter`, `ClaudeCodeAdapter`), compaction and the memory write pass off the reply path. |
| `crates/arbosctl` | Headless client: pair, create project, open chat, send, stream, resume by cursor, stop/start box, usage, timings. |
| `crates/arbos-mac` | The GPUI desktop client (zeronsh/zui fork): project rail, tabs, Cursor-style chat, composer; `FixtureSource` (local Loro docs, scripted agent) and `RelaySource` (WebSocket to relay rooms). Builds on macOS and Linux. |
| `harness/arbos-pi-ext` | The pi extension (TypeScript, loaded by pi through jiti): `tool_call` deny+audit hook, act-call counting, `ask_user`, `route`, output offload to handles (> 4,000 chars), `peek`, `remember`/`recall`, working-set section. |
| `image/` | `Dockerfile` (the full `arbos-agent` image: pi + Prime + Claude Code + agent-browser/Chrome), `Dockerfile.dev-inline` (pi-only dev image built by Daytona's declarative builder), the D64 hook scripts (`arbos-shell`, Claude `PreToolUse`), Prime skills, pi seed config. |

## Prerequisites

- Rust stable (`rust-toolchain.toml` pins the channel; `rustup` installs it).
- Node 22.19+ (pi needs zstd in `zlib`; 22.14 is too old) and pnpm 9 for `harness/`.
- `pi` 1.2.0 (`npm i -g @earendil-works/pi-coding-agent@1.2.0`) for a local box; `prime-agent` 0.10.0 plus its kernel venv (see `image/Dockerfile`) for Prime subagents. Without Prime, set `ARBOS_DEFAULT_HARNESS=pi`.
- Secrets, only on the relay host: `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) for the model proxy and `DAYTONA_API_KEY` for real boxes. The box never sees them (D2).
- For `arbos-mac` on Linux: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb-xkb-dev libasound2-dev libssl-dev libgl1-mesa-dev libfontconfig-dev` (and `mesa-vulkan-drivers` to run without a GPU).

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
| `ARBOS_DAYTONA_SNAPSHOT` | snapshot for new boxes, default `arbos-agent-0.1.4` |
| `ARBOS_DAYTONA_DOCKERFILE`, `ARBOS_BOOTSTRAP_DIR` | dev-image path (see below) |
| `ARBOS_AGENTD_BIN`, `ARBOS_PI_BIN`, `ARBOS_PI_EXT`, `ARBOS_PRIME_BIN`, `ARBOS_DEFAULT_HARNESS` | process-box binaries and default subagent harness |
| `RUST_LOG` | e.g. `info,arbos_relay=debug` |

Example for a public relay on one machine (the Daytona boxes dial `https://…` back): `ARBOS_RELAY_LISTEN=0.0.0.0:7700 ARBOS_RELAY_PUBLIC_URL=https://relay.example.com cargo run --release -p arbos-relay`, with TLS terminated by a reverse proxy or `cloudflared tunnel --url http://127.0.0.1:7700`. The relay itself speaks plain HTTP/WS.

## Run on Daytona

With `DAYTONA_API_KEY` set, `project create` (without `--local-box`) creates a real sandbox from the snapshot named by `ARBOS_DAYTONA_SNAPSHOT` (default `arbos-agent-0.1.4`, the current registered build). The relay must be reachable from the box (`ARBOS_RELAY_PUBLIC_URL`). If no production snapshot is registered yet, point the relay at the dev image instead:

```bash
export DAYTONA_API_KEY=… ARBOS_RELAY_PUBLIC_URL=https://<public-url-of-this-relay> \
       ARBOS_DAYTONA_DOCKERFILE=image/Dockerfile.dev-inline ARBOS_BOOTSTRAP_DIR=./dist ARBOS_DEFAULT_HARNESS=pi
# dist/ holds a static arbos-agentd (x86_64-unknown-linux-musl) and arbos-pi-ext.tar; the box fetches both at boot.
cargo run -p arbos-relay
cargo run -p arbosctl -- project create --name demo --provider daytona
```

`cargo run -p arbos-relay -- daytona-smoke` runs the D25 lifecycle check (`create → stop → start → archive → start → delete`) against the live API and prints a timing table.

## Run the desktop client

```bash
cargo run -p arbos-mac                                  # fixture source: seeded chats + scripted agent
ARBOS_RELAY_URL=ws://127.0.0.1:7700 ARBOS_RELAY_TOKEN=... cargo run -p arbos-mac   # relay rooms
```

First run against a relay shows the pairing screen: enter the one-time code the relay logged. The device token goes to the login keychain on macOS (`security`), or `~/.config/arbos/` (0600) elsewhere; `ARBOS_RELAY_TOKEN` overrides. Matte is the default theme; Glass = macOS vibrancy + in-window frosted panels (the frost also renders on Linux). `ARBOS_SURFACE=glass` starts in Glass.

macOS packaging: `scripts/bundle-mac.sh` builds a universal `dist/Arbos.app` + DMG and signs/notarizes when `ARBOS_SIGN_IDENTITY` / `ARBOS_NOTARY_PROFILE` are set (skipped otherwise). `scripts/demo-mac.sh` launches the app in Glass, runs a scripted turn and records with `screencapture -v`.

## Connectors

The relay brokers Gmail/Calendar, Slack, GitHub and Harness Comms for the boxes (D2/D28/D40): provider tokens are stored AES-256-GCM-encrypted in the relay DB (key from `ARBOS_RELAY_KEY`, else generated once into the `meta` table) and never enter a box. The agent calls `connectors_list` / `connector_call` over MCP; agentd forwards to `POST /connectors/call`, which attaches the bearer upstream and redacts `access_token` / `refresh_token` / `Authorization` from every reply. Write and send actions go through an approval card first (`ARBOS_APPROVAL_STUB=auto|expire|reject` until the approvals feature lands).

- Device endpoints: `GET /connectors`, `GET /connectors/catalog`, `POST /connectors/{provider}/connect` (`{"token": ...}` for GitHub PATs / Harness Comms keys, otherwise returns an OAuth `authUrl`), `GET /connectors/oauth/callback`, `DELETE /connectors/{id}`, `GET /connectors/outbox`.
- OAuth apps: `ARBOS_GOOGLE_CLIENT_ID/SECRET`, `ARBOS_SLACK_CLIENT_ID/SECRET`, `ARBOS_GITHUB_CLIENT_ID/SECRET`; redirect is `{public_url}/connectors/oauth/callback` (`ARBOS_OAUTH_REDIRECT_URL` overrides). `ARBOS_HARNESS_COMMS_URL` is the default base URL for the Harness Comms connector.
- `ARBOS_CONNECTORS_MOCK=1`: every action returns canned data, OAuth completes against the relay's own callback, and sends land in `connector_outbox` instead of leaving the machine. The Mac fixture source shows the same panel under Settings → Connectors.

## Routines

A routine is a prompt that runs while you are away: on a cron schedule, when a webhook is called, or on an event (`cross_project_message`, `connector_event`, `box_awake`). The relay scheduler (15 s tick) wakes the box and queues a `routine_run` push; agentd posts the prompt into the routine's chat as a user message with `sender: routine:<id>`, runs a normal turn and reports the run back (`queued → running → done | failed`; runs the box never finishes are failed after 30 min). No new environment variables.

```bash
arbosctl routines create <project> --name "Stand-up" --prompt "Summarise what changed since yesterday" --cron "0 9 * * 1-5" --tz Europe/Berlin
arbosctl routines create <project> --name "Deploy finished" --prompt "Check the deploy and report" --webhook   # prints the fire URL
arbosctl routines list <project>            # trigger, enabled, last/next run
arbosctl routines run <project> <routine>   # fire now (also works while paused)
arbosctl routines runs <project> <routine>  # history
arbosctl routines pause|resume|delete <project> <routine>
```

Webhook routines fire with `POST <relay>/routines/<id>/fire?secret=<secret>`; a JSON body is appended to the prompt as trigger context. In the desktop client the same lives under Settings → Routines (list, presets, pause/resume, run now, history).

## Image

`.github/workflows/image.yml` builds `image/Dockerfile` on an image tag (`image-v0.1.4`) or by hand, pushes it to GHCR, smoke-tests it, and registers the Daytona snapshot `arbos-agent-<version>` with `image/register-snapshot.sh` (secrets `DAYTONA_API_KEY`, `GHCR_READ_TOKEN`).

Without CI (unpublished repo) the same build runs on any Docker host, including a Daytona Docker-in-Docker sandbox (`FROM docker:27-dind`, start `dockerd-entrypoint.sh`, upload the build context over SSH), then goes to Daytona's transient registry:

```bash
# static agentd for the image
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release -p arbos-agentd --target x86_64-unknown-linux-musl
mkdir -p dist && cp target/x86_64-unknown-linux-musl/release/arbos-agentd dist/
docker build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:0.1.4 .
# temporary push credentials: GET $DAYTONA_API_URL/docker-registry/registry-push-access?snapshotName=arbos-agent-0.1.4
docker tag arbos-agent:0.1.4 cr.app.daytona.io/<project>/arbos-agent:0.1.4 && docker push cr.app.daytona.io/<project>/arbos-agent:0.1.4
image/register-snapshot.sh arbos-agent-0.1.4 cr.app.daytona.io/<project>/arbos-agent:0.1.4
```

Transient-registry tags are immutable: bump the version for every rebuild. The Arbos layers are last in the Dockerfile, so a code change rebuilds in seconds against the cached toolchain layers.

## CI

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, and the harness typecheck.
