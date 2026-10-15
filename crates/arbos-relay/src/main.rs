mod api;
mod auth;
mod blobs;
mod boxes;
mod browser;
mod daytona;
mod db;
mod proxy;
mod rooms;
mod smoke;
mod state;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;
use clap::{Parser, Subcommand};
use tokio::sync::{Mutex, RwLock};

use crate::state::{AppState, Config};

#[derive(Parser, Debug)]
#[command(
    name = "arbos-relay",
    version,
    about = "Arbos relay: registry, pairing, sync rooms, blobs, model proxy, Daytona"
)]
struct Args {
    /// Listen address.
    #[arg(long, env = "ARBOS_RELAY_LISTEN", default_value = "127.0.0.1:7700")]
    listen: String,
    /// URL boxes and clients use to reach this relay.
    #[arg(long, env = "ARBOS_RELAY_PUBLIC_URL")]
    public_url: Option<String>,
    /// Data directory (SQLite + blobs + local boxes).
    #[arg(long, env = "ARBOS_RELAY_DATA", default_value = "./.arbos-local/relay")]
    data: PathBuf,
    /// Admin token for `POST /pair/new`; generated and printed when absent.
    #[arg(long, env = "ARBOS_RELAY_ADMIN_TOKEN")]
    admin_token: Option<String>,
    /// Daytona snapshot name for new boxes.
    #[arg(
        long,
        env = "ARBOS_DAYTONA_SNAPSHOT",
        default_value = "arbos-agent-0.1.4"
    )]
    daytona_snapshot: String,
    /// Inline Dockerfile (e.g. image/Dockerfile.dev-inline) built by Daytona when the snapshot does not exist.
    #[arg(long, env = "ARBOS_DAYTONA_DOCKERFILE")]
    daytona_dockerfile: Option<PathBuf>,
    /// Directory served to boxes at /bootstrap/{file} (arbos-agentd, arbos-pi-ext.tar).
    #[arg(long, env = "ARBOS_BOOTSTRAP_DIR")]
    bootstrap_dir: Option<PathBuf>,
    /// Path to the arbos-agentd binary for `process` boxes.
    #[arg(long, env = "ARBOS_AGENTD_BIN", default_value = "arbos-agentd")]
    agentd_bin: PathBuf,
    /// Docker image for `docker` boxes.
    #[arg(long, env = "ARBOS_LOCAL_BOX_IMAGE", default_value = "arbos-agent:dev")]
    local_box_image: String,
    /// OpenAI-compatible upstream (OpenRouter by default).
    #[arg(
        long,
        env = "ARBOS_OPENAI_BASE",
        default_value = "https://openrouter.ai/api/v1"
    )]
    openai_base: String,
    /// Anthropic-compatible upstream; OpenRouter's `/api/v1` when only an OpenRouter key exists.
    #[arg(long, env = "ARBOS_ANTHROPIC_BASE")]
    anthropic_base: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the server (default).
    Serve,
    /// Print a one-time pairing code (writes to the same SQLite file).
    PairCode,
    /// D25 lifecycle check against the live Daytona API with timings (create → stop → start → archive → start → delete).
    DaytonaSmoke {
        /// Snapshot name; when absent, an inline Dockerfile (node:24 + pi) is built by Daytona.
        #[arg(long)]
        snapshot: Option<String>,
        /// Keep the sandbox instead of deleting it.
        #[arg(long)]
        keep: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,loro_internal=warn,arbos_relay=debug".into()),
        )
        .init();
    let args = Args::parse();
    let db = db::Db::open(&args.data.join("relay.db"))?;

    if let Some(Cmd::PairCode) = args.cmd {
        let code = arbos_core::auth::pairing_code();
        db.new_pairing_code(&code, 10 * 60 * 1000)?;
        println!("{code}");
        return Ok(());
    }

    let openrouter_key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let openai_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let daytona_key = std::env::var("DAYTONA_API_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    let anthropic_base = args.anthropic_base.clone().unwrap_or_else(|| {
        if anthropic_key.is_some() {
            "https://api.anthropic.com".to_owned()
        } else {
            "https://openrouter.ai/api".to_owned()
        }
    });
    let admin_token = args.admin_token.clone().or_else(|| {
        let t = arbos_core::auth::mint(arbos_core::auth::TokenKind::Device).replace("adv_", "adm_");
        tracing::info!("no ARBOS_RELAY_ADMIN_TOKEN set; generated admin token: {t}");
        Some(t)
    });
    let public_url = args
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.listen));

    let cfg = Config {
        public_url,
        data_dir: args.data.clone(),
        admin_token,
        openrouter_key,
        anthropic_key,
        openai_key,
        openai_base: args.openai_base.clone(),
        anthropic_base,
        daytona_snapshot: args.daytona_snapshot.clone(),
        daytona_dockerfile: args.daytona_dockerfile.clone(),
        bootstrap_dir: args.bootstrap_dir.clone(),
        agentd_bin: args.agentd_bin.clone(),
        local_box_image: args.local_box_image.clone(),
        box_idle_grace_ms: 30_000,
    };
    let daytona = daytona_key.map(|k| {
        daytona::Daytona::new(
            k,
            std::env::var("DAYTONA_API_URL").ok(),
            std::env::var("DAYTONA_ORG_ID").ok(),
        )
    });
    if let Some(Cmd::DaytonaSmoke { snapshot, keep }) = args.cmd {
        let dt = daytona.ok_or_else(|| anyhow::anyhow!("DAYTONA_API_KEY is required"))?;
        return smoke::run(&dt, snapshot, keep).await;
    }
    let state = AppState {
        db: db.clone(),
        cfg: Arc::new(cfg),
        http: reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(300))
            .build()?,
        daytona,
        rooms: Arc::new(RwLock::new(HashMap::new())),
        box_states: Arc::new(RwLock::new(HashMap::new())),
        local_children: Arc::new(Mutex::new(HashMap::new())),
        box_conns: Arc::new(Mutex::new(HashMap::new())),
        wake_started: Arc::new(Mutex::new(HashMap::new())),
        last_client_push: Arc::new(Mutex::new(HashMap::new())),
        stale_bounces: Arc::new(Mutex::new(HashMap::new())),
        conn_seq: Arc::new(AtomicU64::new(0)),
    };

    if db.device_count()? == 0 {
        let code = arbos_core::auth::pairing_code();
        db.new_pairing_code(&code, 60 * 60 * 1000)?;
        tracing::info!("no devices paired yet; one-time pairing code (1 h): {code}");
    }

    rooms::spawn_sweeper(state.clone()).await;
    boxes::spawn_wake_runner(state.clone()).await;

    let app = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/pair/new", post(api::pair_new))
        .route("/pair", post(api::pair))
        .route("/devices", get(api::list_devices))
        .route("/devices/{id}", delete(api::revoke_device))
        .route(
            "/projects",
            get(api::list_projects).post(api::create_project),
        )
        .route(
            "/projects/{id}",
            get(api::get_project).patch(api::patch_project),
        )
        .route(
            "/projects/{id}/chats/{chat}",
            axum::routing::patch(api::patch_chat).delete(api::delete_chat),
        )
        .route(
            "/projects/{id}/browser/stream",
            get(browser::browser_stream),
        )
        .route("/projects/{id}/history/{chat}", get(api::chat_history))
        .route("/projects/{id}/status", get(api::project_status))
        .route(
            "/projects/{id}/chats",
            get(api::list_chats).post(api::create_chat),
        )
        .route("/projects/{id}/wake", post(api::wake))
        .route("/projects/{id}/start", post(api::start))
        .route("/projects/{id}/stop", post(api::stop))
        .route("/projects/{id}/keepalive", post(api::keepalive))
        .route("/projects/{id}/box/hello", post(api::box_hello))
        .route("/projects/{id}/push", post(api::push))
        .route("/projects/{id}/pushes", get(api::take_pushes))
        .route("/projects/{id}/usage", get(api::usage))
        .route("/projects/{id}/timings", post(api::record_timing))
        .route("/room/{project}/{chat}", get(rooms::ws_room))
        .route(
            "/room/{project}/{chat}/checkpoint",
            get(rooms::get_checkpoint).post(rooms::post_checkpoint),
        )
        .route("/room/{project}/{chat}/stats", get(rooms::room_stats))
        .route("/blob/{project}/{key}", get(blobs::get).put(blobs::put))
        .route("/bootstrap/{file}", get(blobs::bootstrap))
        .route("/proxy/v1/models", get(proxy::openai_models))
        .route("/proxy/v1/{*tail}", post(proxy::openai))
        .route("/proxy/anthropic/{*tail}", post(proxy::anthropic))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!(listen = %args.listen, data = %args.data.display(), "arbos-relay listening");
    axum::serve(listener, app).await?;
    Ok(())
}
