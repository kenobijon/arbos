# Self-hosting Arbos

Arbos has one server-side piece: the **relay** (`arbos-relay`). Everything else — the Mac app, the iPhone app, `arbosctl`, and the agent machines — talks to it. This guide takes you from a checkout to a relay your own devices can use.

Words used here:

- **Relay**: the one internet-facing service. It holds the chat rooms, provider keys, connector tokens and the vault, meters model calls, and wakes machines.
- **Machine** (also "box"): the user's cloud computer, one per user. By default a [Daytona](https://www.daytona.io) sandbox built from the `arbos-agent` image. Each project is a **workspace** on it.
- **Process box**: the same machine, run as a child process of the relay on the relay host. No Daytona, no Docker. Good for a first run.
- **Public URL**: the `https://` address machines and devices dial back to. It must be stable for persistent projects.

## 1. Prerequisites

- Rust stable (`rust-toolchain.toml` pins the channel; `rustup` installs it).
- Node 22.19+ and pnpm 9 for `harness/` (pi needs zstd in `zlib`; 22.14 is too old).
- `pi` 1.2.0 (`npm i -g @earendil-works/pi-coding-agent@1.2.0`) for a process box; `prime-agent` 0.10.0 plus its kernel venv (see `image/Dockerfile`) for Prime subagents. Without Prime, set `ARBOS_DEFAULT_HARNESS=pi`.
- A model key: `OPENROUTER_API_KEY` (or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`). It lives on the relay host only; a machine never sees it.
- For real machines: `DAYTONA_API_KEY`.
- For the Mac app on Linux: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb-xkb-dev libasound2-dev libssl-dev libgl1-mesa-dev libfontconfig-dev` (and `mesa-vulkan-drivers` without a GPU). The iPhone app needs Xcode 16+ and fastlane.

Build everything:

```bash
cargo build --workspace
cd harness && pnpm install && pnpm -r typecheck
```

Prebuilt static `arbos-relay` / `arbos-agentd` / `arbosctl` for Linux are attached to every [release](https://github.com/unarbos/arbos/releases).

## 2. First run: relay + process box on one host

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
```

Every tool, the pi main agent and the subagents run for real; only the sandbox boundary and the per-workspace Unix users are missing. Workspaces live under `.arbos-local/relay/boxes/u_default/work/<project>`. `arbosctl` keeps its device token under `~/.config/arbos/` (`ARBOSCTL_CONFIG` overrides); `cargo run -p arbosctl -- --help` lists every command.

## 3. Sign-in modes

`ARBOS_AUTH_MODE` picks how devices sign in:

- `paircode` — the relay prints a one-time code; `arbosctl pair <code>` or the apps' pairing screen exchange it for a device token. Default when no GitHub OAuth app is configured. Simplest for one person.
- `github` — "Sign in with GitHub" (OAuth, PKCE), JWT access tokens with rotating refresh tokens, credits and spend caps. Needs your own OAuth app: callback `https://<your relay>/auth/github/callback`, then `ARBOS_GITHUB_CLIENT_ID`, `ARBOS_GITHUB_CLIENT_SECRET`, `ARBOS_PUBLIC_URL` (the relay's public origin) and optionally `ARBOS_JWT_KEY`. This is what arbos.life runs.

## 4. Relay environment

All optional unless noted.

| Variable | Meaning |
| --- | --- |
| `ARBOS_RELAY_LISTEN` | bind address, default `127.0.0.1:7700`; use `0.0.0.0:7700` on a server |
| `ARBOS_RELAY_PUBLIC_URL` | the URL machines and devices dial back (**required for Daytona machines**), e.g. `https://relay.example.com`; WebSockets must pass through |
| `ARBOS_RELAY_DATA` | SQLite + blobs + local boxes, default `./.arbos-local/relay` |
| `ARBOS_RELAY_ADMIN_TOKEN` | token for the admin endpoints (`/admin/*`, `POST /pair/new`); generated and logged when unset |
| `ARBOS_RELAY_KEY` | AES-256-GCM key for connector tokens and vault values; generated once into the `meta` table when unset |
| `OPENROUTER_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | provider keys for the model proxy (never reach a machine) |
| `ARBOS_OPENAI_BASE`, `ARBOS_ANTHROPIC_BASE` | upstreams, default OpenRouter |
| `ARBOS_OPENAI_AUDIO_BASE`, `ARBOS_VOICE_MOCK` | speech-to-text / text-to-speech upstream (default `https://api.openai.com/v1`, needs `OPENAI_API_KEY`); `ARBOS_VOICE_MOCK=1` answers without a key |
| `ARBOS_OPENAI_IMAGES_BASE`, `ARBOS_IMAGE_MODEL`, `ARBOS_IMAGE_MOCK` | image generation upstream and model; `ARBOS_IMAGE_MOCK=1` returns a labelled placeholder PNG |
| `DAYTONA_API_KEY`, `DAYTONA_API_URL`, `DAYTONA_ORG_ID` | Daytona access; without a key new projects get process boxes |
| `ARBOS_DAYTONA_SNAPSHOT` | snapshot for new machines (the current registered `arbos-agent-<version>`) |
| `ARBOS_DAYTONA_DOCKERFILE`, `ARBOS_BOOTSTRAP_DIR` | dev-image path (§6) |
| `ARBOS_AUTH_MODE` | `github` or `paircode` (§3) |
| `ARBOS_GITHUB_CLIENT_ID`, `ARBOS_GITHUB_CLIENT_SECRET`, `ARBOS_PUBLIC_URL`, `ARBOS_JWT_KEY` | GitHub OAuth app and the relay's public origin (§3) |
| `ARBOS_DEFAULT_GRANT_USD` (100), `ARBOS_DEFAULT_DAILY_CAP_USD` (20), `ARBOS_GLOBAL_DAILY_CAP_USD` (500) | inference credits: welcome grant, per-user daily cap, relay-wide daily cap that trips the kill switch (`PUT /admin/killswitch`) |
| `ARBOS_ALWAYS_ON` | machines stay on (default on in `github` mode): no auto-stop, a machine per signed-in user from sign-in; manual Stop and the kill switch are the only things that keep one down |
| `ARBOS_MACHINE_USD_PER_HOUR` (0.332), `ARBOS_MACHINE_STOPPED_USD_PER_DAY` (0.013), `ARBOS_GLOBAL_MACHINE_DAILY_CAP_USD` (400) | machine time is metered into its own ledger bucket, outside the grant and caps; the global machine cap only logs a warning |
| `ARBOS_AGENTD_BIN`, `ARBOS_PI_BIN`, `ARBOS_PI_EXT`, `ARBOS_PRIME_BIN`, `ARBOS_DEFAULT_HARNESS` | process-box binaries and default subagent harness |
| `ARBOS_MEDIA_VENV` | python with the document libraries for process boxes (`scripts/box-media-deps.sh` prints the export) |
| `ARBOS_GOOGLE_CLIENT_ID/SECRET`, `ARBOS_SLACK_CLIENT_ID/SECRET`, `ARBOS_GITHUB_CLIENT_ID/SECRET`, `ARBOS_OAUTH_REDIRECT_URL`, `ARBOS_HARNESS_COMMS_URL`, `ARBOS_CONNECTORS_MOCK` | connector OAuth apps (redirect `{public_url}/connectors/oauth/callback`); `ARBOS_CONNECTORS_MOCK=1` returns canned data |
| `ARBOS_SLACK_SIGNING_SECRET`, `ARBOS_SLACK_BOT_TOKEN`, `ARBOS_TELEGRAM_BOT_TOKEN`, `ARBOS_TELEGRAM_WEBHOOK_SECRET`, `ARBOS_TELEGRAM_POLL`, `ARBOS_WHATSAPP_TOKEN`, `ARBOS_WHATSAPP_PHONE_ID`, `ARBOS_WHATSAPP_VERIFY_TOKEN`, `ARBOS_WHATSAPP_APP_SECRET`, `ARBOS_BEEPER_POLL_S`, `ARBOS_CHANNELS_MOCK` | channel adapters (Slack Events API at `{public_url}/channels/slack/events`, Telegram webhook or long polling, WhatsApp Cloud API webhook, Beeper via Harness Comms); `ARBOS_CHANNELS_MOCK=1` turns every platform into a mock |
| `APNS_KEY_P8`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC` | iPhone notifications and VoIP call pushes over APNs (token auth; the `.p8` as text or a path); without them push is off and the relay says so once |
| `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_WEBHOOK_SECRET` | voice/video calls on your own LiveKit (§9); without them the call endpoints answer 503 `calls_off` |
| `ARBOS_VAULT_MOCK` | seeds two demo vault items and a local egress echo target |
| `RUST_LOG` | e.g. `info,arbos_relay=debug` |

Example for a public relay on one server, TLS terminated by a reverse proxy (Caddy, nginx) or a Cloudflare tunnel in front of the plain-HTTP relay:

```bash
ARBOS_RELAY_LISTEN=0.0.0.0:7700 ARBOS_RELAY_PUBLIC_URL=https://relay.example.com cargo run --release -p arbos-relay
```

`scripts/relay-start.sh` starts the relay with a cloudflared tunnel beside it: `ARBOS_TUNNEL=named` (a Zero Trust tunnel whose token is in `ARBOS_TUNNEL_TOKEN`; `ARBOS_TUNNEL_HOSTNAME` sets the name), `ARBOS_TUNNEL=quick` (dev only — a random `*.trycloudflare.com` URL that changes on every restart), or `ARBOS_TUNNEL=none` with your own `ARBOS_RELAY_PUBLIC_URL`. `scripts/deploy/relay-server.sh` is the systemd + Caddy install used for relay.arbos.life (`--from-source <ref>` builds the relay on the server).

If the public URL changes, the relay writes the new one into every machine at the next wake (`/work/.arbos/relay-url`) and agentd prefers that file; a machine that still cannot say hello within 60 s is bounced once and then shown as `error · box cannot reach relay`.

## 5. Real machines on Daytona

With `DAYTONA_API_KEY` set, `project create` (without `--local-box`) creates a sandbox from `ARBOS_DAYTONA_SNAPSHOT` at **4 vCPU / 8 GiB / 10 GiB** (the Daytona per-sandbox maximum; about $0.33 per active hour, $0.013 per stopped day). The relay must be reachable from the sandbox (`ARBOS_RELAY_PUBLIC_URL`).

- One machine per user; every project is a workspace `/work/<project>` owned by its own Unix user at mode 700, with its own pi session, subagents, SQLite memory/history, blob dir, browser profile and cgroup slice. One `arbos-agentd` per machine supervises them; up to 4 subagents per workspace and 8 per machine run at once.
- `GET /machine` shows state, spec, spend and workspace states; `POST /machine/start|stop` drives it; `arbosctl machine status|start|stop` from a terminal. Per-user snapshot override: `PUT /admin/users/{id}/box {"snapshot": "…"}` before the machine exists.
- `cargo run -p arbos-relay -- daytona-smoke` runs the lifecycle check (`create → stop → start → archive → start → delete`) against the live API.
- Older per-project sandboxes are copied into the user machine with `arbosctl migrate-to-user-box` (idempotent; old sandboxes are stopped and marked, never deleted).

## 6. The machine image

`.github/workflows/image.yml` builds `image/Dockerfile` on an image tag (`image-v<version>`) or by hand (`workflow_dispatch` with a version), pushes it to GHCR as `ghcr.io/unarbos/arbos/arbos-agent:<version>` (public), smoke-tests it, and registers the Daytona snapshot `arbos-agent-<version>` with `image/register-snapshot.sh` (needs the `DAYTONA_API_KEY` repository secret; otherwise run the script by hand). The image bakes in `arbos-agentd`, so every agentd change needs an image rebuild; machines do not update it at boot.

Without CI, the same build runs on any Docker host and goes to Daytona's transient registry (tags are immutable: bump the version for every rebuild):

```bash
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release -p arbos-agentd --target x86_64-unknown-linux-musl
mkdir -p dist && cp target/x86_64-unknown-linux-musl/release/arbos-agentd dist/
docker build --platform linux/amd64 -f image/Dockerfile -t arbos-agent:<v> .
# temporary push credentials: GET $DAYTONA_API_URL/docker-registry/registry-push-access?snapshotName=arbos-agent-<v>
docker tag arbos-agent:<v> cr.app.daytona.io/<project>/arbos-agent:<v> && docker push cr.app.daytona.io/<project>/arbos-agent:<v>
image/register-snapshot.sh arbos-agent-<v> cr.app.daytona.io/<project>/arbos-agent:<v> 4 8 10
```

While no production snapshot is registered, point the relay at the dev image instead: `ARBOS_DAYTONA_DOCKERFILE=image/Dockerfile.dev-inline ARBOS_BOOTSTRAP_DIR=./dist ARBOS_DEFAULT_HARNESS=pi` (`dist/` holds the static agentd and `arbos-pi-ext.tar`; the machine fetches both at boot).

## 7. Pointing the apps at your relay

- **Mac**: `defaults write sh.arbos.mac RelayURL https://relay.example` for the bundled app, or `ARBOS_RELAY_URL=https://relay.example cargo run -p arbos-mac` from source (`ARBOS_RELAY_URL=fixture` runs the seeded demo without a relay). In `paircode` mode the sign-in screen falls back to the one-time code from the relay log. `ARBOS_RELAY_TOKEN` overrides the stored credentials.
- **iPhone**: build `apps/ios/Arbos` with Xcode and enter the relay URL on the pairing screen (`SIMCTL_CHILD_ARBOS_RELAY_URL` / `_TOKEN` for simulator runs).
- **Terminal**: `arbosctl --relay https://relay.example pair <code>`.
- **Your own computer**: `arbos-local pair <code> --relay https://relay.example --name "My laptop"` then `arbos-local run` lets an agent run a command or read a file on it, behind approval cards (Settings → This Mac lists paired helpers).

## 8. Updates

Self-built relays update by rebuilding. The Mac app updates itself from the appcast of the channel it was built on (`ARBOS_UPDATE_CHANNEL=stable|edge`, `ARBOS_UPDATE_FEED=<url>` to point at your own feed, `ARBOS_UPDATE_DISABLE=1` to turn it off). `scripts/bundle-mac.sh` builds a universal `dist/Arbos.app`, the DMG and the updater tarball, signing and notarizing when the credentials are present and saying what it skipped otherwise.

## 9. Calls (LiveKit)

Voice and video calls between people run on a [LiveKit](https://github.com/livekit/livekit) server you host next to the relay. The relay decides who may be in a call, mints the join tokens, and keeps the call log; media goes LiveKit ↔ apps and never through the relay.

1. Run `livekit-server` with a key pair and a webhook pointing at the relay (`livekit.yaml`):

```yaml
port: 7880
rtc:
  tcp_port: 7881
  port_range_start: 50000
  port_range_end: 60000
  use_external_ip: true
keys:
  <LIVEKIT_API_KEY>: <LIVEKIT_API_SECRET>
turn:
  enabled: true
  domain: livekit.example.com
  tls_port: 5349
webhook:
  api_key: <LIVEKIT_API_KEY>
  urls:
    - https://relay.example.com/livekit/webhook
```

Put TLS in front of port 7880 as `wss://livekit.example.com` (Caddy: `reverse_proxy localhost:7880`), open UDP 50000–60000 and TCP 7881, and give TURN a certificate (or terminate TURN/TLS on 443 so strict networks work). LiveKit signs each webhook with the API key pair; the relay verifies it with `LIVEKIT_API_SECRET` (set `LIVEKIT_WEBHOOK_SECRET` only when the webhook uses a different key).

2. Give the relay `LIVEKIT_URL=wss://livekit.example.com`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET` and restart it; the log says `LiveKit calls on`. `arbosctl admin calls` shows the configuration, live calls, and the last ended ones.

3. Call tokens are good for an hour, rooms are named `call_<ulid>`, at most 16 people. A call ends when the last person leaves, when LiveKit reports `room_finished`, or when nobody has been in it for a minute (the relay then closes the LiveKit room too). Ringing on iPhones that are closed needs the APNs key above (PushKit VoIP push, same key).

