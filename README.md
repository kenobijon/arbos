# Arbos

Arbos is a messenger for working with AI agents. You talk to a project agent in a chat — on the Mac, on the iPhone, or from a terminal — and it does the work on your own cloud computer: it answers, acts in a few tool calls, or hands bigger jobs to subagents that run in the background and report back in prose. Every user has **one** Daytona computer; every project is a workspace on it with its own folder, agent sessions, memory and history. Provider keys never leave the relay.

## What is in this repository

| Path | What |
| --- | --- |
| `crates/arbos-mac` | The Mac app (GPUI): project rail, tabs, chat transcript, composer with voice and attachments, approval cards, Settings (Machine, Connectors, Vault, Routines, Skills, Channels, This Mac), machine status, in-app updates. Also builds on Linux. |
| `apps/ios/Arbos` | The iPhone app (SwiftUI): the same chats over the relay (Loro replica per room), home with machine status, attach sheet, voice memos, TestFlight via fastlane. |
| `crates/arbos-relay` | The one service outside the boxes: device pairing, project registry and sync rooms (SQLite), per-user machine lifecycle on Daytona (wake, keep-alive, stop, stale-box detection, migration), model proxy with per-project metering and spend caps, blob store, connectors/vault broker, routines, channels, group chats, `arbos-local` forwarding. |
| `crates/arbos-agentd` | The box daemon: one process per machine supervising one workspace per project — Loro doc host, SQLite+FTS5 history and memory, project MCP server, the pi main-agent driver, the subagent harness registry, per-workspace Unix user + cgroup isolation. |
| `crates/arbosctl` | Headless client for the relay: pair, projects, chats (send/stream/stop/answer), machine start/stop/status, migration, usage, timings, groups, blobs. |
| `crates/arbos-local` | Helper that runs on your own Mac so an agent can run a command or read a file there, behind approval cards. |
| `crates/arbos-update` | Appcast + Ed25519 payload signatures + version order shared by the release pipeline and the app's Update button. |
| `crates/arbos-core` | Shared types: ids, binary room frames, the Loro chat-doc schema, the `route` schema, the pi/Prime RPC codec. |
| `harness/arbos-pi-ext` | The pi extension (TypeScript): `route`, `ask_user`, deny+audit hook, act-call limits, output offload to handles, working set. |
| `image/` | The `arbos-agent` sandbox image (pi + Prime + Claude Code + agent-browser/Chrome), hook scripts, Prime skills, pi seed; `register-snapshot.sh`. |
| `scripts/` | `bundle-mac.sh` (DMG, signing, notarization), `release/` (publish, local release), `runner/` (self-hosted Mac runner), `relay-start.sh` (relay + Cloudflare tunnel), `landing/`. |
| `docs/` | Design notes and decisions. |

## How it works

- **Relay** (`arbos-relay`): the only internet-facing piece. Devices pair once with a one-time code (D35) and talk to it over HTTPS/WebSocket; boxes dial back to its public URL. It owns the chat rooms (opaque Loro row log + checkpoints), wakes machines on demand, forwards model calls with metering, and keeps every secret: provider keys, connector tokens, the vault.
- **One machine per user** (D70): a Daytona sandbox from the `arbos-agent` snapshot, **4 vCPU / 8 GiB / 10 GiB** (the Daytona per-sandbox maximum; about $0.33 per active hour, $0.013 per stopped day, $0 archived). It starts on the first message and auto-stops when idle. Each project is a workspace on it: `/work/<project>` owned by its own Unix user at mode 700, its own pi session, subagents, SQLite memory/history, blob dir, browser profile and cgroup slice. `GET /machine` shows state, spec, spend and workspace states; `POST /machine/start|stop` drives it. Older per-project sandboxes are copied in with `arbosctl migrate-to-user-box`.
- **Harness**: the main agent is **pi** (one RPC session per chat) with the `arbos-pi-ext` extension; every turn starts with a `route` call (`reply`, `act` with ≤ 8 tool calls, `new`/`wake` a subagent). Subagents run through the harness registry — **Prime Agent** by default (Python kernel), **pi**, or **Claude Code** — in the workspace, up to 4 per workspace and 8 per machine, and report back as a prose turn written by the main agent. Users only ever see prose; tool detail lives in the Details drawer and the history index.
- **Clients**: the Mac app and the iPhone app are peers of the same Loro docs; `arbosctl` does everything from a terminal. Approvals, questions with widgets, attachments, voice, group chats, routines, connectors, skills and the vault are described below.

## Install

- **Mac**: download the latest DMG from the [edge release](https://github.com/unarbos/arbos/releases/tag/edge) or a stable `v*` release; the app updates itself (Update button in the rail).
- **iPhone**: TestFlight (`com.unarbos.arbos.ios`), uploaded from every push to `main`.
- **Relay**: build from source (below) or take the static `arbos-relay` from a release; run it with a stable public URL (`scripts/relay-start.sh`).

## Prerequisites (building from source)

- Rust stable (`rust-toolchain.toml` pins the channel; `rustup` installs it).
- Node 22.19+ (pi needs zstd in `zlib`; 22.14 is too old) and pnpm 9 for `harness/`.
- `pi` 1.2.0 (`npm i -g @earendil-works/pi-coding-agent@1.2.0`) for a local box; `prime-agent` 0.10.0 plus its kernel venv (see `image/Dockerfile`) for Prime subagents. Without Prime, set `ARBOS_DEFAULT_HARNESS=pi`.
- Secrets, only on the relay host: `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`) for the model proxy and `DAYTONA_API_KEY` for real machines. A box never sees them (D2).
- For `arbos-mac` on Linux: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb-xkb-dev libasound2-dev libssl-dev libgl1-mesa-dev libfontconfig-dev` (and `mesa-vulkan-drivers` to run without a GPU). The iPhone app needs Xcode 16+ and fastlane.

## Build and check

```bash
cargo build --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd harness && pnpm install && pnpm -r typecheck
```

## Run locally (no Daytona, no Docker): the `process` box

The relay can run `arbos-agentd` as a child process on the same host (one process = the user's machine, one workspace per project under `.arbos-local/relay/boxes/u_default/work/<project>`). Every tool, the pi main agent, and the subagents run for real; only the sandbox boundary and the per-workspace users are missing.

```bash
# 1. relay (prints a one-time pairing code on first start)
export OPENROUTER_API_KEY=…
export ARBOS_AGENTD_BIN=target/debug/arbos-agentd ARBOS_PI_BIN=$(command -v pi) \
       ARBOS_PI_EXT=$PWD/harness/arbos-pi-ext/index.ts
cargo run -p arbos-relay -- --listen 127.0.0.1:7700 --data ./.arbos-local/relay

# 2. pair this machine
cargo run -p arbosctl -- pair <code>

# 3. a project (the first one creates the user's machine; later ones are workspaces on it)
cargo run -p arbosctl -- project create --name demo --local-box
cargo run -p arbosctl -- machine status

# 4. chat
cargo run -p arbosctl -- chat open demo                        # prints a chat id
cargo run -p arbosctl -- chat send demo <chat> "hello" --stream
cargo run -p arbosctl -- chat stream demo <chat> --until-done  # resumes by cursor after a kill
cargo run -p arbosctl -- chat stop demo <chat>                 # aborts the running turn at once
cargo run -p arbosctl -- chat answer demo <chat> --answer-json '"nightly"' --stream   # answer the open ask_user question
cargo run -p arbosctl -- machine stop && cargo run -p arbosctl -- machine start
cargo run -p arbosctl -- timings demo                          # boot, wake, TTFT, subagent timings
```

`arbosctl` keeps its device token and per-chat replicas under `~/.config/arbos/` (`ARBOSCTL_CONFIG` overrides). All commands: `cargo run -p arbosctl -- --help`.

Questions and widgets: `ask_user(question, widget?)` is the one way an agent asks the user something. Without a widget the question is plain and the composer answers it. With a widget (`choice`, `multi_select`, `text`, `date`, `form` with up to 8 fields) the chat gets a `question` part the clients render as a card; the answer goes back as an `answer` command `{questionId, value}` and agentd renders it to text for the model (`Answer: nightly`, or one bullet per form field). `arbosctl chat answer … --answer-json <json>` answers the newest open question from a terminal (`--question <id>` picks one). All three harness seams (agentd MCP/hook, the pi extension, the Prime `arbos_ask_user` skill) take the same widget shape.

What a chat shows: the user only ever sees prose from the main agent, written like a colleague in chat (what got done and the outcome, never how the tools were called; agentd also strips leading "Called N tools"/"Running …" lines and ```json blocks before text enters the doc). A hand-off (`new`/`wake`) is one plain sentence from the model, then a progress message with status events only (`working`, then `done`/`failed`), then a prose result turn (`turn.kind = "subagent_result"`) written by the main agent from the subagent's output. Status codes are `working`, `done`, `failed`, `stopped`, `waiting_user`; tool calls and raw subagent output are parts with `hidden: true` (Details drawer, not the transcript) and in T3. Up to 4 subagents run per workspace and 8 per machine at once; more wait in FIFO order, and the chat stays responsive meanwhile.

Relay environment (all optional unless noted):

| Variable | Meaning |
| --- | --- |
| `ARBOS_RELAY_LISTEN` | bind address, default `127.0.0.1:7700`; use `0.0.0.0:7700` on a server |
| `ARBOS_RELAY_PUBLIC_URL` | the URL boxes and clients dial back (**required for Daytona boxes**), e.g. `https://relay.example.com` or a cloudflared/ngrok URL in front of the plain-HTTP relay; WebSockets pass through such tunnels |
| `ARBOS_RELAY_DATA` | SQLite + blobs + local boxes, default `./.arbos-local/relay` |
| `ARBOS_RELAY_ADMIN_TOKEN` | token for `POST /pair/new`; generated and logged when unset |
| `OPENROUTER_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | provider keys for the model proxy (never reach a box) |
| `ARBOS_OPENAI_BASE`, `ARBOS_ANTHROPIC_BASE` | upstreams, default OpenRouter |
| `ARBOS_OPENAI_IMAGES_BASE`, `ARBOS_IMAGE_MOCK` | image generation upstream (default `https://api.openai.com/v1`, needs `OPENAI_API_KEY`); `ARBOS_IMAGE_MOCK=1` returns a labelled placeholder PNG instead |
| `DAYTONA_API_KEY`, `DAYTONA_API_URL`, `DAYTONA_ORG_ID` | Daytona access; without a key new projects get `process` boxes |
| `ARBOS_DAYTONA_SNAPSHOT` | snapshot for new boxes, default `arbos-agent-0.1.11` |
| `ARBOS_DAYTONA_DOCKERFILE`, `ARBOS_BOOTSTRAP_DIR` | dev-image path (see below) |
| `ARBOS_AUTH_MODE` | `github` (self-serve: Sign in with GitHub, JWT bearers, credits) or `paircode` (self-hosters); default `github` when `ARBOS_GITHUB_CLIENT_ID` is set |
| `ARBOS_GITHUB_CLIENT_ID`, `ARBOS_GITHUB_CLIENT_SECRET`, `ARBOS_PUBLIC_URL`, `ARBOS_JWT_KEY` | GitHub OAuth app, the public origin of this relay (callback `…/auth/github/callback`), optional JWT signing key (derived from the relay key when unset) |
| `ARBOS_DEFAULT_GRANT_USD` (100), `ARBOS_DEFAULT_DAILY_CAP_USD` (20), `ARBOS_GLOBAL_DAILY_CAP_USD` (500) | inference credits: welcome grant, per-user daily cap, relay-wide daily cap that trips the kill switch (`PUT /admin/killswitch`) |
| `ARBOS_ALWAYS_ON` | machines always on (default on in `github` mode): no Daytona auto-stop, a box per signed-in user from sign-in, re-woken after a relay restart; manual Stop and the kill switch are the only things that keep one down |
| `ARBOS_MACHINE_USD_PER_HOUR` (0.332), `ARBOS_MACHINE_STOPPED_USD_PER_DAY` (0.013), `ARBOS_GLOBAL_MACHINE_DAILY_CAP_USD` (400) | machine time is metered into its own ledger bucket (`machine_today_usd` in `/me`), outside the grant and caps; the global machine cap only logs a warning |
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

## One machine per user (D70)

Each user has one Daytona computer; every project of that user is a workspace on it: its own folder `/work/<project-id>`, its own pi main session and subagents, its own SQLite memory/history and blob dir, its own loopback MCP/hook/model-stub ports (7800 + 10·n). One `arbos-agentd` per box supervises them (`crates/arbos-agentd/src/workspace.rs`): workspaces start from `POST /box/hello` and the 3 s `GET /box/projects` poll, stop when a project is deleted, and up to 8 subagents run per box (4 per workspace). The box starts on demand and auto-stops when idle, as before.

- Class: the snapshot fixes vCPU/RAM/disk. Default `ARBOS_DAYTONA_SNAPSHOT` = `arbos-agent-0.1.11` registered at **4 vCPU / 8 GiB / 10 GiB** (the Daytona per-sandbox maximum on this org; $0.332 per active hour, $0.013 per stopped day, $0 archived). Per-user override: `PUT /admin/users/{id}/box {"snapshot": "…"}` (admin token) before the box is created.
- Machine API (devices): `GET /machine` → `{user_id, state, since, provider, sandbox_id, spec {cpu, mem_gib, disk_gib, class}, spend_today_usd, spend_cap_usd, workspaces: [{project_id, state}], migrated_from}`; `POST /machine/start`, `POST /machine/stop?force=`. `arbosctl machine status|start|stop`.
- Project docs: `status.box = {state, since}` (the user's machine, mirrored into every project doc) and `status.workspace = {state, since}` with `state ∈ running | stopped`.
- Box API (box token): `POST /box/hello`, `GET /box/projects`, `POST /box/keepalive`, `POST /projects/{id}/workspace {state}`. The box names the workspace in `X-Arbos-Project` on model calls and skills calls, so usage and skills stay per project.
- Isolation inside the box: agentd runs as root (it re-execs through the image's passwordless sudo; `ARBOS_ISOLATION=off` keeps the old single-user mode, local `process` boxes never isolate); every workspace gets its own Unix user `ws-<project>` (no sudo), `/work/<project>` at mode 700 under a non-listable `/work` (711), its own HOME/caches, and a cgroup v2 slice (`cpuset.cpus` = the box's CPU quota so `nproc` tells the truth, `memory.max` = half the box, `memory.high` 3/8, `pids.max` 1024, `cpu.weight` 100). Toolchains stay shared read-only under `/usr/local` and `/opt`. At boot the box checks that workspace A cannot read workspace B and reports it in `/healthz` (`isolation.cross_read_blocked`); the same check is the ignored root test `isolation::tests::workspace_a_cannot_read_workspace_b`. Not enforceable in a Sysbox container: a PID namespace (`ps` shows other workspaces' processes) and anything above the box's own quota.
- Migration from the old one-sandbox-per-project layout: `arbosctl migrate-to-user-box [--project <id>] [--force]` (`POST /machine/migrate`). For each legacy sandbox it stops the old agentd, packs `/work` (repo + `.arbos` state), copies it into the user box as `/work/<project>` while that workspace is held, releases it, stops the old sandbox and marks it `migrated_to` (never deletes). Idempotent.

## Run on Daytona

With `DAYTONA_API_KEY` set, `project create` (without `--local-box`) creates a real sandbox from the snapshot named by `ARBOS_DAYTONA_SNAPSHOT` (default `arbos-agent-0.1.11`, the current registered build). The relay must be reachable from the box (`ARBOS_RELAY_PUBLIC_URL`). If no production snapshot is registered yet, point the relay at the dev image instead:

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
cargo run -p arbos-mac                                  # https://relay.arbos.life (compiled-in default)
ARBOS_RELAY_URL=fixture cargo run -p arbos-mac          # fixture source: seeded chats + scripted agent
ARBOS_RELAY_URL=ws://127.0.0.1:7700 cargo run -p arbos-mac   # a local relay
```

Self-hosters on macOS can also point the bundle at their relay with `defaults write sh.arbos.mac RelayURL https://relay.example`. First launch shows "Sign in with GitHub": the relay runs the OAuth leg (`/auth/github/start`), macOS opens it in an `ASWebAuthenticationSession` that returns on `arbos://auth/callback`, Linux opens the system browser (`ARBOS_BROWSER` overrides) and listens on a loopback port, and the one-time grant is exchanged at `POST /auth/token`. The JWT and its rotating refresh token go to the login keychain on macOS (`security`), or `~/.config/arbos/` (0600) elsewhere; the token refreshes five minutes before expiry or on a 401. A relay in `ARBOS_AUTH_MODE=paircode` answers 409 on start, and the screen falls back to the one-time pairing code from the relay log. `ARBOS_RELAY_TOKEN` overrides both. After sign-in the app lands in a chat: an account with no projects gets "Home" created for it, and the machine provisions on the first message. Matte is the default theme; Glass = macOS vibrancy + in-window frosted panels (the frost also renders on Linux). `ARBOS_SURFACE=glass` starts in Glass.

Widget cards answer with the mouse or the keyboard (arrows move, space toggles, Enter submits); the fixture's "Beta release" chat under Arbos shows every widget type. System notifications (macOS `osascript`, Linux `notify-send`) fire for turn finished, needs input, approval requested, wake failed, spend cap reached, cross-project message and routine ran — at most one per project per minute (later ones fold into "N updates in <Project>"), never while that project is streaming, and only while the window is unfocused or the chat is not on screen. Settings has an On/Off switch; `ARBOS_NOTIFY_ALWAYS=1` ignores the focus gate (demos), `ARBOS_NOTIFY_DRY_RUN=1` logs instead of delivering.

macOS packaging: `scripts/bundle-mac.sh` builds a universal `dist/Arbos.app`, the DMG, and the updater tarball; it signs with the hardened runtime when `ARBOS_SIGN_IDENTITY` is set and notarizes + staples when the notary credentials are too (`NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID`, or `ARBOS_NOTARY_PROFILE`); every step it lacks credentials for is skipped with a line saying so. `scripts/demo-mac.sh` launches the app in Glass, runs a scripted turn and records with `screencapture -v`.

## Releases and in-app updates

Every green push to `main` becomes an installable build; nobody picks a version by hand (`.github/workflows/release.yml`, D42–D44). The workspace version in `Cargo.toml` is **0.3.0**; it sorts above every build the previous Arbos shipped (Mac releases up to `v0.2.112` and the rolling `edge`), so an installed older app sees the new builds as updates.

- **edge** (push to `main`): version `0.3.0.<commit count>`, release `edge-0.3.0.<n>` (pre-release) with `Arbos-…-macos-universal.dmg`, the updater tarball, static `arbos-relay`/`arbos-agentd`/`arbosctl` for Linux, and `appcast.xml`. The rolling release `edge` holds the feed the app polls: `https://github.com/unarbos/arbos/releases/download/edge/appcast.xml`.
- **stable** (tag `v0.3.0`): release `v0.3.0` marked latest; feed `https://github.com/unarbos/arbos/releases/latest/download/appcast.xml`. The tag must match the `Cargo.toml` version.
- The Mac job runs on the self-hosted EC2 Mac (label `arbos-mac`, repository variable `ARBOS_MAC_RUNNER`; `scripts/runner/install-self-hosted-runner.sh`) or on `macos-15`. It signs with the Developer ID certificate, notarizes with an App Store Connect API key, and staples.
- Payloads are signed with an Ed25519 key in Sparkle's `sparkle:edSignature` encoding (`crates/arbos-update`, CLI `arbos-updatectl`). The public half is committed in `crates/arbos-update/update-key.pub` and compiled into the app; the private half lives in the 1Password vault "Arbos" and is read in CI with `OP_SERVICE_ACCOUNT_TOKEN`. The publish job verifies the appcast against the compiled-in key before anything is uploaded.
- The app polls the feed 5 s after launch and then hourly. When a newer build exists (numeric comparison per dotted component, `crates/arbos-update/src/version.rs`) an **Update** button appears in the rail above the settings gear; one click downloads, verifies the signature, swaps `Arbos.app` in place, and relaunches. `ARBOS_UPDATE_CHANNEL=stable|edge`, `ARBOS_UPDATE_FEED=<url>`, `ARBOS_UPDATE_INTERVAL_SECS`, and `ARBOS_UPDATE_DISABLE=1` override the defaults.
- By hand, from a Mac with `op` access: `scripts/release/local-release.sh [--dry-run] [--channel stable]` does the whole release (`scripts/release/publish-release.sh` is the shared publish step).
- **iPhone**: `.github/workflows/ios.yml` archives `apps/ios/Arbos` on every push and uploads to TestFlight on pushes to `main` (fastlane `beta` lane, App Store Connect API key). `MARKETING_VERSION` is 1.0.0; the build number is the UTC time `YYYYMMDDHHMM` (passed as `CURRENT_PROJECT_VERSION`), so it always rises above every earlier TestFlight build.

## Voice

Speech goes through the relay's model proxy so the OpenAI key never reaches a device: `POST /proxy/v1/audio/transcriptions` (multipart, Whisper-compatible; `gpt-4o-mini-transcribe`) and `POST /proxy/v1/audio/speech` (`gpt-4o-mini-tts`, WAV) forward to `ARBOS_OPENAI_AUDIO_BASE` (default `https://api.openai.com/v1`) with `OPENAI_API_KEY`, and meter per project under `api = openai-audio` (STT by the minute, TTS by the character). `ARBOS_VOICE_MOCK=1` answers without a key: a `[mock transcript] N s of audio …` text and a 1 s sine WAV, flagged with `x-arbos-voice-mock: 1`.

In the desktop client the composer has a mic button (hold to talk, or click to toggle; 16 kHz mono WAV kept in memory). The transcript lands in the composer (Enter sends); with `♪ Voice` on it is sent directly and the reply is read aloud when it finishes (`▶ Replay` under the message). The sent message carries the clip as an `attachment` part `{ "kind": "audio", "mime": "audio/wav", "ref": "voice-<ulid>.wav" }` uploaded to the project's blob store. `☎` in the tab bar opens a call strip (timer, level meter, mute, hang up) that loops listening → thinking → speaking with an energy VAD. Without an input device the app records the bundled `crates/arbos-mac/assets/test-voice.wav` and says so; without an output device replies are decoded but not played. Audio is not streamed to the box: a turn is one uploaded clip and one spoken reply.

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

## Files and images

The agent produces documents and media in the box (the project folder's `out/`, with python-pptx, python-docx, openpyxl, reportlab, Pillow and ffmpeg from the image's venv) and hands them over with the `deliver_file(path, title?, kind?)` MCP tool: agentd uploads the file to the relay blob store and appends an `attachment` part `{ name, mime, ref, bytes, kind, title }` to the reply (D38; the bytes never enter the chat doc). `generate_image(prompt, size?, quality?)` goes through the relay's `POST /proxy/v1/images/generations` (OpenAI `gpt-image-1`, metered per project like the model proxy, `ARBOS_IMAGE_MODEL` overrides the model) and delivers the PNG the same way. The desktop client shows each attachment as a card (icon by kind, title, size, inline preview for images) with Download → `~/Downloads/<name>`, then reveals it in Finder (`xdg-open` on Linux).

```bash
scripts/box-media-deps.sh ./.arbos-local          # process box: venv under the data dir, prints the ARBOS_MEDIA_VENV export
arbosctl blob get <project> <ref> --out deck.pptx  # fetch a delivered file by its attachment ref
```

`ARBOS_MEDIA_VENV` points agentd at the python with the document libraries (the image sets `/opt/prime-venv`; without it the system `python3` is used). The Prime skill `arbos_deliver_file` wraps both tools for subagents.

## Group chats

A group is a chat with `kind: group` and a list of `participants` (project ids, host first), hosted by one project's box. Every user message there goes out from the host box through `POST /projects/{host}/groups/{chat}/send` and reaches the other participants as a `group_message` push; their boxes run it as a turn in their own "Projects ⇄" tab (`kind: projects`) and post the prose reply back (`…/reply`), which the host appends as a `role: project` message with `sender: project:<id>`, `senderName`, `senderIcon`. Agents address each other only with the main-session MCP tool `group_send(text, to?)` (subagents are refused — the MCP server tells sessions apart by role-bound bearer tokens). The relay enforces D37: a trace id per thread, depth ≤ 2, no cycles, 20 group messages per project per hour (`429`). Endpoints: `POST /projects/{id}/groups {title?, participants}`, `GET|PATCH /projects/{id}/groups/{chat}` (`{add, remove, title}`).

```bash
arbosctl group create --host Alpha --with Beta,Gamma --title "Launch sync"   # prints the chat id
arbosctl group send Alpha <chat> "Beta, what do you need from Alpha?" --stream --wait 120
arbosctl group show|add|remove Alpha <chat> [projects]
```

In the desktop client a group tab shows the participants' glyphs stacked in its title, each Project's message carries a sender chip, and "+ project" in the tab header adds a Project to the open group or starts a new group with it.

## Vault

The vault holds logins, API keys and secrets the agent may *use* but never *see* (§4.7, D28). Values are AES-256-GCM-encrypted with the same `ARBOS_RELAY_KEY` as connector tokens and are write-only over the device API; the agent only ever handles the placeholder `arbos-secret://<item>/<field>`. Every use is a request the owner approves (Settings → Vault, or `POST /vault/requests/{id}/decide`): `header`/`body` uses are honoured by `POST /egress`, which substitutes placeholders only in requests to one of the item's `allowed_hosts` (exact or `*.example.com`, never in URLs) and replaces the value with `[REDACTED:vault]` on the way back; `env` uses surface as `ARBOS_SECRET_<NAME>` in the child-agent environment (mirrored to Daytona Secrets when a Daytona key is configured); `browser_fill` lets agentd drive agent-browser itself, holding the values for at most 30 s and redacting them from audit and history (D19).

- Device endpoints: `GET/POST /vault/items`, `PUT/DELETE /vault/items/{id}`, `GET /vault/requests`, `POST /vault/requests/{id}/decide` (`{"approve": true|false}`). Box endpoints: `POST /vault/request`, `GET /vault/requests/{id}`, `POST /vault/requests/{id}/values` (approved `browser_fill` only, 10 min window), `POST /egress`.
- MCP tools: `vault_list`, `vault_request`, `egress`, `browser_fill`. `vault_request` posts one approval card (risk `credential`) whose payload carries the relay request id; approving it in the chat forwards the decision to the relay row, which stays the authority, and a decision made in Settings → Vault settles the card.  `ARBOS_VAULT_WAIT_S` (default 180) bounds how long a tool call waits for a decision.
- `ARBOS_VAULT_MOCK=1`: seeds `demo-login` and `demo-api-key` with `allowed_hosts=[127.0.0.1, localhost]` and serves `ANY /vault/mock/echo` as a local egress target that reflects headers and body; `browser_fill` simulates the form submit when agent-browser is not installed. The Mac fixture source shows the same panel under Settings → Vault.

## Image

`.github/workflows/image.yml` builds `image/Dockerfile` on an image tag (`image-v0.1.11`) or by hand (`workflow_dispatch` with a version), pushes it to GHCR as `ghcr.io/unarbos/arbos/arbos-agent:<version>` (public, so Daytona pulls it without credentials), smoke-tests it, and registers the Daytona snapshot `arbos-agent-<version>` with `image/register-snapshot.sh` (needs the `DAYTONA_API_KEY` repository secret; until it is set, run the script by hand with the GHCR image, as for 0.1.10 and 0.1.11). The relay needs an image rebuild whenever `arbos-agentd` changes: the binary is baked into the image, boxes do not update it at boot.

Without CI the same build runs on any Docker host, including a Daytona Docker-in-Docker sandbox (`FROM docker:27-dind`, start `dockerd`, upload the build context over SSH), then goes to Daytona's transient registry:

```bash
# static agentd for the image
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release -p arbos-agentd --target x86_64-unknown-linux-musl
mkdir -p dist && cp target/x86_64-unknown-linux-musl/release/arbos-agentd dist/
docker build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:<v> .
# temporary push credentials: GET $DAYTONA_API_URL/docker-registry/registry-push-access?snapshotName=arbos-agent-<v>
docker tag arbos-agent:<v> cr.app.daytona.io/<project>/arbos-agent:<v> && docker push cr.app.daytona.io/<project>/arbos-agent:<v>
image/register-snapshot.sh arbos-agent-<v> cr.app.daytona.io/<project>/arbos-agent:<v> 4 8 10
```

Transient-registry tags are immutable: bump the version for every rebuild. The Arbos layers are last in the Dockerfile, so a code change rebuilds in seconds against the cached toolchain layers.

## CI

`.github/workflows/ci.yml` runs on pushes to `main`, `arbos-v2`, and `cursor/**` and on pull requests: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux, the harness typecheck, shellcheck + actionlint, and a layout check that the previous repo's `release-main.yml` / `fx-*` workflows are gone. `release.yml` runs after a green CI on `main`.

## History

This tree replaced the previous Arbos on 2026-10-15 (`main` was force-pushed to the `arbos-v2` integration branch). The earlier `main` — the zeron-based desktop app, the fx harness and the phase-6 phone — is kept as branch `backup/main-2026-10-15`; the generation before that (the first mobile app, voice server, deploy scripts) is branch and tag `legacy-2026-10`. Their releases (`v0.1.x`, `v0.2.x`, the old `edge`) stay on GitHub; the appcast feeds now point at the 0.3.0 builds.
