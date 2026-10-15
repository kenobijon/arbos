//! `arbos-local`: the helper on the user's computer (design §10 "user-computer access", F9).
//! It pairs like any device (D35), keeps one WebSocket to the relay (`WS /local/{deviceId}`),
//! announces hostname/os/home, and performs `run` / `read_file` / `list_dir` requests the
//! relay forwards from a project box. The agent only reaches it after the user approved the
//! exact command or path in the chat (agentd gates every call).

mod config;
mod ops;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

use arbos_core::local::{LocalClientFrame, LocalHello, LocalServerFrame};

use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "arbos-local",
    version,
    about = "Arbos helper: lets your project agents run approved commands and read files on this computer"
)]
struct Args {
    /// Relay base URL (http:// or https://). Saved on `pair`.
    #[arg(long, env = "ARBOS_RELAY_URL")]
    relay: Option<String>,
    /// Config file (default ~/.config/arbos/arbos-local.json).
    #[arg(long, env = "ARBOS_LOCAL_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Exchange a one-time pairing code for a device token (D35).
    Pair {
        code: String,
        /// Name shown in Arbos → Settings → This Mac (default: this machine's hostname).
        #[arg(long)]
        name: Option<String>,
    },
    /// Connect to the relay and serve requests until stopped.
    Run,
    /// Print the paired device id and relay.
    Status,
}

fn hostname() -> String {
    std::env::var("ARBOS_LOCAL_HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "computer".into())
}

fn hello() -> LocalHello {
    LocalHello {
        hostname: hostname(),
        os: std::env::consts::OS.to_owned(),
        home: std::env::var("HOME").unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let mut cfg = Config::load(args.config.clone())?;
    if let Some(r) = &args.relay {
        cfg.relay_url = r.trim_end_matches('/').to_owned();
    }
    match args.cmd {
        Cmd::Pair { code, name } => pair(&mut cfg, &code, name).await,
        Cmd::Status => {
            if cfg.device_id.is_empty() {
                println!("not paired (relay {})", cfg.relay_url);
            } else {
                println!("device {} · relay {}", cfg.device_id, cfg.relay_url);
            }
            Ok(())
        }
        Cmd::Run => run(cfg).await,
    }
}

async fn pair(cfg: &mut Config, code: &str, name: Option<String>) -> Result<()> {
    let name = name.unwrap_or_else(hostname);
    let resp = reqwest::Client::new()
        .post(format!("{}/pair", cfg.relay_url))
        .json(&json!({ "code": code.trim(), "name": name }))
        .send()
        .await
        .with_context(|| format!("POST {}/pair", cfg.relay_url))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("relay HTTP {status}: {text}"));
    }
    let v: Value = serde_json::from_str(&text).context("pair reply")?;
    cfg.device_token = v["device_token"].as_str().unwrap_or("").to_owned();
    cfg.device_id = v["device_id"].as_str().unwrap_or("").to_owned();
    if cfg.device_token.is_empty() || cfg.device_id.is_empty() {
        return Err(anyhow!("relay returned no device token"));
    }
    cfg.save()?;
    println!(
        "paired as device {} ({}); config {}\nnext: arbos-local run",
        cfg.device_id,
        name,
        cfg.path.display()
    );
    Ok(())
}

/// Reconnect forever with capped exponential backoff; one successful session resets it.
async fn run(cfg: Config) -> Result<()> {
    cfg.require_paired()?;
    let cfg = Arc::new(cfg);
    let http = reqwest::Client::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = std::time::Instant::now();
        match session(cfg.clone(), http.clone()).await {
            Ok(()) => tracing::info!("relay closed the connection"),
            Err(e) => tracing::warn!(error = %e, "session ended"),
        }
        if started.elapsed() > Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        tracing::info!(seconds = backoff.as_secs(), "reconnecting");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn session(cfg: Arc<Config>, http: reqwest::Client) -> Result<()> {
    let url = cfg.ws_url();
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("dial {}/local/{}", cfg.relay_url, cfg.device_id))?;
    let (sink, mut stream) = ws.split();
    let sink = Arc::new(Mutex::new(sink));
    let h = hello();
    sink.lock()
        .await
        .send(Message::Text(
            serde_json::to_string(&LocalClientFrame::Hello(h.clone()))?.into(),
        ))
        .await?;
    tracing::info!(host = %h.hostname, os = %h.os, device = %cfg.device_id, "connected to relay");

    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Ping(p) => {
                sink.lock().await.send(Message::Pong(p)).await?;
                continue;
            }
            Message::Close(_) => break,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };
        let frame: LocalServerFrame = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "bad frame from relay");
                continue;
            }
        };
        match frame {
            LocalServerFrame::Ping => {
                sink.lock()
                    .await
                    .send(Message::Text(
                        serde_json::to_string(&LocalClientFrame::Pong)?.into(),
                    ))
                    .await?;
            }
            LocalServerFrame::Exec(req) => {
                // Each request runs on its own task so a slow command never blocks pings.
                let sink = sink.clone();
                let cfg = cfg.clone();
                let http = http.clone();
                tokio::spawn(async move {
                    tracing::info!(id = %req.id, op = req.op.name(), subject = %req.op.subject(), "exec");
                    let result = ops::execute(
                        &req.op,
                        ops::BlobSink {
                            http: &http,
                            relay_url: &cfg.relay_url,
                            token: &cfg.device_token,
                            project_id: &req.project_id,
                        },
                    )
                    .await;
                    let frame = LocalClientFrame::Result { id: req.id, result };
                    if let Ok(s) = serde_json::to_string(&frame) {
                        let _ = sink.lock().await.send(Message::Text(s.into())).await;
                    }
                });
            }
        }
    }
    Ok(())
}
