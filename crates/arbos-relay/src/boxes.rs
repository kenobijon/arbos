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
use crate::db::{Project, Sandbox};
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

/// Environment every box gets. No provider key is in it (D2).
fn box_env(
    state: &AppState,
    project: &Project,
    box_token: &str,
    mcp_token: &str,
    relay_url: &str,
) -> BTreeMap<String, String> {
    let models = &project.models;
    let talking = models
        .pointer("/talking_model/id")
        .and_then(Value::as_str)
        .unwrap_or("anthropic/claude-haiku-5.5");
    let subagent = models
        .pointer("/subagent_model/id")
        .and_then(Value::as_str)
        .unwrap_or("anthropic/claude-sonnet-5.5");
    let mut env = BTreeMap::new();
    env.insert("ARBOS_PROJECT_ID".into(), project.id.clone());
    env.insert("ARBOS_PROJECT_NAME".into(), project.name.clone());
    env.insert("ARBOS_RELAY_URL".into(), relay_url.to_owned());
    env.insert("ARBOS_BOX_TOKEN".into(), box_token.to_owned());
    env.insert("ARBOS_MCP_TOKEN".into(), mcp_token.to_owned());
    env.insert("ARBOS_TALKING_MODEL".into(), talking.to_owned());
    env.insert("ARBOS_SUBAGENT_MODEL".into(), subagent.to_owned());
    env.insert("ARBOS_INSTRUCTIONS".into(), project.instructions.clone());
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

/// Create the box for a new project (D32: at project creation, not at first message).
pub async fn create_box(
    state: &AppState,
    project: &Project,
    provider: Provider,
) -> Result<Sandbox> {
    let box_token = mint(TokenKind::Box);
    let mcp_token = mint(TokenKind::Mcp);
    let mut sb = Sandbox {
        project_id: project.id.clone(),
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
    };
    state.db.upsert_sandbox(&sb)?; // token hash must exist before the box dials in
    state
        .set_box_state(&project.id, BoxState::Waking, Some("creating"))
        .await;
    let t0 = Instant::now();
    match provider {
        Provider::Daytona => {
            let dt = state
                .daytona
                .as_ref()
                .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
            let relay_url = state.cfg.public_url.clone();
            let env = box_env(state, project, &box_token, &mcp_token, &relay_url);
            let snapshot_ok = dt.get_snapshot(&state.cfg.daytona_snapshot).await.is_ok();
            let inline = match (&state.cfg.daytona_dockerfile, snapshot_ok) {
                (Some(p), false) => Some(
                    std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
                ),
                _ => None,
            };
            let mut body = CreateSandbox {
                name: Some(format!(
                    "arbos-{}",
                    project
                        .name
                        .to_ascii_lowercase()
                        .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
                )),
                snapshot: if inline.is_none() {
                    Some(state.cfg.daytona_snapshot.clone())
                } else {
                    None
                },
                cpu: inline.as_ref().map(|_| 2),
                memory: inline.as_ref().map(|_| 4),
                disk: inline.as_ref().map(|_| 10),
                build_info: inline.map(|d| json!({ "dockerfileContent": d })),
                auto_stop_interval: Some(30),
                auto_archive_interval: Some(7 * 24 * 60),
                auto_delete_interval: Some(-1),
                ttl_minutes: Some(0),
                ..Default::default()
            };
            for (k, v) in env {
                body.env.insert(k, Value::String(v));
            }
            body.labels
                .insert("arbos.project".into(), Value::String(project.id.clone()));
            let created = dt.create(&body).await?;
            sb.sandbox_id = Some(created.id.clone());
            sb.device_id = format!("box:{}", created.id);
            sb.snapshot = Some(state.cfg.daytona_snapshot.clone());
            sb.daytona_state = Some(format!("{:?}", created.state).to_ascii_lowercase());
            state.db.upsert_sandbox(&sb)?;
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
            state.db.upsert_sandbox(&sb)?;
            state.db.record_timing(
                &project.id,
                "daytona_create_to_started_ms",
                t0.elapsed().as_millis() as i64,
                created.runner_id.as_deref(),
            )?;
        }
        Provider::Process => {
            sb.sandbox_id = Some(format!("local-{}", project.id));
            sb.device_id = format!("box:local-{}", project.id);
            state.db.upsert_sandbox(&sb)?;
            save_local_tokens(state, &project.id, &box_token, &mcp_token)?;
            start_process(state, project, &mut sb, &box_token, &mcp_token).await?;
            state.db.record_timing(
                &project.id,
                "local_create_ms",
                t0.elapsed().as_millis() as i64,
                None,
            )?;
        }
        Provider::Docker => {
            let name = format!("arbos-box-{}", project.id.to_ascii_lowercase());
            sb.sandbox_id = Some(name.clone());
            sb.device_id = format!("box:{name}");
            state.db.upsert_sandbox(&sb)?;
            save_local_tokens(state, &project.id, &box_token, &mcp_token)?;
            start_docker(state, project, &mut sb, &box_token, &mcp_token, true).await?;
            state.db.record_timing(
                &project.id,
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
        .insert(project.id.clone(), now_ms());
    Ok(sb)
}

/// Four consecutive free loopback ports for a process box (main, MCP, hook, model stub), chosen
/// by binding rather than hashing the project id (ULID digits w–z are not base-32 digits).
async fn allocate_local_ports(state: &AppState) -> Result<u16> {
    let taken: std::collections::HashSet<u16> = state
        .db
        .list_projects()?
        .iter()
        .filter_map(|p| state.db.sandbox(&p.id).ok().flatten())
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
    project: &Project,
    sb: &mut Sandbox,
    box_token: &str,
    mcp_token: &str,
) -> Result<()> {
    let work = state
        .cfg
        .data_dir
        .join("boxes")
        .join(&project.id)
        .join("work");
    std::fs::create_dir_all(&work)?;
    // A process box that is still alive (relay restart, double wake) must go before a new one binds.
    if let Some(mut child) = state.local_children.lock().await.remove(&project.id) {
        if let Some(pid) = child.id() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    } else if let Some(pid) = sb.local_pid {
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let port = allocate_local_ports(state).await?;
    let env = box_env(state, project, box_token, mcp_token, &state.cfg.public_url);
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
    state.db.upsert_sandbox(sb)?;
    state
        .local_children
        .lock()
        .await
        .insert(project.id.clone(), child);
    Ok(())
}

async fn start_docker(
    state: &AppState,
    project: &Project,
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
        let env = box_env(state, project, box_token, mcp_token, &relay_url);
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
    state.db.upsert_sandbox(sb)?;
    Ok(())
}

/// Start (wake) an existing box. Returns once the start call is accepted; the box's own
/// room dial marks it awake.
pub async fn start_box(state: &AppState, project_id: &str) -> Result<()> {
    let project = state
        .db
        .project(project_id)?
        .ok_or_else(|| anyhow!("no project"))?;
    let mut sb = state
        .db
        .sandbox(project_id)?
        .ok_or_else(|| anyhow!("no sandbox"))?;
    let provider = Provider::parse(&sb.provider).ok_or_else(|| anyhow!("bad provider"))?;
    state
        .wake_started
        .lock()
        .await
        .insert(project_id.to_owned(), now_ms());
    state
        .set_box_state(project_id, BoxState::Waking, Some("starting"))
        .await;
    // The box token is minted once per box (Daytona env is fixed at create). Local providers re-inject it
    // from tokens.json; the relay DB holds only the hash.
    let tokens = load_local_tokens(state, project_id)?;
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
                tracing::warn!(%project_id, "sandbox is started but the box is silent; restarting it");
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
            state.db.upsert_sandbox(&sb)?;
            state.db.record_timing(
                project_id,
                &format!("daytona_start_from_{:?}_ms", from).to_ascii_lowercase(),
                t0.elapsed().as_millis() as i64,
                None,
            )?;
            push_relay_url(state, dt, &id).await;
        }
        Provider::Process => {
            let (box_token, mcp_token) = tokens.ok_or_else(|| anyhow!("local tokens missing"))?;
            start_process(state, &project, &mut sb, &box_token, &mcp_token).await?;
        }
        Provider::Docker => {
            let (box_token, mcp_token) = tokens.ok_or_else(|| anyhow!("local tokens missing"))?;
            start_docker(state, &project, &mut sb, &box_token, &mcp_token, false).await?;
        }
    }
    Ok(())
}

/// Stop (hibernate) a box. `force` is the Stop-project path (§4.7).
pub async fn stop_box(state: &AppState, project_id: &str, force: bool) -> Result<()> {
    let mut sb = state
        .db
        .sandbox(project_id)?
        .ok_or_else(|| anyhow!("no sandbox"))?;
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
                project_id,
                "daytona_stop_ms",
                t0.elapsed().as_millis() as i64,
                None,
            )?;
        }
        Provider::Process => {
            if let Some(mut child) = state.local_children.lock().await.remove(project_id) {
                if let Some(pid) = child.id() {
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
                }
                let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
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
    state.db.upsert_sandbox(&sb)?;
    state.box_conns.lock().await.remove(project_id);
    state
        .set_box_state(
            project_id,
            BoxState::Asleep,
            Some(if force { "stopped_by_user" } else { "stopped" }),
        )
        .await;
    if force {
        let _ = state.db.set_project_state(project_id, "stopped_by_user");
    }
    Ok(())
}

/// Local providers keep the plaintext box/MCP tokens in the relay data dir so a restart can re-inject them.
pub fn save_local_tokens(
    state: &AppState,
    project_id: &str,
    box_token: &str,
    mcp_token: &str,
) -> Result<()> {
    let dir = state.cfg.data_dir.join("boxes").join(project_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("tokens.json"),
        json!({ "box": box_token, "mcp": mcp_token }).to_string(),
    )?;
    Ok(())
}

pub fn load_local_tokens(state: &AppState, project_id: &str) -> Result<Option<(String, String)>> {
    let p = state
        .cfg
        .data_dir
        .join("boxes")
        .join(project_id)
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
/// `/work/.arbos/relay-url`, which agentd prefers over its env at boot.
pub const RELAY_URL_FILE: &str = "/work/.arbos/relay-url";

async fn push_relay_url(state: &AppState, dt: &crate::daytona::Daytona, sandbox_id: &str) {
    let url = state.cfg.public_url.trim_end_matches('/').to_owned();
    for attempt in 1..=4u32 {
        match dt
            .toolbox_upload(sandbox_id, RELAY_URL_FILE, format!("{url}\n").into_bytes())
            .await
        {
            Ok(()) => {
                tracing::info!(sandbox_id, %url, attempt, "relay url pushed into the box");
                return;
            }
            Err(e) => {
                tracing::warn!(sandbox_id, attempt, error = %e, "relay url push failed");
                tokio::time::sleep(Duration::from_millis(1500 * attempt as u64)).await;
            }
        }
    }
}

/// A started box must say hello within this long, or it is treated as unable to reach the relay.
pub const HELLO_DEADLINE_MS: i64 = 60_000;
/// Reasons that count as the user asking again (they reset a `box cannot reach relay` error).
fn explicit_wake(reason: &str) -> bool {
    matches!(reason, "wake" | "start" | "open")
}

pub async fn ensure_awake(state: &AppState, project_id: &str, reason: &str) {
    let (st, since) = state.box_state(project_id).await;
    if st == BoxState::Awake {
        return;
    }
    if st == BoxState::Error {
        // Stale-box error: no more automatic retries; an explicit wake/start tries again once.
        if !explicit_wake(reason) {
            return;
        }
        state.stale_bounces.lock().await.remove(project_id);
    }
    if st == BoxState::Waking {
        // Already waking: either still inside the hello deadline (nothing to do) or stale, in
        // which case `stale_wake_check` has bounced it once or flipped it to error.
        let _ = stale_wake_check(state, project_id, since).await;
        return;
    }
    if let Err(e) = state.db.upsert_wake_job(project_id, reason) {
        tracing::warn!(error = %e, "wake job");
    }
    state
        .wake_started
        .lock()
        .await
        .insert(project_id.to_owned(), now_ms());
    state
        .set_box_state(project_id, BoxState::Waking, Some(reason))
        .await;
}

/// Stale-box detector (runs from the liveness sweep and from `ensure_awake`): a box that has been
/// waking for longer than `HELLO_DEADLINE_MS` without a socket is re-pushed the relay URL and
/// bounced once; the second time it becomes `error · box cannot reach relay` and the wake job
/// ends, so the relay stops waking it forever. Returns true when it acted.
async fn stale_wake_check(state: &AppState, project_id: &str, since: i64) -> bool {
    let connected = state
        .box_conns
        .lock()
        .await
        .get(project_id)
        .map(|c| c.0 > 0)
        .unwrap_or(false);
    if connected || now_ms() - since < HELLO_DEADLINE_MS {
        return false;
    }
    let bounces = {
        let mut m = state.stale_bounces.lock().await;
        let n = m.entry(project_id.to_owned()).or_insert(0);
        *n += 1;
        *n
    };
    let _ = state.db.finish_wake_job(project_id);
    if bounces > 1 {
        tracing::error!(%project_id, "box started but never reached the relay; giving up");
        state
            .set_box_state(project_id, BoxState::Error, Some("box cannot reach relay"))
            .await;
        return true;
    }
    tracing::warn!(%project_id, "box has been waking for >60 s without connecting; re-pushing the relay url and bouncing once");
    if let Err(e) = state.db.upsert_wake_job(project_id, "stale box: bounce") {
        tracing::warn!(error = %e, "wake job");
    }
    state
        .wake_started
        .lock()
        .await
        .insert(project_id.to_owned(), now_ms());
    // Restart the clock: the next check happens one deadline after the bounce.
    state
        .box_states
        .write()
        .await
        .insert(project_id.to_owned(), (BoxState::Waking, now_ms()));
    true
}

pub async fn box_connected(state: &AppState, project_id: &str) {
    {
        let mut c = state.box_conns.lock().await;
        let e = c.entry(project_id.to_owned()).or_insert((0, 0));
        e.0 += 1;
    }
    state.stale_bounces.lock().await.remove(project_id);
    let _ = state.db.finish_wake_job(project_id);
    if let Some(t0) = state.wake_started.lock().await.remove(project_id) {
        let _ = state
            .db
            .record_timing(project_id, "wake_to_box_connected_ms", now_ms() - t0, None);
    }
    state.set_box_state(project_id, BoxState::Awake, None).await;
}

pub async fn box_disconnected(state: &AppState, project_id: &str) {
    let mut c = state.box_conns.lock().await;
    let e = c.entry(project_id.to_owned()).or_insert((0, 0));
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
    for (project_id, since) in waking {
        stale_wake_check(state, &project_id, since).await;
    }
    let snapshot: Vec<(String, u32, i64)> = state
        .box_conns
        .lock()
        .await
        .iter()
        .map(|(k, (n, t))| (k.clone(), *n, *t))
        .collect();
    for (project_id, n, last) in snapshot {
        if n == 0 && last > 0 && now_ms() - last > state.cfg.box_idle_grace_ms {
            let (st, _) = state.box_state(&project_id).await;
            if st == BoxState::Awake {
                state
                    .set_box_state(&project_id, BoxState::Asleep, Some("box socket lost"))
                    .await;
                // A client row that landed while the box was already dying (still "awake" in the
                // grace window) has no box to take it: wake again so the command is not stranded.
                let pushed = state
                    .last_client_push
                    .lock()
                    .await
                    .get(&project_id)
                    .copied()
                    .unwrap_or(0);
                if pushed >= last - 5_000 {
                    tracing::warn!(%project_id, "box died with client rows outstanding; waking it again");
                    ensure_awake(state, &project_id, "rows pending after box loss").await;
                }
            }
        }
    }
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
            for (project_id, reason, attempts) in jobs {
                let (st, _) = state.box_state(&project_id).await;
                if st == BoxState::Awake || st == BoxState::Error {
                    // Awake: the dial finished it. Error: the stale-box detector gave up; only an
                    // explicit wake/start re-arms it.
                    let _ = state.db.finish_wake_job(&project_id);
                    continue;
                }
                // Mark the job as in flight by pushing its due time out; the box dial finishes it.
                let _ = state.db.retry_wake_job(&project_id, 120_000, "in flight");
                tracing::info!(%project_id, %reason, attempts, "waking box");
                match start_box(&state, &project_id).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!(%project_id, error = %e, "wake failed");
                        let n = state
                            .db
                            .retry_wake_job(&project_id, 5_000 * (attempts + 1), &e.to_string())
                            .unwrap_or(99);
                        if n >= 3 {
                            let _ = state.db.finish_wake_job(&project_id);
                            state
                                .set_box_state(&project_id, BoxState::Error, Some(&e.to_string()))
                                .await;
                        }
                    }
                }
            }
        }
    });
}

/// Keep-alive from the box while a turn runs (design §4.6): forward as "update last activity".
pub async fn keepalive(state: &AppState, project_id: &str, turn_running: bool) -> Result<()> {
    let sb = state
        .db
        .sandbox(project_id)?
        .ok_or_else(|| anyhow!("no sandbox"))?;
    if sb.provider == "daytona" {
        if let (Some(dt), Some(id)) = (&state.daytona, &sb.sandbox_id) {
            dt.refresh_activity(id).await?;
            let _ = dt.set_autostop(id, if turn_running { 0 } else { 30 }).await;
        }
    }
    state.db.touch_sandbox(project_id)?;
    Ok(())
}

/// Current provider view of the box, for `GET /projects/{id}/status`.
pub async fn provider_status(state: &AppState, project_id: &str) -> Value {
    let sb = match state.db.sandbox(project_id) {
        Ok(Some(s)) => s,
        _ => return json!({ "provider": null }),
    };
    let mut v = json!({
        "provider": sb.provider, "sandbox_id": sb.sandbox_id, "box_url": sb.box_url,
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
