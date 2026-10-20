//! Sans-IO room sync session (crdt-sync §7–§8.6, §11).
//!
//! A client opens a socket, feeds bytes in, and sends bytes out. Handshake,
//! catch-up, push/ack, resend, and reconnect backoff live here — no tokio or
//! tungstenite. The Mac (and later UniFFI/Swift) drives [`RoomSession`] from
//! its I/O loop and performs the queued [`RoomAction`]s.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::{json, Value};

use crate::ids::ulid;
use crate::wire::{
    self, Ack, ErrorHeader, Frame, FrameType, Hello, PushHeader, RowHeader, RowsReq, State,
    StatusHeader, WakeHeader, ROW_MAX,
};
use crate::PROTO_VERSION;

/// Wait this long after `on_open` for a `STATE` frame before giving up.
pub const HELLO_DEADLINE: Duration = Duration::from_secs(15);
/// Presence cadence hint for hosts that drive ticks (Mac/iOS ~15 s).
pub const PRESENCE_EVERY: Duration = Duration::from_secs(15);
/// Reconnect backoff floor (§8.6).
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);
/// Reconnect backoff ceiling (§8.6).
pub const BACKOFF_MAX: Duration = Duration::from_secs(16);
/// After a connection that stayed up at least this long, reset backoff to min.
pub const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);

/// Room path for the WebSocket URL: `people/{chat}` or `{project}/{chat}`.
pub fn room_path(project_id: &str, chat_id: &str, is_people: bool) -> String {
    if is_people {
        format!("people/{chat_id}")
    } else {
        wire::room_name(project_id, chat_id)
    }
}

/// What the host should do next. Drain with [`RoomSession::drain_actions`].
#[derive(Debug, Clone)]
pub enum RoomAction {
    /// Write these frame bytes on the socket.
    SendFrame(Vec<u8>),
    /// `GET /room/{room}/checkpoint` then call [`RoomSession::on_checkpoint`]
    /// (or [`RoomSession::on_checkpoint_failed`]).
    FetchCheckpoint {
        room: String,
        frontier: Vec<u8>,
        checkpoint_seq: i64,
    },
    /// Import these Loro bytes into the room doc.
    ApplyBytes(Vec<u8>),
    /// Persist the room cursor (and mark the store dirty).
    Cursor(i64),
    /// Persist the pending PUSH queue.
    Pending(Vec<(String, Vec<u8>)>),
    /// Box lifecycle from `STATE` / `STATUS`.
    /// `poll_machine`: true for `STATUS` (host refreshes `GET /machine`).
    BoxState {
        state: wire::BoxState,
        detail: Option<String>,
        poll_machine: bool,
    },
    /// Catch-up finished (`ROWS_DONE`).
    LinkLive,
    /// Socket died or handshake failed; host should reconnect after `after`.
    ScheduleReconnect { after: Duration },
    /// Hard relay error (not `lagging`); tear down this attempt.
    SessionError { code: String, message: String },
    /// Another device's presence / call / typing (host maps to UI events).
    Presence {
        device: Option<String>,
        handle: Option<String>,
        typing: Option<bool>,
        in_call: Option<String>,
        mine: bool,
    },
    /// Local update exceeded [`ROW_MAX`]; dropped (host may log).
    DroppedOversize { bytes: usize },
    /// A pending PUSH was acknowledged (host may clear UI “unacked” marks).
    Ack { batch_id: String, seq: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Before `on_open`.
    Idle,
    /// HELLO sent; waiting for STATE (or HELLO_DEADLINE).
    WaitState,
    /// STATE seen; waiting for host checkpoint resolution.
    WaitCheckpoint,
    /// Catch-up / live.
    Live,
    /// Closed; host should reconnect.
    Closed,
}

/// Sans-IO state machine for one room connection attempt (and reconnect policy).
pub struct RoomSession {
    device_id: String,
    room: String,
    cursor: i64,
    pending: Vec<(String, Vec<u8>)>,
    phase: Phase,
    actions: VecDeque<RoomAction>,
    backoff: Duration,
    /// Host clock at `on_open` (for HELLO deadline and healthy-session reset).
    opened_at_ms: Option<u64>,
    /// Host clock when the last healthy session began (successful live).
    live_since_ms: Option<u64>,
    /// Pending STATE checkpoint fields while host resolves containment/fetch.
    checkpoint_seq: i64,
    /// Delay chosen by the last [`Self::on_close`] / error close.
    reconnect_after: Option<Duration>,
    /// Last host clock seen (`on_open` / `on_tick` / `on_close`).
    last_now_ms: u64,
}

impl RoomSession {
    pub fn new(
        device_id: impl Into<String>,
        room: impl Into<String>,
        cursor: i64,
        pending: Vec<(String, Vec<u8>)>,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            room: room.into(),
            cursor,
            pending,
            phase: Phase::Idle,
            actions: VecDeque::new(),
            backoff: BACKOFF_MIN,
            opened_at_ms: None,
            live_since_ms: None,
            checkpoint_seq: 0,
            reconnect_after: None,
            last_now_ms: 0,
        }
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    pub fn cursor(&self) -> i64 {
        self.cursor
    }

    pub fn pending(&self) -> &[(String, Vec<u8>)] {
        &self.pending
    }

    pub fn backoff(&self) -> Duration {
        self.backoff
    }

    /// Sleep this long before the next dial (set by the last close).
    pub fn reconnect_after(&self) -> Duration {
        self.reconnect_after.unwrap_or(self.backoff)
    }

    pub fn is_live(&self) -> bool {
        self.phase == Phase::Live
    }

    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    pub fn drain_actions(&mut self) -> Vec<RoomAction> {
        self.actions.drain(..).collect()
    }

    fn push(&mut self, action: RoomAction) {
        self.actions.push_back(action);
    }

    fn encode(ty: FrameType, header: Value, payload: Vec<u8>) -> Vec<u8> {
        Frame::new(ty, header, payload).encode().to_vec()
    }

    /// Socket is up. Sends `HELLO` and starts the STATE deadline.
    pub fn on_open(&mut self, now_ms: u64) {
        self.phase = Phase::WaitState;
        self.opened_at_ms = Some(now_ms);
        self.last_now_ms = now_ms;
        self.live_since_ms = None;
        let hello = Hello {
            cursor: self.cursor,
            device: self.device_id.clone(),
            room: self.room.clone(),
            proto: PROTO_VERSION,
        };
        let header = serde_json::to_value(&hello).expect("hello");
        self.push(RoomAction::SendFrame(Self::encode(
            FrameType::Hello,
            header,
            Vec::new(),
        )));
    }

    /// Inbound WebSocket binary frame.
    pub fn on_frame(&mut self, bytes: &[u8]) {
        if self.phase == Phase::Closed || self.phase == Phase::Idle {
            return;
        }
        let Ok(Some(frame)) = Frame::decode(bytes.to_vec().into()) else {
            return;
        };
        match frame.ty {
            FrameType::State => self.on_state(frame),
            FrameType::Row => self.on_row(frame),
            FrameType::RowsDone => self.on_rows_done(frame),
            FrameType::Ack => self.on_ack(frame),
            FrameType::Status => self.on_status(frame),
            FrameType::Error => self.on_error(frame),
            FrameType::Presence => self.on_presence(frame),
            FrameType::ProbeOk
            | FrameType::Hello
            | FrameType::RowsReq
            | FrameType::Push
            | FrameType::Probe
            | FrameType::Wake => {}
        }
    }

    fn on_state(&mut self, frame: Frame) {
        if self.phase != Phase::WaitState {
            return;
        }
        let Ok(state) = serde_json::from_value::<State>(frame.header) else {
            return;
        };
        self.push(RoomAction::BoxState {
            state: state.box_state,
            detail: None,
            poll_machine: false,
        });
        let frontier = frame.payload.to_vec();
        self.checkpoint_seq = state.checkpoint_seq;
        if state.checkpoint_seq > 0 {
            self.phase = Phase::WaitCheckpoint;
            self.push(RoomAction::FetchCheckpoint {
                room: self.room.clone(),
                frontier,
                checkpoint_seq: state.checkpoint_seq,
            });
        } else {
            self.finish_handshake();
        }
    }

    /// Host: local oplog already includes the STATE frontier — advance cursor
    /// and continue (crdt-sync §7.3).
    pub fn on_checkpoint_skipped(&mut self) {
        if self.phase != Phase::WaitCheckpoint {
            return;
        }
        self.cursor = self.cursor.max(self.checkpoint_seq);
        self.push(RoomAction::Cursor(self.cursor));
        self.finish_handshake();
    }

    /// Host imported checkpoint bytes (or applied them itself). Advances the
    /// cursor and continues catch-up. Does not emit [`RoomAction::ApplyBytes`]
    /// — the host already has the snapshot.
    pub fn on_checkpoint(&mut self, seq: i64) {
        if self.phase != Phase::WaitCheckpoint {
            return;
        }
        self.cursor = self.cursor.max(seq.max(self.checkpoint_seq));
        self.push(RoomAction::Cursor(self.cursor));
        self.finish_handshake();
    }

    /// Host could not fetch/import the checkpoint; continue with RowsReq anyway
    /// (same as today's Mac: log and proceed).
    pub fn on_checkpoint_failed(&mut self) {
        if self.phase != Phase::WaitCheckpoint {
            return;
        }
        self.finish_handshake();
    }

    fn finish_handshake(&mut self) {
        self.phase = Phase::Live;
        // Own rows are requested too: a row pushed by this device but saved
        // before its ACK is only on the relay. Loro drops duplicates.
        let req = RowsReq {
            after: self.cursor,
            exclude_own: false,
        };
        let header = serde_json::to_value(&req).expect("rows_req");
        self.push(RoomAction::SendFrame(Self::encode(
            FrameType::RowsReq,
            header,
            Vec::new(),
        )));
        let pending = self.pending.clone();
        for (batch_id, bytes) in pending {
            let header = serde_json::to_value(PushHeader { batch_id }).expect("push");
            self.push(RoomAction::SendFrame(Self::encode(
                FrameType::Push,
                header,
                bytes,
            )));
        }
    }

    fn on_row(&mut self, frame: Frame) {
        if self.phase != Phase::Live {
            return;
        }
        let Ok(header) = serde_json::from_value::<RowHeader>(frame.header) else {
            return;
        };
        self.push(RoomAction::ApplyBytes(frame.payload.to_vec()));
        self.cursor = self.cursor.max(header.seq);
        self.push(RoomAction::Cursor(self.cursor));
    }

    fn on_rows_done(&mut self, frame: Frame) {
        if self.phase != Phase::Live {
            return;
        }
        let head_seq = frame
            .header
            .get("headSeq")
            .and_then(Value::as_i64)
            .unwrap_or(self.cursor);
        self.cursor = self.cursor.max(head_seq);
        self.push(RoomAction::Cursor(self.cursor));
        self.push(RoomAction::LinkLive);
    }

    fn on_ack(&mut self, frame: Frame) {
        if self.phase != Phase::Live {
            return;
        }
        let Ok(header) = serde_json::from_value::<Ack>(frame.header) else {
            return;
        };
        let before = self.pending.len();
        self.pending.retain(|(id, _)| *id != header.batch_id);
        if self.pending.len() != before {
            self.push(RoomAction::Pending(self.pending.clone()));
        }
        self.cursor = self.cursor.max(header.seq);
        self.push(RoomAction::Cursor(self.cursor));
        self.push(RoomAction::Ack {
            batch_id: header.batch_id,
            seq: header.seq,
        });
    }

    fn on_status(&mut self, frame: Frame) {
        let Ok(header) = serde_json::from_value::<StatusHeader>(frame.header) else {
            return;
        };
        self.push(RoomAction::BoxState {
            state: header.box_state,
            detail: header.detail,
            poll_machine: true,
        });
    }

    fn on_error(&mut self, frame: Frame) {
        let Ok(header) = serde_json::from_value::<ErrorHeader>(frame.header) else {
            return;
        };
        if header.code == "lagging" && self.phase == Phase::Live {
            let req = RowsReq {
                after: self.cursor,
                exclude_own: true,
            };
            let h = serde_json::to_value(&req).expect("rows_req");
            self.push(RoomAction::SendFrame(Self::encode(
                FrameType::RowsReq,
                h,
                Vec::new(),
            )));
        } else {
            self.push(RoomAction::SessionError {
                code: header.code,
                message: header.message,
            });
            self.close_for_reconnect(Some(self.last_now_ms));
        }
    }

    fn on_presence(&mut self, frame: Frame) {
        let h = &frame.header;
        let device = h.get("device").and_then(Value::as_str).map(str::to_string);
        let mine = device.as_deref() == Some(self.device_id.as_str());
        self.push(RoomAction::Presence {
            device,
            handle: h
                .get("handle")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            typing: h.get("typing").and_then(Value::as_bool),
            in_call: h.get("in_call").and_then(Value::as_str).map(str::to_string),
            mine,
        });
    }

    /// Local Loro commit bytes → `PUSH` (or queue if not live yet — host should
    /// only call this while a session is open; pending is resent after catch-up).
    pub fn on_local_commit(&mut self, bytes: Vec<u8>) {
        if bytes.len() > ROW_MAX {
            self.push(RoomAction::DroppedOversize { bytes: bytes.len() });
            return;
        }
        let batch_id = ulid();
        self.pending.push((batch_id.clone(), bytes.clone()));
        self.push(RoomAction::Pending(self.pending.clone()));
        if self.phase == Phase::Live {
            let header = serde_json::to_value(PushHeader { batch_id }).expect("push");
            self.push(RoomAction::SendFrame(Self::encode(
                FrameType::Push,
                header,
                bytes,
            )));
        }
    }

    /// Host clock tick: enforce HELLO deadline; mark live-since on first live tick.
    pub fn on_tick(&mut self, now_ms: u64) {
        self.last_now_ms = now_ms;
        if self.phase == Phase::WaitState {
            if let Some(opened) = self.opened_at_ms {
                if now_ms.saturating_sub(opened) >= HELLO_DEADLINE.as_millis() as u64 {
                    self.push(RoomAction::SessionError {
                        code: "state_timeout".into(),
                        message: "STATE did not arrive".into(),
                    });
                    self.close_for_reconnect(Some(now_ms));
                }
            }
        }
        if self.phase == Phase::Live && self.live_since_ms.is_none() {
            self.live_since_ms = Some(now_ms);
        }
    }

    /// Socket closed or host is abandoning this attempt.
    pub fn on_close(&mut self, now_ms: u64) {
        self.last_now_ms = now_ms;
        self.close_for_reconnect(Some(now_ms));
    }

    fn close_for_reconnect(&mut self, now_ms: Option<u64>) {
        if self.phase == Phase::Closed {
            return;
        }
        // Parity with Mac's old `connected_at.elapsed() > BACKOFF_RESET_AFTER`.
        let healthy = match (self.opened_at_ms, now_ms) {
            (Some(since), Some(now)) => {
                now.saturating_sub(since) > BACKOFF_RESET_AFTER.as_millis() as u64
            }
            _ => false,
        };
        if healthy {
            self.backoff = BACKOFF_MIN;
        }
        let after = self.backoff;
        self.reconnect_after = Some(after);
        self.backoff = (self.backoff.saturating_mul(2)).min(BACKOFF_MAX);
        self.phase = Phase::Closed;
        self.opened_at_ms = None;
        self.live_since_ms = None;
        self.push(RoomAction::ScheduleReconnect { after });
    }

    /// Reset for the next dial after sleeping out [`RoomAction::ScheduleReconnect`].
    pub fn prepare_reconnect(&mut self) {
        self.phase = Phase::Idle;
        self.actions.clear();
        self.checkpoint_seq = 0;
        self.reconnect_after = None;
    }

    pub fn wake(&mut self, reason: impl Into<String>) {
        if self.phase != Phase::Live {
            return;
        }
        let header = serde_json::to_value(WakeHeader {
            reason: reason.into(),
        })
        .expect("wake");
        self.push(RoomAction::SendFrame(Self::encode(
            FrameType::Wake,
            header,
            Vec::new(),
        )));
    }

    pub fn presence(&mut self, header: Value) {
        if self.phase != Phase::Live {
            return;
        }
        self.push(RoomAction::SendFrame(Self::encode(
            FrameType::Presence,
            header,
            Vec::new(),
        )));
    }

    pub fn typing(&mut self, mut presence_header: Value, active: bool) {
        if let Some(obj) = presence_header.as_object_mut() {
            obj.insert("typing".into(), json!(active));
        }
        self.presence(presence_header);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{BoxState, FrameType};

    fn state_frame(checkpoint_seq: i64, frontier: Vec<u8>) -> Vec<u8> {
        let header = json!({
            "headSeq": 10,
            "seqFloor": 0,
            "checkpointSeq": checkpoint_seq,
            "checkpointSize": frontier.len() as i64,
            "rowCount": 0,
            "rowBytes": 0,
            "box": "awake",
        });
        Frame::new(FrameType::State, header, frontier)
            .encode()
            .to_vec()
    }

    fn row_frame(seq: i64, payload: Vec<u8>) -> Vec<u8> {
        let header = json!({
            "seq": seq,
            "device": "dev:other",
            "batchId": "batch-row",
        });
        Frame::new(FrameType::Row, header, payload)
            .encode()
            .to_vec()
    }

    fn rows_done(head: i64) -> Vec<u8> {
        Frame::new(FrameType::RowsDone, json!({ "headSeq": head }), Vec::new())
            .encode()
            .to_vec()
    }

    fn ack_frame(batch_id: &str, seq: i64) -> Vec<u8> {
        Frame::new(
            FrameType::Ack,
            json!({ "batchId": batch_id, "seq": seq }),
            Vec::new(),
        )
        .encode()
        .to_vec()
    }

    fn take_sends(actions: &[RoomAction]) -> Vec<Frame> {
        actions
            .iter()
            .filter_map(|a| match a {
                RoomAction::SendFrame(b) => Frame::decode(b.clone().into()).ok().flatten(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn room_path_people_and_project() {
        assert_eq!(room_path("p1", "c1", false), "p1/c1");
        assert_eq!(room_path("p1", "c1", true), "people/c1");
    }

    #[test]
    fn connect_catch_up_and_push_resend_before_ack() {
        let mut s = RoomSession::new("dev:me", "proj/chat", 0, Vec::new());
        s.on_open(1_000);
        let open = s.drain_actions();
        let sends = take_sends(&open);
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].ty, FrameType::Hello);

        // STATE with no checkpoint → RowsReq immediately.
        s.on_frame(&state_frame(0, Vec::new()));
        let after_state = s.drain_actions();
        assert!(after_state.iter().any(|a| matches!(
            a,
            RoomAction::BoxState {
                state: BoxState::Awake,
                poll_machine: false,
                ..
            }
        )));
        let sends = take_sends(&after_state);
        assert!(sends.iter().any(|f| f.ty == FrameType::RowsReq));
        assert!(s.is_live());

        // Catch-up row + done.
        s.on_frame(&row_frame(1, b"row-bytes".to_vec()));
        let row_actions = s.drain_actions();
        assert!(row_actions
            .iter()
            .any(|a| matches!(a, RoomAction::ApplyBytes(b) if b == b"row-bytes")));
        assert!(row_actions
            .iter()
            .any(|a| matches!(a, RoomAction::Cursor(1))));

        s.on_frame(&rows_done(1));
        assert!(s
            .drain_actions()
            .iter()
            .any(|a| matches!(a, RoomAction::LinkLive)));

        // Local commit → PUSH; disconnect before ACK → pending resent on next open.
        s.on_local_commit(b"local-update".to_vec());
        let push_actions = s.drain_actions();
        let pending = s.pending().to_vec();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, b"local-update");
        let pushes: Vec<_> = take_sends(&push_actions)
            .into_iter()
            .filter(|f| f.ty == FrameType::Push)
            .collect();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].payload.as_ref(), b"local-update");

        s.on_close(2_000);
        let close = s.drain_actions();
        assert!(close.iter().any(|a| matches!(
            a,
            RoomAction::ScheduleReconnect { after } if *after == BACKOFF_MIN
        )));

        // New connection: HELLO, catch-up, then resend pending PUSH.
        s.prepare_reconnect();
        s.on_open(5_000);
        let _ = s.drain_actions();
        s.on_frame(&state_frame(0, Vec::new()));
        let resend = s.drain_actions();
        let pushes: Vec<_> = take_sends(&resend)
            .into_iter()
            .filter(|f| f.ty == FrameType::Push)
            .collect();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].payload.as_ref(), b"local-update");
        let batch_id = pending[0].0.clone();
        assert_eq!(
            pushes[0].header.get("batchId").and_then(Value::as_str),
            Some(batch_id.as_str())
        );

        // ACK clears pending.
        s.on_frame(&ack_frame(&batch_id, 2));
        let ack_actions = s.drain_actions();
        assert!(s.pending().is_empty());
        assert!(ack_actions
            .iter()
            .any(|a| matches!(a, RoomAction::Pending(p) if p.is_empty())));
        assert!(ack_actions
            .iter()
            .any(|a| matches!(a, RoomAction::Ack { seq: 2, .. })));
    }

    #[test]
    fn checkpoint_skipped_when_host_says_contained() {
        let mut s = RoomSession::new("dev:me", "proj/chat", 3, Vec::new());
        s.on_open(1);
        let _ = s.drain_actions();
        s.on_frame(&state_frame(5, b"frontier".to_vec()));
        let actions = s.drain_actions();
        assert!(actions.iter().any(|a| matches!(
            a,
            RoomAction::FetchCheckpoint {
                checkpoint_seq: 5,
                ..
            }
        )));
        s.on_checkpoint_skipped();
        assert_eq!(s.cursor(), 5);
        let cont = s.drain_actions();
        assert!(take_sends(&cont).iter().any(|f| f.ty == FrameType::RowsReq));
    }
}
