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
cargo run -p arbosctl -- chat answer demo <chat> --answer-json '"nightly"' --stream   # answer the open ask_user question
cargo run -p arbosctl -- box stop demo && cargo run -p arbosctl -- box start demo
cargo run -p arbosctl -- timings demo                          # boot, wake, TTFT, subagent timings
```

`arbosctl` keeps its device token and per-chat replicas under `~/.config/arbos/` (`ARBOSCTL_CONFIG` overrides). All commands: `cargo run -p arbosctl -- --help`.

Questions and widgets: `ask_user(question, widget?)` is the one way an agent asks the user something. Without a widget the question is plain and the composer answers it. With a widget (`choice`, `multi_select`, `text`, `date`, `form` with up to 8 fields) the chat gets a `question` part the clients render as a card; the answer goes back as an `answer` command `{questionId, value}` and agentd renders it to text for the model (`Answer: nightly`, or one bullet per form field). `arbosctl chat answer … --answer-json <json>` answers the newest open question from a terminal (`--question <id>` picks one). All three harness seams (agentd MCP/hook, the pi extension, the Prime `arbos_ask_user` skill) take the same widget shape.

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

## Run the desktop client

```bash
cargo run -p arbos-mac                                  # fixture source: seeded chats + scripted agent
ARBOS_RELAY_URL=ws://127.0.0.1:7700 ARBOS_RELAY_TOKEN=... cargo run -p arbos-mac   # relay rooms
```

First run against a relay shows the pairing screen: enter the one-time code the relay logged. The device token goes to the login keychain on macOS (`security`), or `~/.config/arbos/` (0600) elsewhere; `ARBOS_RELAY_TOKEN` overrides. Matte is the default theme; Glass = macOS vibrancy + in-window frosted panels (the frost also renders on Linux). `ARBOS_SURFACE=glass` starts in Glass.

Widget cards answer with the mouse or the keyboard (arrows move, space toggles, Enter submits); the fixture's "Beta release" chat under Arbos shows every widget type. System notifications (macOS `osascript`, Linux `notify-send`) fire for turn finished, needs input, approval requested, wake failed, spend cap reached, cross-project message and routine ran — at most one per project per minute (later ones fold into "N updates in <Project>"), never while that project is streaming, and only while the window is unfocused or the chat is not on screen. Settings has an On/Off switch; `ARBOS_NOTIFY_ALWAYS=1` ignores the focus gate (demos), `ARBOS_NOTIFY_DRY_RUN=1` logs instead of delivering.

macOS packaging: `scripts/bundle-mac.sh` builds a universal `dist/Arbos.app`, the DMG, and the updater tarball; it signs with the hardened runtime when `ARBOS_SIGN_IDENTITY` is set and notarizes + staples when the notary credentials are too (`NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID`, or `ARBOS_NOTARY_PROFILE`); every step it lacks credentials for is skipped with a line saying so. `scripts/demo-mac.sh` launches the app in Glass, runs a scripted turn and records with `screencapture -v`.

## Releases and in-app updates

Every green push to `main` becomes an installable build, with no human picking a version (`.github/workflows/release.yml`, D42–D44):

- **edge** (push to `main`): version `0.1.0.<commit count>`, release `edge-0.1.0.<n>` (pre-release) with `Arbos-…-macos-universal.dmg`, the updater tarball, static `arbos-relay`/`arbos-agentd`/`arbosctl` for Linux, and `appcast.xml`. The rolling release `edge` holds the feed the app polls: `https://github.com/unarbos/arbos/releases/download/edge/appcast.xml`.
- **stable** (tag `v0.1.0`): release `v0.1.0` marked latest; feed `https://github.com/unarbos/arbos/releases/latest/download/appcast.xml`.
- The Mac job runs on the self-hosted EC2 Mac (label `arbos-mac`, repository variable `ARBOS_MAC_RUNNER`; `scripts/runner/install-self-hosted-runner.sh`) or on `macos-15`. It signs with the Developer ID certificate, notarizes with an App Store Connect API key, and staples.
- Payloads are signed with an Ed25519 key in Sparkle's `sparkle:edSignature` encoding (`crates/arbos-update`, CLI `arbos-updatectl`). The public half is committed in `crates/arbos-update/update-key.pub` and compiled into the app; the private half lives in the 1Password vault "Arbos" and is read in CI with `OP_SERVICE_ACCOUNT_TOKEN`. The publish job verifies the appcast against the compiled-in key before anything is uploaded.
- The app polls the feed 5 s after launch and then hourly. When a newer build exists an **Update** button appears in the rail above the settings gear; one click downloads, verifies the signature, swaps `Arbos.app` in place, and relaunches. `ARBOS_UPDATE_CHANNEL=stable|edge`, `ARBOS_UPDATE_FEED=<url>`, `ARBOS_UPDATE_INTERVAL_SECS`, and `ARBOS_UPDATE_DISABLE=1` override the defaults.
- By hand, from a Mac with `op` access: `scripts/release/local-release.sh [--dry-run] [--channel stable]` does the whole release (`scripts/release/publish-release.sh` is the shared publish step).
- iOS: `.github/workflows/ios.yml` archives `apps/ios/Arbos` and uploads to TestFlight on pushes to `main` that touch `apps/ios/`.

## Connectors

The relay brokers Gmail/Calendar, Slack, GitHub and Harness Comms for the boxes (D2/D28/D40): provider tokens are stored AES-256-GCM-encrypted in the relay DB (key from `ARBOS_RELAY_KEY`, else generated once into the `meta` table) and never enter a box. The agent calls `connectors_list` / `connector_call` over MCP; agentd forwards to `POST /connectors/call`, which attaches the bearer upstream and redacts `access_token` / `refresh_token` / `Authorization` from every reply. Write and send actions go through an approval card first (one card per call; see Approvals).

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

## Skills library (F5)

A skill is a Markdown how-to with YAML frontmatter (`name`, `description`, `tags`, `when_to_use`, `version`), stored on the relay (`skills` table + FTS5) and shared across Projects. Devices: `GET/POST /skills`, `GET/PUT/DELETE /skills/{id|slug}`, `GET /skills/search?q=`. Boxes: `GET /skills/index` (lean, with a `rev`), `GET /skills/{id}`, `POST /skills` (own origin). A skill is visible to a box when it is `shared` or its `origin_project_id` is that project.

agentd caches the index (refresh every 60 s, on a `skills_rev` change seen in the 3 s project poll, on the `skills_changed` push, and after `skills_save`) and, per turn, selects up to 3 skills by keyword overlap with the prompt under a 600-token budget. They enter the pi system prompt as the `arbos-skills` section (after the byte-stable prefix, as `## Relevant skills`) and the subagent pack as section 11; the chosen slugs and text are a hidden `skills` part on the turn (Mac Details drawer, `arbosctl chat dump`) and an INFO log line `skills injected into the working set`. MCP tools: `skills_list(q?)`, `skills_read(slug)`, `skills_save(...)` (audited to `.arbos/audit.jsonl`). Headless: `arbosctl skills list [--q]|show <slug>|add --name … [--file skill.md] [--shared]|delete <slug>`. Mac: Settings → Skills.

## Channels

External chats talk to an agent through the relay (§4.5 Flow 4–5, D35): an inbound message from a *bound* chat becomes a user message in the bound Arbos chat (`sender: channel:<platform>:<user>`), runs a normal turn, and the final prose goes back over the same channel via `POST /projects/{id}/channels/reply`. A chat is bound by minting a code in Settings → Channels (or `arbosctl channels pair <project>`, 10 min TTL, one use) and sending `/pair <code>` from that chat. Senders without a binding get exactly one hint reply and never reach a box; the binding is per `(platform, chat, user)`, so other members of a group chat are ignored.

- Slack: `ARBOS_SLACK_SIGNING_SECRET` + `ARBOS_SLACK_BOT_TOKEN`; Events API URL `{public_url}/channels/slack/events` (URL verification handled, signature checked with a 5 min skew).
- Telegram: `ARBOS_TELEGRAM_BOT_TOKEN`; webhook `{public_url}/channels/telegram/webhook/{ARBOS_TELEGRAM_WEBHOOK_SECRET}` or `ARBOS_TELEGRAM_POLL=1` for long polling.
- WhatsApp Cloud API: `ARBOS_WHATSAPP_TOKEN`, `ARBOS_WHATSAPP_PHONE_ID`, `ARBOS_WHATSAPP_VERIFY_TOKEN` (optional `ARBOS_WHATSAPP_APP_SECRET` for `X-Hub-Signature-256`); webhook `GET/POST {public_url}/channels/whatsapp/webhook`.
- Beeper: reuses the Harness Comms connector token — the relay polls `POST /v1/messages/poll` (cursor kept in `meta`, `ARBOS_BEEPER_POLL_S`, default 30 s) and sends through `POST /v1/requests` with idempotent request ids. Only messages the user sent themself (`sender.is_self`) can pair or receive the hint, so the relay never writes to someone else's contact.
- Endpoints: `GET /channels` (platform state + bindings + active code), `POST /channels/pair/new`, `GET/DELETE /channels/bindings[/{id}]`, `GET /channels/mock/outbox`. `ARBOS_CHANNELS_MOCK=1` turns every platform into a mock: `POST /channels/mock/inbound` injects a message and replies land in `channel_outbox` instead of leaving the machine (`arbosctl channels inbound|outbox|list|unbind`).

## Approvals (draft-and-approve)

Risky actions wait for the user. The agent drafts outbound messages with the `draft_message` tool (email, Slack, Telegram, WhatsApp, SMS); sending connectors (`connector_call` send/write), vault requests, local computer use and destructive shell commands (`rm -rf` outside `tmp/`, `git push --force`, `DROP TABLE`, cloud deletes, …; the list is `arbos_core::approval::RISKY_RULES`) are gated the same way. Each one becomes an `approval` card in the chat (`state: pending|approved|rejected|expired`, 24 h expiry); nothing runs until the client writes an `approve` or `reject` command. Both decisions land in `/work/.arbos/audit.jsonl` on the box and are mirrored to the relay (`GET /projects/{id}/audit`). An approved draft is delivered through the connector that carries its channel (email → `google.send`, else `harness_comms.send`; Slack → `slack.post_message`; Telegram/WhatsApp → `harness_comms.send`) without a second card; with no such connector it is recorded in the box's `outbox` table and the agent says so.

```bash
cargo run -p arbosctl -- chat send demo <chat> "Draft an email to sam@example.com confirming Tuesday" --stream   # prints the card and its id
cargo run -p arbosctl -- chat approve demo <chat> <approvalId>      # or: chat reject … --reason "too long"
cargo run -p arbosctl -- audit demo                                  # approval_requested / approval_decided entries
```

`ARBOS_APPROVAL_TIMEOUT_SECS` (box, default 86400) caps how long an inline-gated tool call waits.

## Your computer: `arbos-local`

`arbos-local` is a small helper that runs on the user's own machine so a project agent can run a command or read a file there. It pairs like any device (D35), keeps one WebSocket to the relay (`WS /local/{deviceId}`), announces hostname/os/home and performs `run` (shell, timeout ≤ 600 s, 256 KiB per stream), `read_file` (≤ 1 MiB inline; bigger files land in the relay blob store and reach the agent as a handle) and `list_dir` (≤ 2000 entries). The relay only forwards: a box calls `POST /projects/{id}/local/{device}/exec` (box token) and the relay waits on the helper's socket for the result. The agent's MCP tools are `local_devices`, `local_run`, `local_read`, `local_list`; every call shows the user an approval card (risk `computer`) with the exact command or path and is refused without an approved outcome (unit-tested). Settings → This Mac lists paired helpers with online state, the install command, and a per-device "Allow reads without asking" toggle (default off; when on, `read_file`/`list_dir` skip the card but are still audited; commands always ask). The card is a real approval (see Approvals above): `arbosctl chat approve/reject` or the Mac app decide it, and a late approval re-runs the identical call once.

```bash
arbos-relay pair-code                                   # on the relay host: a one-time code
arbos-local pair <code> --relay http://127.0.0.1:7700 --name "Jacob's MacBook"
arbos-local run                                         # reconnects with backoff; Ctrl-C to stop
```

Device endpoints: `GET /local/devices`, `PATCH /local/devices/{id} {"allow_reads": bool}`, `DELETE /local/devices/{id}` (forgets the helper and revokes its token). Config lives in `~/.config/arbos/arbos-local.json` (`ARBOS_LOCAL_CONFIG` overrides); `ARBOS_LOCAL_HOSTNAME` overrides the advertised hostname.

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

`.github/workflows/ci.yml` runs on pushes to `main`, `arbos-v2`, and `cursor/**` and on pull requests: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, the harness typecheck, shellcheck + actionlint, and a layout check that the previous repo's `release-main.yml` / `fx-*` workflows are gone. `release.yml` runs after a green CI on `main`.
