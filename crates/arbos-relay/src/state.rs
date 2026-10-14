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
    pub anthropic_base: String,
    pub daytona_snapshot: String,
    /// Inline Dockerfile for Daytona's declarative builder when the snapshot is not registered.
    pub daytona_dockerfile: Option<PathBuf>,
    /// Directory served at `/bootstrap/{file}` to boxes (arbos-agentd binary, arbos-pi-ext.tar).
    pub bootstrap_dir: Option<PathBuf>,
    pub agentd_bin: PathBuf,
    pub local_box_image: String,
    pub box_idle_grace_ms: i64,
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
    /// project_id → live box state as the relay believes it (STATUS frames, crdt-sync §8).
    pub box_states: Arc<RwLock<HashMap<String, (BoxState, i64)>>>,
    /// project_id → local child process (process provider only).
    pub local_children: Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    /// project_id → (open box sockets, last disconnect ms).
    pub box_conns: Arc<Mutex<HashMap<String, (u32, i64)>>>,
    /// project_id → ms when the current wake started (for wake timing).
    pub wake_started: Arc<Mutex<HashMap<String, i64>>>,
    pub conn_seq: Arc<std::sync::atomic::AtomicU64>,
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

    pub async fn box_state(&self, project_id: &str) -> (BoxState, i64) {
        self.box_states
            .read()
            .await
            .get(project_id)
            .copied()
            .unwrap_or((BoxState::Asleep, 0))
    }

    pub async fn set_box_state(&self, project_id: &str, st: BoxState, detail: Option<&str>) {
        let now = arbos_core::ids::now_ms();
        let changed = {
            let mut w = self.box_states.write().await;
            let prev = w.get(project_id).map(|x| x.0);
            w.insert(project_id.to_owned(), (st, now));
            prev != Some(st)
        };
        let arbos = match st {
            BoxState::Awake => "awake",
            BoxState::Waking => "waking",
            BoxState::Asleep => "hibernated_light",
            BoxState::Error => "error",
        };
        let _ = self.db.set_project_state(project_id, arbos);
        if changed {
            self.broadcast_status(project_id, st, detail).await;
        }
    }

    /// `STATUS` frame to every room of the project (crdt-sync §7.4, 0x0c).
    pub async fn broadcast_status(&self, project_id: &str, st: BoxState, detail: Option<&str>) {
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
        let prefix = format!("{project_id}/");
        let rooms = self.rooms.read().await;
        for (name, room) in rooms.iter() {
            if name.starts_with(&prefix) {
                let _ = room.tx.send(RoomBroadcast {
                    from_conn: 0,
                    bytes: frame.clone(),
                });
            }
        }
    }
}
