//! Shared relay state: db, config, per-room broadcast channels, box providers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{broadcast, Mutex, RwLock};

use arbos_core::wire::BoxState;

use crate::daytona::Daytona;
use crate::db::Db;

#[derive(Clone)]
pub struct Config {
    pub public_url: String,
    pub data_dir: PathBuf,
    pub admin_token: Option<String>,
    pub openrouter_key: Option<String>,
    pub anthropic_key: Option<String>,
    pub openai_key: Option<String>,
    pub openai_base: String,
    /// Upstream for `/proxy/v1/images/generations` (OpenAI's own API; OpenRouter has no images endpoint).
    pub openai_images_base: String,
    pub anthropic_base: String,
    pub daytona_snapshot: String,
    /// Inline Dockerfile for Daytona's declarative builder when the snapshot is not registered.
    pub daytona_dockerfile: Option<PathBuf>,
    /// Directory served at `/bootstrap/{file}` to boxes (arbos-agentd binary, arbos-pi-ext.tar).
    pub bootstrap_dir: Option<PathBuf>,
    pub agentd_bin: PathBuf,
    pub local_box_image: String,
    pub box_idle_grace_ms: i64,
    /// `ARBOS_CONNECTORS_MOCK=1`: connectors answer with canned data (F1).
    pub connectors_mock: bool,
    /// `ARBOS_CHANNELS_MOCK=1`: channel sends land in `channel_outbox`; `/channels/mock/inbound` is open (F7).
    pub channels_mock: bool,
    /// `ARBOS_IMAGE_MOCK=1`: image generation answers with a labelled placeholder PNG (F10).
    pub image_mock: bool,
}

/// One frame to fan out to the other sockets in a room.
#[derive(Clone, Debug)]
pub struct RoomBroadcast {
    pub from_conn: u64,
    pub bytes: Bytes,
}

pub struct Room {
    pub tx: broadcast::Sender<RoomBroadcast>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub daytona: Option<Daytona>,
    pub rooms: Arc<RwLock<HashMap<String, Arc<Room>>>>,
    /// user_id → live state of the user's box as the relay believes it (STATUS frames, crdt-sync §8).
    pub box_states: Arc<RwLock<HashMap<String, (BoxState, i64)>>>,
    /// user_id → local child process (process provider only).
    pub local_children: Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    /// user_id → (open box sockets, last disconnect ms).
    pub box_conns: Arc<Mutex<HashMap<String, (u32, i64)>>>,
    /// user_id → ms when the current wake started (for wake timing).
    pub wake_started: Arc<Mutex<HashMap<String, i64>>>,
    /// user_id → ms of the last client row; rows pushed while a box was dying must re-wake it.
    pub last_client_push: Arc<Mutex<HashMap<String, i64>>>,
    /// user_id → wakes that produced no hello within the deadline (stale-box detector).
    pub stale_bounces: Arc<Mutex<HashMap<String, u32>>>,
    pub conn_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Encrypts connector tokens at rest (F1).
    pub relay_key: Arc<crate::connectors::crypto::RelayKey>,
    /// device_id → live `arbos-local` helper socket (F9).
    pub local_conns: crate::local::LocalConns,
}

impl AppState {
    pub async fn room(&self, name: &str) -> Arc<Room> {
        if let Some(r) = self.rooms.read().await.get(name) {
            return r.clone();
        }
        let mut w = self.rooms.write().await;
        w.entry(name.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1024);
                Arc::new(Room { tx })
            })
            .clone()
    }

    /// The user whose box serves a project.
    pub fn user_of(&self, project_id: &str) -> Option<String> {
        self.db
            .project(project_id)
            .ok()
            .flatten()
            .map(|p| p.user_id)
    }

    /// May this principal touch this project (by id or name)? Unknown projects: no.
    pub fn allows(&self, principal: &crate::auth::Principal, project_id: &str) -> bool {
        match self.user_of(project_id) {
            Some(u) => principal.can_access_project(&u),
            None => false,
        }
    }

    pub async fn box_state(&self, user_id: &str) -> (BoxState, i64) {
        self.box_states
            .read()
            .await
            .get(user_id)
            .copied()
            .unwrap_or((BoxState::Asleep, 0))
    }

    /// Box state seen from one project (its user's box).
    pub async fn project_box_state(&self, project_id: &str) -> (BoxState, i64) {
        match self.user_of(project_id) {
            Some(u) => self.box_state(&u).await,
            None => (BoxState::Asleep, 0),
        }
    }

    pub async fn set_box_state(&self, user_id: &str, st: BoxState, detail: Option<&str>) {
        let now = arbos_core::ids::now_ms();
        let changed = {
            let mut w = self.box_states.write().await;
            let prev = w.get(user_id).map(|x| x.0);
            w.insert(user_id.to_owned(), (st, now));
            prev != Some(st)
        };
        let arbos = match st {
            BoxState::Awake => "awake",
            BoxState::Waking => "waking",
            BoxState::Asleep => "hibernated_light",
            BoxState::Error => "error",
        };
        // The project state column mirrors the user's box for every project of that user.
        for p in self.db.projects_of_user(user_id).unwrap_or_default() {
            let _ = self.db.set_project_state(&p.id, arbos);
            if st != BoxState::Awake {
                let _ = self.db.set_workspace_state(&p.id, "stopped");
            }
        }
        if changed {
            self.broadcast_status(user_id, st, detail).await;
        }
    }

    /// `STATUS` frame to every room of every project of the user (crdt-sync §7.4, 0x0c).
    pub async fn broadcast_status(&self, user_id: &str, st: BoxState, detail: Option<&str>) {
        let frame = arbos_core::wire::Frame::new(
            arbos_core::wire::FrameType::Status,
            serde_json::to_value(arbos_core::wire::StatusHeader {
                box_state: st,
                since: arbos_core::ids::now_ms(),
                detail: detail.map(str::to_owned),
            })
            .expect("status header"),
            Bytes::new(),
        )
        .encode();
        let prefixes: Vec<String> = self
            .db
            .projects_of_user(user_id)
            .unwrap_or_default()
            .into_iter()
            .map(|p| format!("{}/", p.id))
            .collect();
        let rooms = self.rooms.read().await;
        for (name, room) in rooms.iter() {
            if prefixes.iter().any(|p| name.starts_with(p)) {
                let _ = room.tx.send(RoomBroadcast {
                    from_conn: 0,
                    bytes: frame.clone(),
                });
            }
        }
    }
}
