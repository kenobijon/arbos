//! App-only room sync session (crdt-sync §7–§8.6, §11).
//!
//! [`RoomSession`] is sans-IO: no tokio, tungstenite, or HTTP. The Mac (and
//! later UniFFI/Swift) drives it from an I/O loop. Shared wire, ids, and the
//! Loro chat-doc schema stay in `arbos-core`. Only device clients depend on
//! this crate — not the relay, agentd, `arbosctl`, or `arbos-local`.

pub mod room;

pub use room::{
    room_path, RoomAction, RoomSession, BACKOFF_MAX, BACKOFF_MIN, BACKOFF_RESET_AFTER,
    HELLO_DEADLINE, PRESENCE_EVERY,
};
