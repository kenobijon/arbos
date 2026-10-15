mod agent;
mod config;
mod connectors;
mod harness;
mod hooks;
mod http;
mod llm;
mod mcp;
mod memory;
mod rpc_child;
mod settings;
mod store;
mod sync;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use arbos_core::wire::PROJECT_DOC_CHAT_ID;

use crate::agent::Agent;
use crate::config::Config;
use crate::store::Store;
use crate::sync::{Incoming, SyncHub};

#[derive(Parser, Debug)]
#[command(name = "arbos-agentd", version, about = "Arbos box entrypoint")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Boot the box: doc host, main agent, MCP, history store (image ENTRYPOINT).
    Boot,
    /// Print the effective configuration (secrets masked).
    Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,loro_internal=warn,arbos_agentd=debug".into()),
        )
        .init();
    let args = Args::parse();
    match args.cmd {
        Cmd::Boot => boot().await,
        Cmd::Config => {
            let cfg = Config::from_env()?;
            println!(
                "{}",
                json!({ "project_id": cfg.project_id, "relay_url": cfg.relay_url, "state_dir": cfg.state_dir, "work_dir": cfg.work_dir, "port": cfg.port, "mcp": cfg.mcp_url(), "hook": cfg.hook_base(), "proxy": cfg.proxy_base(), "box_token": mask(&cfg.box_token), "mcp_token": mask(&cfg.mcp_token), "talking_model": cfg.talking_model, "subagent_model": cfg.subagent_model })
            );
            Ok(())
        }
    }
}

fn mask(s: &str) -> String {
    if s.len() > 8 {
        format!("{}…({} chars)", &s[..6], s.len())
    } else {
        "(unset)".into()
    }
}

/// First-boot seeding (sandbox-image.md §4 step 5): pi seed → state dir, AGENTS.md, mcp.json.
fn seed_state(cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(&cfg.state_dir)?;
    std::fs::create_dir_all(cfg.pi_dir())?;
    std::fs::create_dir_all(cfg.pi_sessions_dir())?;
    std::fs::create_dir_all(cfg.blobs_dir())?;
    std::fs::create_dir_all(cfg.state_dir.join("logs"))?;
    std::fs::create_dir_all(cfg.prime_dir())?;
    std::fs::create_dir_all(&cfg.work_dir)?;
    let seed = std::path::Path::new("/etc/arbos/pi-seed");
    if seed.exists() {
        copy_dir(seed, &cfg.pi_dir())?;
    }
    let claude_seed = std::path::Path::new("/etc/arbos/claude-seed/settings.json");
    let claude_dir = cfg.state_dir.join("claude");
    if claude_seed.exists() && !claude_dir.join("settings.json").exists() {
        std::fs::create_dir_all(&claude_dir)?;
        std::fs::copy(claude_seed, claude_dir.join("settings.json"))?;
    }
    // models.json: the relay proxy as provider `arbos-relay` through the loopback stub (D2).
    // The built-in `openrouter` provider pointed at the loopback stub keeps pi's OpenRouter behaviour
    // (cache_control breakpoints for Anthropic models, usage accounting) while the key stays on the relay.
    let models = json!({
        "providers": {
            "openrouter": { "baseUrl": format!("{}/v1", cfg.proxy_base()), "apiKey": cfg.box_token },
            "arbos-relay": {
                "baseUrl": format!("{}/v1", cfg.proxy_base()),
                "api": "openai-completions",
                "apiKey": cfg.box_token,
                "models": [
                    { "id": cfg.talking_model, "name": format!("{} (relay)", cfg.talking_model), "reasoning": false, "input": ["text", "image"], "contextWindow": 1000000, "maxTokens": 32000, "cost": { "input": 0.1, "output": 0.5, "cacheRead": 0.01, "cacheWrite": 0.125 } },
                    { "id": cfg.subagent_model, "name": format!("{} (relay)", cfg.subagent_model), "reasoning": true, "input": ["text", "image"], "contextWindow": 1000000, "maxTokens": 64000, "cost": { "input": 2, "output": 10, "cacheRead": 0.2, "cacheWrite": 2.5 } }
                ]
            }
        }
    });
    std::fs::write(
        cfg.pi_dir().join("models.json"),
        serde_json::to_string_pretty(&models)?,
    )?;
    std::fs::write(
        cfg.prime_dir().join("models.json"),
        serde_json::to_string_pretty(&models)?,
    )?;
    let settings = json!({
        "defaultProvider": cfg.pi_provider(), "defaultModel": cfg.talking_model, "defaultThinkingLevel": "off",
        "defaultTools": ["read", "bash", "edit", "write", "codemode"],
        "compaction": { "enabled": true, "reserveTokens": 16384, "keepRecentTokens": 20000 },
        "hideThinkingBlock": true
    });
    std::fs::write(
        cfg.pi_dir().join("settings.json"),
        serde_json::to_string_pretty(&settings)?,
    )?;
    // Project tools reach the main agent through codemode only: 14 direct schemas cost ~4k prefix tokens (D68).
    let mcp = json!({ "mcpServers": { "arbos": { "url": cfg.mcp_url(), "headers": { "Authorization": format!("Bearer {}", cfg.mcp_token) }, "exposure": "codemode" } } });
    std::fs::write(
        cfg.pi_dir().join("mcp.json"),
        serde_json::to_string_pretty(&mcp)?,
    )?;
    let prime_settings_path = cfg.prime_dir().join("settings.json");
    let mut prime_settings: Value = std::fs::read_to_string(&prime_settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}));
    // Same shape `prime-agent mcp add arbos --url … --bearer-token-env-var ARBOS_MCP_TOKEN` writes.
    prime_settings["mcpServers"] = json!({ "arbos": { "type": "http", "url": cfg.mcp_url(), "bearer_token_env_var": "ARBOS_MCP_TOKEN", "bearerTokenEnvVar": "ARBOS_MCP_TOKEN" } });
    std::fs::write(
        &prime_settings_path,
        serde_json::to_string_pretty(&prime_settings)?,
    )?;
    let agents_md = cfg.work_dir.join("AGENTS.md");
    if !agents_md.exists() {
        let src = std::path::Path::new("/etc/arbos/AGENTS.md");
        if src.exists() {
            std::fs::copy(src, &agents_md)?;
        } else {
            std::fs::write(&agents_md, include_str!("../../../image/AGENTS.md"))?;
        }
    }
    if !cfg.work_dir.join(".git").exists() {
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&cfg.work_dir)
            .status();
    }
    Ok(())
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let p = e.path();
        let target = dst.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &target)?;
        } else if !target.exists() {
            std::fs::copy(&p, &target)?;
        }
    }
    Ok(())
}

/// The relay URL baked into the sandbox env goes stale when the relay's public URL changes (quick
/// tunnels change on every restart). The relay writes the current URL to `<state>/relay-url` at
/// every wake; agentd prefers a reachable file URL over the env, and keeps re-reading the file
/// until one of the candidates answers `/healthz`. Returns the URL that answered.
async fn resolve_relay_url() -> String {
    let state_dir = std::env::var("ARBOS_STATE_DIR").unwrap_or_else(|_| "/work/.arbos".into());
    let file = std::path::Path::new(&state_dir).join("relay-url");
    let env_url = std::env::var("ARBOS_RELAY_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_owned())
        .filter(|u| !u.is_empty());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let file_url = std::fs::read_to_string(&file)
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .filter(|u| !u.is_empty());
        let mut candidates: Vec<String> = Vec::new();
        for u in [file_url.clone(), env_url.clone()].into_iter().flatten() {
            if !candidates.contains(&u) {
                candidates.push(u);
            }
        }
        for u in &candidates {
            let ok = http
                .get(format!("{u}/healthz"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ok {
                if file_url.as_deref() != Some(u.as_str()) {
                    let _ = std::fs::create_dir_all(&state_dir);
                    let _ = std::fs::write(&file, format!("{u}\n"));
                }
                if env_url.as_deref() != Some(u.as_str()) {
                    tracing::warn!(url = %u, env = ?env_url, "relay url from the wake file (env is stale)");
                }
                return u.clone();
            }
        }
        if attempt == 1 || attempt % 15 == 0 {
            tracing::warn!(
                ?candidates,
                attempt,
                "no relay url answers /healthz; waiting for the relay to push one"
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn boot() -> Result<()> {
    let t_boot = Instant::now();
    let relay_url = resolve_relay_url().await;
    // Config reads the env; the resolved URL wins over whatever the sandbox was created with.
    std::env::set_var("ARBOS_RELAY_URL", &relay_url);
    let cfg = Arc::new(Config::from_env()?);
    seed_state(&cfg)?;
    let store = Store::open(&cfg.state_dir.join("agentd").join("agentd.db"))?;
    let (tx, mut rx) = mpsc::channel::<Incoming>(1024);
    let sync = SyncHub::new(cfg.clone(), store.clone(), tx);
    let agent = Agent::new(cfg.clone(), store.clone(), sync.clone());

    // Loopback servers first, so pi's extension can reach the hook API as soon as it loads.
    let hook_app = hooks::router(agent.clone());
    let mcp_app = mcp::router(agent.clone());
    let stub_app = http::stub_router(agent.clone());
    let main_app = http::router(agent.clone());
    let hook_l = tokio::net::TcpListener::bind(("127.0.0.1", cfg.hook_port))
        .await
        .context("bind hook port")?;
    let mcp_l = tokio::net::TcpListener::bind(("127.0.0.1", cfg.mcp_port))
        .await
        .context("bind mcp port")?;
    let stub_l = tokio::net::TcpListener::bind(("127.0.0.1", cfg.proxy_port))
        .await
        .context("bind proxy stub port")?;
    let main_l = tokio::net::TcpListener::bind(format!("{}:{}", cfg.bind, cfg.port))
        .await
        .context("bind main port")?;
    tokio::spawn(async move { axum::serve(hook_l, hook_app).await.ok() });
    tokio::spawn(async move { axum::serve(mcp_l, mcp_app).await.ok() });
    tokio::spawn(async move { axum::serve(stub_l, stub_app).await.ok() });
    tokio::spawn(async move { axum::serve(main_l, main_app).await.ok() });

    // Hello to the relay: settings, chats, queued pushes.
    let http = reqwest::Client::new();
    let hello = http
        .post(format!("{}/projects/{}/box/hello", cfg.relay_url, cfg.project_id))
        .bearer_auth(&cfg.box_token)
        .json(&json!({ "url": format!("http://127.0.0.1:{}", cfg.port), "version": env!("CARGO_PKG_VERSION"), "proto": arbos_core::PROTO_VERSION }))
        .send()
        .await;
    let mut chats: Vec<Value> = Vec::new();
    match hello {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            chats = v
                .get("chats")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for p in v
                .get("pushes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                tracing::info!(push = %p, "queued push delivered at boot");
                let _ = store.t3_append(
                    "projects",
                    None,
                    None,
                    "push",
                    Some("system"),
                    &p.to_string(),
                    None,
                );
            }
        }
        Ok(r) => tracing::warn!(status = %r.status(), "box hello refused"),
        Err(e) => tracing::warn!(error = %e, "box hello failed"),
    }
    for c in &chats {
        if let (Some(id), Some(title)) = (
            c.get("id").and_then(Value::as_str),
            c.get("title").and_then(Value::as_str),
        ) {
            let _ = store.upsert_chat(
                id,
                title,
                c.get("is_pinned").and_then(Value::as_bool).unwrap_or(false),
                c.get("created_at").and_then(Value::as_i64).unwrap_or(0),
            );
        }
    }
    let known: Vec<String> = store.chats()?.into_iter().map(|c| c.id).collect();
    let initial_chat = known
        .first()
        .cloned()
        .unwrap_or_else(|| "default".to_owned());

    // Rooms: project doc + every chat.
    sync.open_room(PROJECT_DOC_CHAT_ID).await?;
    for id in &known {
        sync.open_room(id).await?;
    }
    // Main agent.
    agent.start_pi(&initial_chat).await?;
    sync.write_status(&[
        ("box", json!("awake")),
        ("runningTurns", json!(0)),
        ("spendTodayUsd", json!(0.0)),
    ])
    .await;
    agent.write_threads_index().await;
    tracing::info!(
        ms = t_boot.elapsed().as_millis() as i64,
        chats = known.len(),
        "arbos-agentd booted"
    );
    let _ = agent
        .post_timing("agentd_boot_ms", t_boot.elapsed().as_millis() as i64, None)
        .await;

    // Registry poll (3 s): new chats get a room; settings changes (SetModel, rename) reach pi and the doc.
    {
        let sync2 = sync.clone();
        let store2 = store.clone();
        let cfg2 = cfg.clone();
        let agent2 = agent.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let Ok(r) = reqwest::Client::new()
                    .get(format!("{}/projects/{}", cfg2.relay_url, cfg2.project_id))
                    .bearer_auth(&cfg2.box_token)
                    .send()
                    .await
                else {
                    continue;
                };
                let Ok(v) = r.json::<Value>().await else {
                    continue;
                };
                let chats = v
                    .get("chats")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for c in &chats {
                    if let Some(id) = c.get("id").and_then(Value::as_str) {
                        let _ = store2.upsert_chat(
                            id,
                            c.get("title").and_then(Value::as_str).unwrap_or("New chat"),
                            c.get("is_pinned").and_then(Value::as_bool).unwrap_or(false),
                            c.get("created_at").and_then(Value::as_i64).unwrap_or(0),
                        );
                        if sync2.host(id).await.is_none() {
                            if let Err(e) = sync2.open_room(id).await {
                                tracing::warn!(error = %e, id, "open room");
                            }
                        }
                    }
                }
                if let Some(p) = v.get("project") {
                    if let Err(e) = agent2.settings_from_relay(p, &chats).await {
                        tracing::warn!(error = %e, "settings from relay");
                    }
                }
            }
        });
    }
    // Prime ask_user marker directory (D64 skill) → InputRequested.
    {
        let agent2 = agent.clone();
        let ask_dir = cfg.state_dir.join("ask");
        tokio::spawn(async move {
            let _ = std::fs::create_dir_all(&ask_dir);
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Ok(rd) = std::fs::read_dir(&ask_dir) else {
                    continue;
                };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(v) = std::fs::read_to_string(&p)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .ok_or(())
                    {
                        let q = v
                            .get("question")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let sid = v
                            .get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let _ = agent2.hook_ask_user(&q, &sid).await;
                    }
                    let _ = std::fs::rename(&p, p.with_extension("done"));
                }
            }
        });
    }
    // Idle keep-alive heartbeat (turn_running=false lets the relay restore auto-stop).
    {
        let agent2 = agent.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                // Running turns: refresh activity with auto-stop off; idle: let the relay restore auto-stop.
                let _ = agent2.keepalive(agent2.any_turn_running()).await;
            }
        });
    }

    // Shutdown: SIGTERM → persist docs + checkpoints, stop children.
    let sync_term = sync.clone();
    let agent_term = agent.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sigterm");
        term.recv().await;
        tracing::info!("SIGTERM: stopping children and persisting docs");
        let _ = tokio::time::timeout(Duration::from_secs(20), agent_term.shutdown()).await;
        sync_term.persist_all().await;
        std::process::exit(0);
    });

    // Command loop. Each command runs on its own task: `send`/`answer` still serialize on the
    // agent's turn lock (FIFO), while `stop` and settings changes land at once, mid-turn.
    while let Some(inc) = rx.recv().await {
        match inc {
            Incoming::Command { chat_id, cmd } => {
                let agent = agent.clone();
                tokio::spawn(async move {
                    agent.handle_command(&chat_id, cmd).await;
                });
            }
            Incoming::CaughtUp { chat_id, head_seq } => {
                tracing::info!(chat_id, head_seq, "room caught up");
            }
            Incoming::Status { chat_id, state } => {
                tracing::debug!(chat_id, ?state, "relay status");
            }
            Incoming::ProjectDocChanged => {
                let agent = agent.clone();
                tokio::spawn(async move {
                    agent.settings_from_doc().await;
                });
            }
        }
    }
    Ok(())
}
