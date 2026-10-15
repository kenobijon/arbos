//! Box lifecycle: providers (Daytona, local process, local Docker), wake jobs,
//! keep-alive, liveness (design §4.5 Flow 5, §4.6; crdt-sync §8).

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde_json::{json, Value};

use arbos_core::auth::{hash, mint, TokenKind};
use arbos_core::ids::now_ms;
use arbos_core::wire::BoxState;

use crate::daytona::{CreateSandbox, SandboxState};
use crate::db::Sandbox;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Daytona,
    Process,
    Docker,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "daytona" => Some(Provider::Daytona),
            "process" => Some(Provider::Process),
            "docker" => Some(Provider::Docker),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Daytona => "daytona",
            Provider::Process => "process",
            Provider::Docker => "docker",
        }
    }
}

/// Environment every box gets. No provider key is in it (D2). Project-level settings (models,
/// instructions, chats) are not baked in any more: the box learns them from `/box/hello` and the
/// `/box/projects` poll, one workspace per project.
pub(crate) fn box_env(
    state: &AppState,
    user_id: &str,
    box_token: &str,
    mcp_token: &str,
    relay_url: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("ARBOS_USER_ID".into(), user_id.to_owned());
    env.insert("ARBOS_RELAY_URL".into(), relay_url.to_owned());
    env.insert("ARBOS_BOX_TOKEN".into(), box_token.to_owned());
    env.insert("ARBOS_MCP_TOKEN".into(), mcp_token.to_owned());
    env.insert("PI_TELEMETRY".into(), "0".into());
    env.insert("PI_SKIP_VERSION_CHECK".into(), "1".into());
    env.insert("PRIME_AGENT_TELEMETRY".into(), "0".into());
    env.insert("DO_NOT_TRACK".into(), "1".into());
    if let Ok(h) = std::env::var("ARBOS_DEFAULT_HARNESS") {
        env.insert("ARBOS_DEFAULT_HARNESS".into(), h);
    }
    let _ = state;
    env
}

/// Create the user's box (one Daytona computer per user; created with the user's first project,
/// D32). `snapshot` overrides the relay default (per-user class).
pub async fn create_box(
    state: &AppState,
    user_id: &str,
    provider: Provider,
    snapshot: Option<&str>,
) -> Result<Sandbox> {
    let snapshot_name = snapshot
        .map(str::to_owned)
        .unwrap_or_else(|| state.cfg.daytona_snapshot.clone());
    let box_token = mint(TokenKind::Box);
    let mcp_token = mint(TokenKind::Mcp);
    let mut sb = Sandbox {
        user_id: user_id.to_owned(),
        provider: provider.as_str().to_owned(),
        sandbox_id: None,
        daytona_state: None,
        device_id: String::new(),
        box_token_hash: hash(&box_token),
        mcp_token: mcp_token.clone(),
        snapshot: None,
        last_started_at: None,
        last_stopped_at: None,
        last_seen_at: None,
        box_url: None,
        preview_token: None,
        local_pid: None,
        migrated_from: Vec::new(),
    };
    state.db.upsert_box(&sb)?; // token hash must exist before the box dials in
    state
        .set_box_state(user_id, BoxState::Waking, Some("creating"))
        .await;
    let t0 = Instant::now();
    match provider {
        Provider::Daytona => {
            let dt = state
                .daytona
                .as_ref()
                .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
            let relay_url = state.cfg.public_url.clone();
            let env = box_env(state, user_id, &box_token, &mcp_token, &relay_url);
            let snapshot_ok = dt.get_snapshot(&snapshot_name).await.is_ok();
            let inline = match (&state.cfg.daytona_dockerfile, snapshot_ok) {
                (Some(p), false) => Some(
                    std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
                ),
                _ => None,
            };
            // Unique per sandbox: a retained old sandbox (upgrade fallback) keeps its name, and
            // Daytona refuses a duplicate with 409.
            let suffix = arbos_core::ids::ulid().to_ascii_lowercase();
            let mut body = CreateSandbox {
                name: Some(format!(
                    "arbos-{}-{}",
                    user_id
                        .to_ascii_lowercase()
                        .replace(|c: char| !c.is_ascii_alphanumeric(), "-"),
                    &suffix[suffix.len().saturating_sub(6)..]
                )),
                snapshot: if inline.is_none() {
                    Some(snapshot_name.clone())
                } else {
                    None
                },
                cpu: inline.as_ref().map(|_| 4),
                memory: inline.as_ref().map(|_| 8),
                disk: inline.as_ref().map(|_| 40),
                build_info: inline.map(|d| json!({ "dockerfileContent": d })),
                // Always-on (default in GitHub mode): Daytona never stops the box by itself.
                auto_stop_interval: Some(if state.cfg.always_on { 0 } else { 30 }),
                auto_archive_interval: Some(7 * 24 * 60),
                auto_delete_interval: Some(-1),
                ttl_minutes: Some(0),
                ..Default::default()
            };
            for (k, v) in env {
                body.env.insert(k, Value::String(v));
            }
            body.labels
                .insert("arbos.user".into(), Value::String(user_id.to_owned()));
            let created = dt.create(&body).await?;
            sb.sandbox_id = Some(created.id.clone());
            sb.device_id = format!("box:{}", created.id);
            sb.snapshot = Some(snapshot_name.clone());
            sb.daytona_state = Some(format!("{:?}", created.state).to_ascii_lowercase());
            state.db.upsert_box(&sb)?;
            let started = dt
                .wait_for(&created.id, Duration::from_secs(180), |s| {
                    s.state == SandboxState::Started
                })
                .await?;
            sb.daytona_state = Some(format!("{:?}", started.state).to_ascii_lowercase());
            sb.last_started_at = Some(now_ms());
            if let Ok(p) = dt.preview_url(&created.id, 7788).await {
                sb.box_url = Some(p.url);
                sb.preview_token = Some(p.token);
            }
            state.db.upsert_box(&sb)?;
            push_relay_url(state, dt, &created.id).await;
            state.db.record_timing(
                &timing_key(user_id),
                "daytona_create_to_started_ms",
                t0.elapsed().as_millis() as i64,
                created.runner_id.as_deref(),
            )?;
        }
        Provider::Process => {
            sb.sandbox_id = Some(format!("local-{user_id}"));
            sb.device_id = format!("box:local-{user_id}");
            state.db.upsert_box(&sb)?;
            save_local_tokens(state, user_id, &box_token, &mcp_token)?;
            start_process(state, user_id, &mut sb, &box_token, &mcp_token).await?;
            state.db.record_timing(
                &timing_key(user_id),
                "local_create_ms",
                t0.elapsed().as_millis() as i64,
                None,
            )?;
        }
        Provider::Docker => {
            let name = format!("arbos-box-{}", user_id.to_ascii_lowercase());
            sb.sandbox_id = Some(name.clone());
            sb.device_id = format!("box:{name}");
            state.db.upsert_box(&sb)?;
            save_local_tokens(state, user_id, &box_token, &mcp_token)?;
            start_docker(state, user_id, &mut sb, &box_token, &mcp_token, true).await?;
            state.db.record_timing(
                &timing_key(user_id),
                "docker_create_ms",
                t0.elapsed().as_millis() as i64,
                None,
            )?;
        }
    }
    state
        .wake_started
        .lock()
        .await
        .insert(user_id.to_owned(), now_ms());
    Ok(sb)
}

/// Box-level timings are recorded under the user key (`GET /projects/{id}/timings` merges them in).
pub fn timing_key(user_id: &str) -> String {
    format!("user:{user_id}")
}

/// Four consecutive free loopback ports for a process box (main, MCP, hook, model stub), chosen
/// by binding rather than hashing the project id (ULID digits w–z are not base-32 digits).
async fn allocate_local_ports(state: &AppState) -> Result<u16> {
    let taken: std::collections::HashSet<u16> = state
        .db
        .list_boxes()?
        .into_iter()
        .filter_map(|s| {
            s.box_url
                .and_then(|u| u.rsplit(':').next().and_then(|p| p.parse().ok()))
        })
        .collect();
    let mut base = 17_000u16;
    while base < 60_000 {
        if (0..4).all(|i| {
            !taken.contains(&(base + i))
                && std::net::TcpListener::bind(("127.0.0.1", base + i)).is_ok()
        }) {
            return Ok(base);
        }
        base += 4;
    }
    Err(anyhow!("no free local port range"))
}

async fn start_process(
    state: &AppState,
    user_id: &str,
    sb: &mut Sandbox,
    box_token: &str,
    mcp_token: &str,
) -> Result<()> {
    let work = state.cfg.data_dir.join("boxes").join(user_id).join("work");
    std::fs::create_dir_all(&work)?;
    // A process box that is still alive (relay restart, double wake) must go before a new one binds.
    if let Some(mut child) = state.local_children.lock().await.remove(user_id) {
        if let Some(pid) = child.id() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    } else if let Some(pid) = sb.local_pid {
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let port = allocate_local_ports(state).await?;
    let env = box_env(state, user_id, box_token, mcp_token, &state.cfg.public_url);
    let mut cmd = tokio::process::Command::new(&state.cfg.agentd_bin);
    // D2 also holds for a local box: start from an empty environment and pass an allow-list, so the
    // relay's provider and Daytona keys never reach agentd or its children.
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        let pass = matches!(
            k.as_str(),
            "PATH" | "HOME" | "LANG" | "LC_ALL" | "TERM" | "TMPDIR" | "RUST_LOG" | "RUST_BACKTRACE"
        ) || ((k.starts_with("ARBOS_")
            || k.starts_with("PRIME_AGENT_")
            || k.starts_with("PI_"))
            && !k.contains("KEY")
            && !k.contains("TOKEN"));
        if pass {
            cmd.env(k, v);
        }
    }
    cmd.arg("boot")
        .envs(env)
        .env("ARBOS_STATE_DIR", work.join(".arbos"))
        .env("ARBOS_WORK_DIR", &work)
        .env("ARBOS_PORT", port.to_string())
        .env("ARBOS_BIND", "127.0.0.1")
        .env("ARBOS_LOCAL_BOX", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(
            work.join("agentd.out.log"),
        )?))
        .stderr(Stdio::from(std::fs::File::create(
            work.join("agentd.err.log"),
        )?))
        .kill_on_drop(false);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", state.cfg.agentd_bin.display()))?;
    sb.local_pid = child.id().map(i64::from);
    sb.box_url = Some(format!("http://127.0.0.1:{port}"));
    sb.last_started_at = Some(now_ms());
    state.db.upsert_box(sb)?;
    // The relay URL file for a local box: same contract as Daytona, written straight to disk.
    let _ = std::fs::create_dir_all(work.join(".arbos"));
    let _ = std::fs::write(
        work.join(".arbos").join("relay-url"),
        format!("{}\n", state.cfg.public_url.trim_end_matches('/')),
    );
    state
        .local_children
        .lock()
        .await
        .insert(user_id.to_owned(), child);
    Ok(())
}

async fn start_docker(
    state: &AppState,
    user_id: &str,
    sb: &mut Sandbox,
    box_token: &str,
    mcp_token: &str,
    create: bool,
) -> Result<()> {
    let name = sb.sandbox_id.clone().unwrap_or_default();
    if create {
        let relay_url = state
            .cfg
            .public_url
            .replace("127.0.0.1", "host.docker.internal")
            .replace("localhost", "host.docker.internal");
        let env = box_env(state, user_id, box_token, mcp_token, &relay_url);
        let mut cmd = tokio::process::Command::new("docker");
        cmd.args([
            "run",
            "-d",
            "--name",
            &name,
            "--add-host",
            "host.docker.internal:host-gateway",
            "-p",
            "127.0.0.1::7788",
        ]);
        for (k, v) in env {
            cmd.arg("-e").arg(format!("{k}={v}"));
        }
        cmd.arg(&state.cfg.local_box_image);
        let out = cmd.output().await.context("docker run")?;
        if !out.status.success() {
            return Err(anyhow!(
                "docker run failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    } else {
        let out = tokio::process::Command::new("docker")
            .args(["start", &name])
            .output()
            .await
            .context("docker start")?;
        if !out.status.success() {
            return Err(anyhow!(
                "docker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    let out = tokio::process::Command::new("docker")
        .args(["port", &name, "7788/tcp"])
        .output()
        .await?;
    let port_line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    let port = port_line.rsplit(':').next().unwrap_or("7788").to_owned();
    sb.box_url = Some(format!("http://127.0.0.1:{port}"));
    sb.last_started_at = Some(now_ms());
    state.db.upsert_box(sb)?;
    Ok(())
}

/// Start (wake) an existing box. Returns once the start call is accepted; the box's own
/// room dial marks it awake.
pub async fn start_box(state: &AppState, user_id: &str) -> Result<()> {
    let mut sb = match state.db.user_box(user_id)? {
        Some(sb) => sb,
        None => {
            // A user with projects but no box yet (pre-D70 data): create it on first wake.
            let provider = if state.daytona.is_some() {
                Provider::Daytona
            } else {
                Provider::Process
            };
            tracing::info!(%user_id, provider = provider.as_str(), "no box for this user yet; creating one");
            ensure_user_box(state, user_id, provider).await?;
            return Ok(());
        }
    };
    let provider = Provider::parse(&sb.provider).ok_or_else(|| anyhow!("bad provider"))?;
    state
        .wake_started
        .lock()
        .await
        .insert(user_id.to_owned(), now_ms());
    state
        .set_box_state(user_id, BoxState::Waking, Some("starting"))
        .await;
    // The box token is minted once per box (Daytona env is fixed at create). Local providers re-inject it
    // from tokens.json; the relay DB holds only the hash.
    let tokens = load_local_tokens(state, user_id)?;
    match provider {
        Provider::Daytona => {
            let dt = state
                .daytona
                .as_ref()
                .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
            let id = sb
                .sandbox_id
                .clone()
                .ok_or_else(|| anyhow!("no sandbox id"))?;
            let t0 = Instant::now();
            let cur = dt.get(&id).await?;
            let from = cur.state;
            if cur.state == SandboxState::Started {
                // The sandbox runs but agentd is gone (Daytona's daemon does not restart a dead
                // entrypoint): bounce it so the entrypoint session starts again.
                tracing::warn!(%user_id, "sandbox is started but the box is silent; restarting it");
                dt.stop(&id, true).await?;
                dt.wait_for(&id, Duration::from_secs(120), |s| {
                    s.state == SandboxState::Stopped
                })
                .await?;
            }
            dt.start(&id).await?;
            let s = dt
                .wait_for(&id, Duration::from_secs(180), |s| {
                    s.state == SandboxState::Started
                })
                .await?;
            sb.daytona_state = Some(format!("{:?}", s.state).to_ascii_lowercase());
            if let Ok(p) = dt.preview_url(&id, 7788).await {
                sb.box_url = Some(p.url);
                sb.preview_token = Some(p.token);
            }
            sb.last_started_at = Some(now_ms());
            state.db.upsert_box(&sb)?;
            if state.cfg.always_on {
                // Boxes created before always-on still carry a 30-minute autostop.
                let _ = dt.set_autostop(&id, 0).await;
            }
            state.db.record_timing(
                &timing_key(user_id),
                &format!("daytona_start_from_{:?}_ms", from).to_ascii_lowercase(),
                t0.elapsed().as_millis() as i64,
                None,
            )?;
            push_relay_url(state, dt, &id).await;
        }
        Provider::Process => {
            let (box_token, mcp_token) = tokens.ok_or_else(|| anyhow!("local tokens missing"))?;
            start_process(state, user_id, &mut sb, &box_token, &mcp_token).await?;
        }
        Provider::Docker => {
            let (box_token, mcp_token) = tokens.ok_or_else(|| anyhow!("local tokens missing"))?;
            start_docker(state, user_id, &mut sb, &box_token, &mcp_token, false).await?;
        }
    }
    Ok(())
}

/// Stop (hibernate) a box. `force` is the Stop-project path (§4.7).
pub async fn stop_box(state: &AppState, user_id: &str, force: bool) -> Result<()> {
    let mut sb = state
        .db
        .user_box(user_id)?
        .ok_or_else(|| anyhow!("no box for user {user_id}"))?;
    let provider = Provider::parse(&sb.provider).ok_or_else(|| anyhow!("bad provider"))?;
    match provider {
        Provider::Daytona => {
            let dt = state
                .daytona
                .as_ref()
                .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
            let id = sb
                .sandbox_id
                .clone()
                .ok_or_else(|| anyhow!("no sandbox id"))?;
            let t0 = Instant::now();
            dt.stop(&id, force).await?;
            if force {
                let _ = dt.update_secrets(&id, vec![]).await;
            }
            let s = dt
                .wait_for(&id, Duration::from_secs(120), |s| {
                    matches!(s.state, SandboxState::Stopped | SandboxState::Archived)
                })
                .await?;
            sb.daytona_state = Some(format!("{:?}", s.state).to_ascii_lowercase());
            state.db.record_timing(
                &timing_key(user_id),
                "daytona_stop_ms",
                t0.elapsed().as_millis() as i64,
                None,
            )?;
        }
        Provider::Process => {
            if let Some(mut child) = state.local_children.lock().await.remove(user_id) {
                if let Some(pid) = child.id() {
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
                }
                // One agentd now stops every workspace's children; give it the full grace.
                if tokio::time::timeout(Duration::from_secs(40), child.wait())
                    .await
                    .is_err()
                {
                    let _ = child.kill().await;
                }
            } else if let Some(pid) = sb.local_pid {
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            sb.local_pid = None;
        }
        Provider::Docker => {
            let name = sb.sandbox_id.clone().unwrap_or_default();
            let _ = tokio::process::Command::new("docker")
                .args(["stop", "-t", "10", &name])
                .output()
                .await;
        }
    }
    sb.last_stopped_at = Some(now_ms());
    state.db.upsert_box(&sb)?;
    state.box_conns.lock().await.remove(user_id);
    state
        .set_box_state(
            user_id,
            BoxState::Asleep,
            Some(if force { "stopped_by_user" } else { "stopped" }),
        )
        .await;
    if force {
        for p in state.db.projects_of_user(user_id).unwrap_or_default() {
            let _ = state.db.set_project_state(&p.id, "stopped_by_user");
        }
    }
    Ok(())
}

/// Local providers keep the plaintext box/MCP tokens in the relay data dir so a restart can re-inject them.
pub fn save_local_tokens(
    state: &AppState,
    user_id: &str,
    box_token: &str,
    mcp_token: &str,
) -> Result<()> {
    let dir = state.cfg.data_dir.join("boxes").join(user_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("tokens.json"),
        json!({ "box": box_token, "mcp": mcp_token }).to_string(),
    )?;
    Ok(())
}

pub fn load_local_tokens(state: &AppState, user_id: &str) -> Result<Option<(String, String)>> {
    let p = state
        .cfg
        .data_dir
        .join("boxes")
        .join(user_id)
        .join("tokens.json");
    if !p.exists() {
        return Ok(None);
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    Ok(Some((
        v.get("box")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        v.get("mcp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )))
}

/// A client needs the box: queue a wake obligation unless it is already awake or waking.
/// The sandbox env bakes `ARBOS_RELAY_URL` at create time; the relay's public URL can change
/// (quick tunnels do on every restart). After every start the current URL goes into the box as
/// `/home/daytona/.arbos/relay-url` (and the legacy `/work/.arbos/relay-url` when that directory
/// exists), which agentd prefers over its env at boot. Written with `sudo` through the toolbox
/// shell: the plain toolbox upload runs as the sandbox user and failed with "permission denied"
/// both under the root-owned `/work` (D70) and against a root-written file in a sticky directory.
pub const RELAY_URL_FILE: &str = "/home/daytona/.arbos/relay-url";

async fn push_relay_url(state: &AppState, dt: &crate::daytona::Daytona, sandbox_id: &str) {
    let url = state.cfg.public_url.trim_end_matches('/').to_owned();
    // Written through a root shell rather than the toolbox upload: the upload runs as the
    // sandbox user and cannot replace a file agentd (root) wrote. The file ends up owned by the
    // sandbox user, world-readable, in a non-sticky directory, so either side can rewrite it.
    let dir = std::path::Path::new(RELAY_URL_FILE)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/home/daytona/.arbos".to_owned());
    let legacy_dir = "/work/.arbos";
    let cmd = format!(
        "sudo -n sh -c 'mkdir -p {dir} && printf \"%s\\n\" \"{url}\" > {file} && chown daytona:daytona {dir} {file} 2>/dev/null; chmod 0777 {dir}; chmod 0666 {file}; \
         if [ -d {legacy_dir} ]; then printf \"%s\\n\" \"{url}\" > {legacy_dir}/relay-url 2>/dev/null && chmod 0666 {legacy_dir}/relay-url 2>/dev/null; fi; echo ok'",
        file = RELAY_URL_FILE,
    );
    for attempt in 1..=4u32 {
        match dt.toolbox_exec(sandbox_id, &cmd, 30).await {
            Ok(out) if out.get("exitCode").and_then(Value::as_i64).unwrap_or(1) == 0 => {
                tracing::info!(sandbox_id, %url, attempt, "relay url pushed into the box");
                return;
            }
            Ok(out) => {
                tracing::warn!(sandbox_id, attempt, result = ?out.get("result"), "relay url push failed");
            }
            Err(e) => {
                tracing::warn!(sandbox_id, attempt, error = %e, "relay url push failed");
            }
        }
        tokio::time::sleep(Duration::from_millis(1500 * attempt as u64)).await;
    }
}

/// A started box must say hello within this long, or it is treated as unable to reach the relay.
pub const HELLO_DEADLINE_MS: i64 = 60_000;
/// A wake timer older than this is stale and is not recorded as `wake_to_box_connected_ms`.
const STALE_WAKE_TIMER_MS: i64 = 5 * 60_000;
/// Reasons that count as the user asking again (they reset a `box cannot reach relay` error).
fn explicit_wake(reason: &str) -> bool {
    matches!(reason, "wake" | "start" | "open" | "api")
}

/// `ensure_awake` for the box that serves a project.
pub async fn ensure_awake_project(state: &AppState, project_id: &str, reason: &str) {
    if let Some(u) = state.user_of(project_id) {
        ensure_awake(state, &u, reason).await;
    }
}

/// The user's box, created on first use (D32 moved from project to user).
pub async fn ensure_user_box(
    state: &AppState,
    user_id: &str,
    provider: Provider,
) -> Result<Sandbox> {
    if let Some(sb) = state.db.user_box(user_id)? {
        return Ok(sb);
    }
    let snapshot = state.db.user_snapshot(user_id)?;
    create_box(state, user_id, provider, snapshot.as_deref()).await
}

pub async fn ensure_awake(state: &AppState, user_id: &str, reason: &str) {
    let (st, since) = state.box_state(user_id).await;
    if st == BoxState::Awake {
        // Nothing to wake; a timer left from an earlier attempt would only mislabel the next
        // reconnect as a slow wake.
        state.wake_started.lock().await.remove(user_id);
        return;
    }
    // Only the kill switch keeps a machine down: machine time is outside the credit balance and
    // caps (machines are always on), so an empty inference balance never blocks a wake.
    if let Err(r) = crate::credits::check_read_only(state) {
        tracing::info!(%user_id, %r, "wake refused");
        state
            .set_box_state(user_id, BoxState::Error, Some(&r.to_string()))
            .await;
        return;
    }
    if explicit_wake(reason) {
        let _ = state.db.set_box_held(user_id, false);
    }
    if st == BoxState::Error {
        // Stale-box error: no more automatic retries; an explicit wake/start tries again once.
        if !explicit_wake(reason) {
            return;
        }
        state.stale_bounces.lock().await.remove(user_id);
    }
    if st == BoxState::Waking {
        // Already waking: either still inside the hello deadline (nothing to do) or stale, in
        // which case `stale_wake_check` has bounced it once or flipped it to error.
        let _ = stale_wake_check(state, user_id, since).await;
        return;
    }
    if let Err(e) = state.db.upsert_wake_job(user_id, reason) {
        tracing::warn!(error = %e, "wake job");
    }
    state
        .wake_started
        .lock()
        .await
        .insert(user_id.to_owned(), now_ms());
    state
        .set_box_state(user_id, BoxState::Waking, Some(reason))
        .await;
}

/// Stale-box detector (runs from the liveness sweep and from `ensure_awake`): a box that has been
/// waking for longer than `HELLO_DEADLINE_MS` without a socket is re-pushed the relay URL and
/// bounced once; the second time it becomes `error · box cannot reach relay` and the wake job
/// ends, so the relay stops waking it forever. Returns true when it acted.
async fn stale_wake_check(state: &AppState, user_id: &str, since: i64) -> bool {
    let connected = state
        .box_conns
        .lock()
        .await
        .get(user_id)
        .map(|c| c.0 > 0)
        .unwrap_or(false);
    if connected || now_ms() - since < HELLO_DEADLINE_MS {
        return false;
    }
    let bounces = {
        let mut m = state.stale_bounces.lock().await;
        let n = m.entry(user_id.to_owned()).or_insert(0);
        *n += 1;
        *n
    };
    let _ = state.db.finish_wake_job(user_id);
    if bounces > 1 {
        tracing::error!(%user_id, "box started but never reached the relay; giving up");
        state
            .set_box_state(user_id, BoxState::Error, Some("box cannot reach relay"))
            .await;
        return true;
    }
    tracing::warn!(%user_id, "box has been waking for >60 s without connecting; re-pushing the relay url and bouncing once");
    if let Err(e) = state.db.upsert_wake_job(user_id, "stale box: bounce") {
        tracing::warn!(error = %e, "wake job");
    }
    state
        .wake_started
        .lock()
        .await
        .insert(user_id.to_owned(), now_ms());
    // Restart the clock: the next check happens one deadline after the bounce.
    state
        .box_states
        .write()
        .await
        .insert(user_id.to_owned(), (BoxState::Waking, now_ms()));
    true
}

pub async fn box_connected(state: &AppState, user_id: &str) {
    {
        let mut c = state.box_conns.lock().await;
        let e = c.entry(user_id.to_owned()).or_insert((0, 0));
        e.0 += 1;
    }
    state.stale_bounces.lock().await.remove(user_id);
    let _ = state.db.finish_wake_job(user_id);
    if let Some(t0) = state.wake_started.lock().await.remove(user_id) {
        // A timer older than the hello deadline belongs to an abandoned wake (the box was
        // already awake, or a wake job gave up); it is not a measurement.
        let elapsed = now_ms() - t0;
        if elapsed <= STALE_WAKE_TIMER_MS {
            let _ = state
                .db
                .record_timing(user_id, "wake_to_box_connected_ms", elapsed, None);
        } else {
            tracing::debug!(%user_id, elapsed, "stale wake timer dropped");
        }
    }
    state.set_box_state(user_id, BoxState::Awake, None).await;
}

pub async fn box_disconnected(state: &AppState, user_id: &str) {
    let mut c = state.box_conns.lock().await;
    let e = c.entry(user_id.to_owned()).or_insert((0, 0));
    e.0 = e.0.saturating_sub(1);
    e.1 = now_ms();
}

/// Called every 5 s: a box with no sockets for `box_idle_grace_ms` is asleep (unless it was never seen),
/// and a box waking past the hello deadline goes through the stale-box detector.
pub async fn sweep_box_liveness(state: &AppState) {
    let waking: Vec<(String, i64)> = state
        .box_states
        .read()
        .await
        .iter()
        .filter(|(_, (st, _))| *st == BoxState::Waking)
        .map(|(k, (_, since))| (k.clone(), *since))
        .collect();
    for (user_id, since) in waking {
        stale_wake_check(state, &user_id, since).await;
    }
    let snapshot: Vec<(String, u32, i64)> = state
        .box_conns
        .lock()
        .await
        .iter()
        .map(|(k, (n, t))| (k.clone(), *n, *t))
        .collect();
    for (user_id, n, last) in snapshot {
        if n == 0 && last > 0 && now_ms() - last > state.cfg.box_idle_grace_ms {
            let (st, _) = state.box_state(&user_id).await;
            if st == BoxState::Awake {
                state
                    .set_box_state(&user_id, BoxState::Asleep, Some("box socket lost"))
                    .await;
                // A client row that landed while the box was already dying (still "awake" in the
                // grace window) has no box to take it: wake again so the command is not stranded.
                let pushed = state
                    .last_client_push
                    .lock()
                    .await
                    .get(&user_id)
                    .copied()
                    .unwrap_or(0);
                if pushed >= last - 5_000 {
                    tracing::warn!(%user_id, "box died with client rows outstanding; waking it again");
                    ensure_awake(state, &user_id, "rows pending after box loss").await;
                }
            }
        }
    }
}

/// Move a deleted project's `/work/<id>` to `/work/.archive/<id>` on the user's box (Daytona
/// through the toolbox shell, local providers on disk). Best effort: the purge removes whatever
/// is left seven days later.
pub async fn archive_workspace_dir(state: &AppState, user_id: &str, project_id: &str) {
    let Ok(Some(sb)) = state.db.user_box(user_id) else {
        return;
    };
    match (
        Provider::parse(&sb.provider),
        &state.daytona,
        &sb.sandbox_id,
    ) {
        (Some(Provider::Daytona), Some(dt), Some(id)) => {
            // Give the box's poll a moment to stop the workspace first.
            tokio::time::sleep(Duration::from_secs(20)).await;
            let cmd = format!(
                "sudo -n sh -c 'mkdir -p /work/.archive && if [ -d /work/{p} ]; then mv /work/{p} /work/.archive/{p}; fi; echo ok'",
                p = project_id
            );
            match dt.toolbox_exec(id, &cmd, 120).await {
                Ok(_) => {
                    tracing::info!(%user_id, project_id, "workspace folder archived on the box")
                }
                Err(e) => tracing::warn!(%user_id, project_id, error = %e, "workspace archive"),
            }
        }
        (Some(Provider::Process), _, _) | (Some(Provider::Docker), _, _) => {
            let base = state.cfg.data_dir.join("boxes").join(user_id).join("work");
            let src = base.join(project_id);
            if src.exists() {
                let dst = base.join(".archive").join(project_id);
                let _ = std::fs::create_dir_all(dst.parent().unwrap_or(&base));
                let _ = std::fs::rename(&src, &dst);
            }
        }
        _ => {}
    }
}

/// Hourly: projects soft-deleted more than seven days ago lose their rows, rooms, blobs, and the
/// archived folder on the box.
pub async fn spawn_project_purge(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let cutoff = now_ms() - 7 * 24 * 3_600_000;
            for (project_id, user_id) in
                state.db.projects_deleted_before(cutoff).unwrap_or_default()
            {
                if let Ok(Some(sb)) = state.db.user_box(&user_id) {
                    if let (Some(dt), Some(id)) = (&state.daytona, &sb.sandbox_id) {
                        if sb.provider == "daytona" {
                            let _ = dt
                                .toolbox_exec(
                                    id,
                                    &format!("sudo -n rm -rf /work/.archive/{project_id}; echo ok"),
                                    120,
                                )
                                .await;
                        }
                    } else {
                        let _ = std::fs::remove_dir_all(
                            state
                                .cfg
                                .data_dir
                                .join("boxes")
                                .join(&user_id)
                                .join("work")
                                .join(".archive")
                                .join(&project_id),
                        );
                    }
                }
                let _ = std::fs::remove_dir_all(state.cfg.data_dir.join("blobs").join(&project_id));
                match state.db.purge_project(&project_id) {
                    Ok(rooms) => {
                        tracing::info!(project_id, rooms = rooms.len(), "deleted project purged")
                    }
                    Err(e) => tracing::warn!(project_id, error = %e, "project purge"),
                }
            }
        }
    });
}

/// Always-on (Jacob, 2026-10-15): every signed-in user's machine runs all the time. At boot and
/// every minute, any box that is not awake or waking — and that its user did not stop by hand —
/// is woken; a member without a box yet gets one. Manual Stop (`held_stopped`) and the kill
/// switch are the only things that keep a machine down.
pub async fn spawn_always_on(state: AppState) {
    if !state.cfg.always_on {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let users = state.db.always_on_users().unwrap_or_default();
            for user_id in users {
                let (st, _) = state.box_state(&user_id).await;
                if matches!(st, BoxState::Awake | BoxState::Waking) {
                    continue;
                }
                if st == BoxState::Error {
                    // A stale-box error stays until the user asks again (see `explicit_wake`).
                    continue;
                }
                tracing::info!(%user_id, "always-on: waking");
                ensure_awake(&state, &user_id, "always-on").await;
            }
        }
    });
}

/// Wake-job runner: 3 attempts with backoff, then S13 error (crdt-sync §8.4 step 5).
pub async fn spawn_wake_runner(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(750)).await;
            let jobs = match state.db.due_wake_jobs() {
                Ok(j) => j,
                Err(_) => continue,
            };
            for (user_id, reason, attempts) in jobs {
                let (st, _) = state.box_state(&user_id).await;
                if st == BoxState::Awake || st == BoxState::Error {
                    // Awake: the dial finished it. Error: the stale-box detector gave up; only an
                    // explicit wake/start re-arms it.
                    let _ = state.db.finish_wake_job(&user_id);
                    continue;
                }
                // Mark the job as in flight by pushing its due time out; the box dial finishes it.
                let _ = state.db.retry_wake_job(&user_id, 120_000, "in flight");
                tracing::info!(%user_id, %reason, attempts, "waking box");
                match start_box(&state, &user_id).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!(%user_id, error = %e, "wake failed");
                        let n = state
                            .db
                            .retry_wake_job(&user_id, 5_000 * (attempts + 1), &e.to_string())
                            .unwrap_or(99);
                        if n >= 3 {
                            let _ = state.db.finish_wake_job(&user_id);
                            state
                                .set_box_state(&user_id, BoxState::Error, Some(&e.to_string()))
                                .await;
                        }
                    }
                }
            }
        }
    });
}

/// Keep-alive from the box while a turn runs (design §4.6): forward as "update last activity".
pub async fn keepalive(state: &AppState, user_id: &str, turn_running: bool) -> Result<()> {
    let sb = state
        .db
        .user_box(user_id)?
        .ok_or_else(|| anyhow!("no box for user {user_id}"))?;
    if sb.provider == "daytona" {
        if let (Some(dt), Some(id)) = (&state.daytona, &sb.sandbox_id) {
            dt.refresh_activity(id).await?;
            let minutes = if state.cfg.always_on || turn_running {
                0
            } else {
                30
            };
            let _ = dt.set_autostop(id, minutes).await;
        }
    }
    state.db.touch_box(user_id)?;
    Ok(())
}

/// Current provider view of the user's box, for `GET /projects/{id}/status`.
pub async fn provider_status(state: &AppState, user_id: &str) -> Value {
    let sb = match state.db.user_box(user_id) {
        Ok(Some(s)) => s,
        _ => return json!({ "provider": null }),
    };
    let mut v = json!({
        "provider": sb.provider, "sandbox_id": sb.sandbox_id, "box_url": sb.box_url, "user_id": sb.user_id,
        "migrated_from": sb.migrated_from,
        "last_started_at": sb.last_started_at, "last_stopped_at": sb.last_stopped_at, "last_seen_at": sb.last_seen_at,
    });
    if sb.provider == "daytona" {
        if let (Some(dt), Some(id)) = (&state.daytona, &sb.sandbox_id) {
            if let Ok(s) = dt.get(id).await {
                v["daytona_state"] = json!(s.state);
                v["daytona_arbos_state"] = json!(s.state.arbos_state());
                v["auto_stop_interval"] = json!(s.auto_stop_interval);
                v["last_activity_at"] = json!(s.last_activity_at);
            }
        }
    }
    v
}
