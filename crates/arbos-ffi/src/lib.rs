//! UniFFI surface for the Swift client (crdt-sync shared-client step 4b / 5a).
//!
//! This is the only crate that names Swift or `uniffi`. It wraps
//! [`arbos_client::RoomSession`] and the parts of `arbos-core` the Swift app
//! needs. Core and client stay free of UniFFI derives.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use arbos_client::room::{
    self as client_room, RoomAction as ClientAction, RoomSession as ClientSession,
};
use arbos_core::doc::{self, decode_vv, encode_vv, ChatDoc as CoreChatDoc};
use arbos_core::wire::{self, Frame, FrameType as WireFrameType};
use loro::Subscription;
use serde_json::Value;

uniffi::setup_scaffolding!();

/// Frame decode or doc import failure. Sans-IO session has no transport errors.
#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Import(String),
}

/// Pending PUSH batch (owned; replaces `(String, Vec<u8>)` over the FFI).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingPush {
    pub batch_id: String,
    pub bytes: Vec<u8>,
}

/// Mirrors [`arbos_core::wire::BoxState`] for UniFFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BoxState {
    Awake,
    Waking,
    Asleep,
    Error,
}

impl From<wire::BoxState> for BoxState {
    fn from(s: wire::BoxState) -> Self {
        match s {
            wire::BoxState::Awake => Self::Awake,
            wire::BoxState::Waking => Self::Waking,
            wire::BoxState::Asleep => Self::Asleep,
            wire::BoxState::Error => Self::Error,
        }
    }
}

/// Owned host actions from [`RoomSession::drain_actions`].
#[derive(Debug, Clone, uniffi::Enum)]
pub enum RoomAction {
    SendFrame {
        bytes: Vec<u8>,
    },
    FetchCheckpoint {
        room: String,
        frontier: Vec<u8>,
        checkpoint_seq: i64,
    },
    ApplyBytes {
        bytes: Vec<u8>,
    },
    Cursor {
        seq: i64,
    },
    Pending {
        batches: Vec<PendingPush>,
    },
    BoxState {
        state: BoxState,
        detail: Option<String>,
        poll_machine: bool,
    },
    LinkLive,
    ScheduleReconnect {
        after_ms: u64,
    },
    SessionError {
        code: String,
        message: String,
    },
    Presence {
        device: Option<String>,
        handle: Option<String>,
        typing: Option<bool>,
        in_call: Option<String>,
        mine: bool,
    },
    DroppedOversize {
        bytes: u64,
    },
    Ack {
        batch_id: String,
        seq: i64,
    },
}

impl From<ClientAction> for RoomAction {
    fn from(a: ClientAction) -> Self {
        match a {
            ClientAction::SendFrame(bytes) => Self::SendFrame { bytes },
            ClientAction::FetchCheckpoint {
                room,
                frontier,
                checkpoint_seq,
            } => Self::FetchCheckpoint {
                room,
                frontier,
                checkpoint_seq,
            },
            ClientAction::ApplyBytes(bytes) => Self::ApplyBytes { bytes },
            ClientAction::Cursor(seq) => Self::Cursor { seq },
            ClientAction::Pending(batches) => Self::Pending {
                batches: batches
                    .into_iter()
                    .map(|(batch_id, bytes)| PendingPush { batch_id, bytes })
                    .collect(),
            },
            ClientAction::BoxState {
                state,
                detail,
                poll_machine,
            } => Self::BoxState {
                state: state.into(),
                detail,
                poll_machine,
            },
            ClientAction::LinkLive => Self::LinkLive,
            ClientAction::ScheduleReconnect { after } => Self::ScheduleReconnect {
                after_ms: after.as_millis() as u64,
            },
            ClientAction::SessionError { code, message } => Self::SessionError { code, message },
            ClientAction::Presence {
                device,
                handle,
                typing,
                in_call,
                mine,
            } => Self::Presence {
                device,
                handle,
                typing,
                in_call,
                mine,
            },
            ClientAction::DroppedOversize { bytes } => Self::DroppedOversize {
                bytes: bytes as u64,
            },
            ClientAction::Ack { batch_id, seq } => Self::Ack { batch_id, seq },
        }
    }
}

/// Wire frame type for [`decode_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FrameType {
    Hello,
    State,
    RowsReq,
    Row,
    RowsDone,
    Push,
    Ack,
    Presence,
    Probe,
    ProbeOk,
    Error,
    Status,
    Wake,
}

impl From<WireFrameType> for FrameType {
    fn from(t: WireFrameType) -> Self {
        match t {
            WireFrameType::Hello => Self::Hello,
            WireFrameType::State => Self::State,
            WireFrameType::RowsReq => Self::RowsReq,
            WireFrameType::Row => Self::Row,
            WireFrameType::RowsDone => Self::RowsDone,
            WireFrameType::Push => Self::Push,
            WireFrameType::Ack => Self::Ack,
            WireFrameType::Presence => Self::Presence,
            WireFrameType::Probe => Self::Probe,
            WireFrameType::ProbeOk => Self::ProbeOk,
            WireFrameType::Error => Self::Error,
            WireFrameType::Status => Self::Status,
            WireFrameType::Wake => Self::Wake,
        }
    }
}

/// One decoded room frame (header as JSON text).
#[derive(Debug, Clone, uniffi::Record)]
pub struct DecodedFrame {
    pub ty: FrameType,
    pub header_json: String,
    pub payload: Vec<u8>,
}

/// Decode one binary room frame. Unknown type → [`FfiError::Decode`].
#[uniffi::export]
pub fn decode_frame(bytes: Vec<u8>) -> Result<DecodedFrame, FfiError> {
    match Frame::decode(bytes.into()) {
        Ok(Some(frame)) => {
            let header_json = serde_json::to_string(&frame.header)
                .map_err(|e| FfiError::Decode(e.to_string()))?;
            Ok(DecodedFrame {
                ty: frame.ty.into(),
                header_json,
                payload: frame.payload.to_vec(),
            })
        }
        Ok(None) => Err(FfiError::Decode("unknown frame type".into())),
        Err(e) => Err(FfiError::Decode(e.to_string())),
    }
}

/// Sans-IO room session for Swift: `Send + Sync` via interior `Mutex`.
#[derive(uniffi::Object)]
pub struct RoomSession {
    inner: Mutex<ClientSession>,
}

impl RoomSession {
    /// Recover a poisoned lock instead of panicking across the FFI.
    fn lock(&self) -> MutexGuard<'_, ClientSession> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl RoomSession {
    #[uniffi::constructor]
    pub fn new(device_id: String, room: String, cursor: i64, pending: Vec<PendingPush>) -> Self {
        let pending = pending.into_iter().map(|p| (p.batch_id, p.bytes)).collect();
        Self {
            inner: Mutex::new(ClientSession::new(device_id, room, cursor, pending)),
        }
    }

    pub fn room(&self) -> String {
        self.lock().room().to_string()
    }

    pub fn cursor(&self) -> i64 {
        self.lock().cursor()
    }

    pub fn pending(&self) -> Vec<PendingPush> {
        self.lock()
            .pending()
            .iter()
            .map(|(batch_id, bytes)| PendingPush {
                batch_id: batch_id.clone(),
                bytes: bytes.clone(),
            })
            .collect()
    }

    pub fn backoff_ms(&self) -> u64 {
        self.lock().backoff().as_millis() as u64
    }

    pub fn reconnect_after_ms(&self) -> u64 {
        self.lock().reconnect_after().as_millis() as u64
    }

    pub fn is_live(&self) -> bool {
        self.lock().is_live()
    }

    pub fn is_closed(&self) -> bool {
        self.lock().is_closed()
    }

    pub fn drain_actions(&self) -> Vec<RoomAction> {
        self.lock()
            .drain_actions()
            .into_iter()
            .map(RoomAction::from)
            .collect()
    }

    pub fn on_open(&self, now_ms: u64) {
        self.lock().on_open(now_ms);
    }

    pub fn on_frame(&self, bytes: Vec<u8>) {
        self.lock().on_frame(&bytes);
    }

    pub fn on_local_commit(&self, bytes: Vec<u8>) {
        self.lock().on_local_commit(bytes);
    }

    pub fn on_tick(&self, now_ms: u64) {
        self.lock().on_tick(now_ms);
    }

    pub fn on_close(&self, now_ms: u64) {
        self.lock().on_close(now_ms);
    }

    pub fn on_checkpoint(&self, seq: i64) {
        self.lock().on_checkpoint(seq);
    }

    pub fn on_checkpoint_skipped(&self) {
        self.lock().on_checkpoint_skipped();
    }

    pub fn on_checkpoint_failed(&self) {
        self.lock().on_checkpoint_failed();
    }

    pub fn prepare_reconnect(&self) {
        self.lock().prepare_reconnect();
    }

    pub fn wake(&self, reason: String) {
        self.lock().wake(reason);
    }

    /// `presence_header` is a JSON object string (open-ended: typing, in_call, …).
    pub fn presence(&self, presence_header: String) {
        let header = parse_presence_header(&presence_header);
        self.lock().presence(header);
    }

    /// Same header shape as [`Self::presence`]; sets / clears the `typing` field.
    pub fn typing(&self, presence_header: String, active: bool) {
        let header = parse_presence_header(&presence_header);
        self.lock().typing(header, active);
    }
}

fn parse_presence_header(s: &str) -> Value {
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(_) | Err(_) => Value::Object(serde_json::Map::new()),
    }
}

/// Change notification matching Swift `ChatDoc.Touched`.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct Touched {
    pub list_changed: bool,
    pub messages: Vec<u64>,
    pub other: bool,
}

/// Foreign callback: Swift implements this instead of `ChatDoc.onChange`.
///
/// The callback runs on the caller's thread (the thread that called
/// [`ChatDoc::apply`], [`ChatDoc::snapshot`], or
/// [`ChatDoc::export_updates_since`]), after the doc lock is released.
#[uniffi::export(foreign)]
pub trait DocChangeListener: Send + Sync {
    fn on_change(&self, touched: Touched);
}

/// Chat doc wrapper: core `ChatDoc` is not `Sync` (`Cell` txn depth), so `Mutex`.
#[derive(uniffi::Object)]
pub struct ChatDoc {
    inner: Mutex<CoreChatDoc>,
    subs: Mutex<Vec<Subscription>>,
    /// Touched events queued while the doc lock is held; flushed after drop.
    pending: Arc<Mutex<Vec<Touched>>>,
    listener: Arc<Mutex<Option<Arc<dyn DocChangeListener>>>>,
}

impl ChatDoc {
    /// Recover a poisoned doc lock instead of panicking across the FFI.
    fn lock_doc(&self) -> MutexGuard<'_, CoreChatDoc> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_subs(&self) -> MutexGuard<'_, Vec<Subscription>> {
        self.subs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_pending(&self) -> MutexGuard<'_, Vec<Touched>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_listener(&self) -> MutexGuard<'_, Option<Arc<dyn DocChangeListener>>> {
        self.listener.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Deliver queued change notifications after the doc lock is dropped.
    fn flush_pending(&self) {
        let queued = std::mem::take(&mut *self.lock_pending());
        let Some(listener) = self.lock_listener().clone() else {
            return;
        };
        for touched in queued {
            listener.on_change(touched);
        }
    }

    fn new_wrapped(doc: CoreChatDoc) -> Self {
        Self {
            inner: Mutex::new(doc),
            subs: Mutex::new(Vec::new()),
            pending: Arc::new(Mutex::new(Vec::new())),
            listener: Arc::new(Mutex::new(None)),
        }
    }
}

#[uniffi::export]
impl ChatDoc {
    /// Empty replica (random Loro peer, matching Mac/iOS client behaviour).
    #[uniffi::constructor]
    pub fn open() -> Self {
        Self::new_wrapped(CoreChatDoc::new(None, false))
    }

    /// Import a full snapshot into a new replica.
    #[uniffi::constructor]
    pub fn open_from_snapshot(snapshot: Vec<u8>) -> Result<Self, FfiError> {
        let doc = CoreChatDoc::from_snapshot(None, false, &snapshot)
            .map_err(|e| FfiError::Import(e.to_string()))?;
        Ok(Self::new_wrapped(doc))
    }

    /// Import update or snapshot bytes.
    pub fn apply(&self, bytes: Vec<u8>) -> Result<(), FfiError> {
        let result = {
            self.lock_doc()
                .import(&bytes)
                .map(|_| ())
                .map_err(|e| FfiError::Import(e.to_string()))
        };
        self.flush_pending();
        result
    }

    /// Updates since an encoded version vector (`oplog_version` / frontier bytes).
    pub fn export_updates_since(&self, from_vv: Vec<u8>) -> Vec<u8> {
        let vv = decode_vv(&from_vv).unwrap_or_default();
        let out = self.lock_doc().export_updates_since(&vv);
        self.flush_pending();
        out
    }

    pub fn snapshot(&self) -> Vec<u8> {
        let out = self.lock_doc().snapshot();
        self.flush_pending();
        out
    }

    /// Encoded oplog version vector (frontier bytes).
    pub fn oplog_version(&self) -> Vec<u8> {
        let vv = self.lock_doc().oplog_vv();
        encode_vv(&vv)
    }

    pub fn contains_frontier(&self, frontier: Vec<u8>) -> bool {
        self.lock_doc().contains_frontier(&frontier)
    }

    /// Message rows as a JSON array, with `project_hidden_parts` and
    /// `project_reactions` already applied.
    pub fn read_rows_json(&self) -> String {
        let rows = self.lock_doc().projected_rows();
        serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
    }

    /// Subscribe to root diffs; maps through core [`doc::touched`].
    ///
    /// Events are queued while the doc lock is held and delivered on the
    /// caller's thread after [`Self::apply`] / [`Self::snapshot`] /
    /// [`Self::export_updates_since`] release the guard (so a Swift listener
    /// that calls [`Self::read_rows_json`] does not deadlock).
    pub fn on_change(&self, listener: Arc<dyn DocChangeListener>) {
        *self.lock_listener() = Some(listener);
        let pending = self.pending.clone();
        let sub = {
            let doc = self.lock_doc();
            doc.doc.subscribe_root(Arc::new(move |event| {
                let t = doc::touched(&event);
                pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Touched {
                        list_changed: t.list_changed,
                        messages: t.messages.iter().map(|i| *i as u64).collect(),
                        other: t.other,
                    });
            }))
        };
        self.lock_subs().push(sub);
    }
}

/// Room path helper (people vs project), same as `arbos_client::room_path`.
#[uniffi::export]
pub fn room_path(project_id: String, chat_id: String, is_people: bool) -> String {
    client_room::room_path(&project_id, &chat_id, is_people)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state_frame(checkpoint_seq: i64) -> Vec<u8> {
        let header = json!({
            "headSeq": 10,
            "seqFloor": 0,
            "checkpointSeq": checkpoint_seq,
            "checkpointSize": 0,
            "rowCount": 0,
            "rowBytes": 0,
            "box": "awake",
        });
        Frame::new(WireFrameType::State, header, Vec::new())
            .encode()
            .to_vec()
    }

    #[test]
    fn room_session_smoke_on_open_and_state() {
        let s = RoomSession::new("dev:me".into(), "proj/chat".into(), 0, Vec::new());
        s.on_open(1_000);
        let open = s.drain_actions();
        assert!(matches!(open.as_slice(), [RoomAction::SendFrame { .. }]));
        let hello = decode_frame(match &open[0] {
            RoomAction::SendFrame { bytes } => bytes.clone(),
            _ => unreachable!(),
        })
        .unwrap();
        assert_eq!(hello.ty, FrameType::Hello);

        s.on_frame(state_frame(0));
        let after = s.drain_actions();
        assert!(after.iter().any(|a| matches!(
            a,
            RoomAction::BoxState {
                state: BoxState::Awake,
                poll_machine: false,
                ..
            }
        )));
        assert!(s.is_live());
        assert!(after.iter().any(|a| match a {
            RoomAction::SendFrame { bytes } => {
                matches!(decode_frame(bytes.clone()), Ok(f) if f.ty == FrameType::RowsReq)
            }
            _ => false,
        }));
    }

    #[test]
    fn chat_doc_round_trip_snapshot() {
        let doc = ChatDoc::open();
        {
            let guard = doc.lock_doc();
            guard.transact(|d| {
                d.append_user_message(
                    "m1",
                    "hi",
                    "dev",
                    &arbos_core::doc::UserMessageOpts::default(),
                )
                .unwrap();
            });
        }
        let snap = doc.snapshot();
        let loaded = ChatDoc::open_from_snapshot(snap).unwrap();
        let rows: Value = serde_json::from_str(&loaded.read_rows_json()).unwrap();
        assert_eq!(rows[0]["role"], "user");
        assert_eq!(rows[0]["parts"][0]["text"], "hi");
    }

    struct RecordingListener {
        rows: Mutex<Vec<usize>>,
        doc: Arc<ChatDoc>,
    }

    impl DocChangeListener for RecordingListener {
        fn on_change(&self, _touched: Touched) {
            // Re-enter the doc on the same thread; must not deadlock.
            let json = self.doc.read_rows_json();
            let parsed: Value = serde_json::from_str(&json).unwrap();
            self.rows
                .lock()
                .unwrap()
                .push(parsed.as_array().map(|a| a.len()).unwrap_or(0));
        }
    }

    #[test]
    fn change_callback_does_not_deadlock_on_reentrant_read() {
        let doc = Arc::new(ChatDoc::open());
        let listener = Arc::new(RecordingListener {
            rows: Mutex::new(Vec::new()),
            doc: doc.clone(),
        });
        doc.on_change(listener.clone());

        {
            let guard = doc.lock_doc();
            guard.transact(|d| {
                d.append_user_message(
                    "m1",
                    "hi",
                    "dev",
                    &arbos_core::doc::UserMessageOpts::default(),
                )
                .unwrap();
            });
        }
        // snapshot commits and emits; flush runs after the lock drops.
        let _ = doc.snapshot();
        assert!(
            !listener.rows.lock().unwrap().is_empty(),
            "listener should have fired after snapshot"
        );
    }
}
