//! Shared Arbos types. No I/O, no GPUI, no tokio: this crate must build on
//! every target the clients and the box use (design §7.2).

pub mod approval;
pub mod auth;
pub mod doc;
pub mod ids;
pub mod local;
pub mod route;
pub mod rpc;
pub mod skill;
pub mod widget;
pub mod wire;

/// Protocol version carried in `HELLO` (crdt-sync §7.2) and in the agentd ↔ relay handshake (D44).
pub const PROTO_VERSION: u32 = 1;

/// Streaming commit cadence for LoroText appends (design §4.3).
pub const STREAM_COMMIT_MS: u64 = 120;

/// Inline cap for tool output carried in the doc; the rest goes by `outputRef` (crdt-sync §6.2).
pub const TOOL_OUTPUT_SUMMARY_MAX: usize = 160;

/// Default main-turn wall-clock deadline in seconds. Override at startup with `ARBOS_TURN_DEADLINE_SECS`.
pub const TURN_DEADLINE_SECS: u64 = 900;

/// Offload threshold for the main agent's tool results (design §4.4.1).
pub const OFFLOAD_CHARS: usize = 4_000;
