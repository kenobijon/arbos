//! Plain Rust view of the Project doc and the Chat docs (`arbos-design.md` §5,
//! `crdt-sync.md` §6). No GPUI, no Loro: the sync layer fills these in, the
//! views only read them.

use serde::{Deserialize, Serialize};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxState {
    Awake,
    Waking,
    HibernatedLight,
    HibernatedDeep,
    StoppedByUser,
    Error,
}

impl BoxState {
    pub fn is_asleep(self) -> bool {
        match self {
            BoxState::HibernatedLight | BoxState::HibernatedDeep | BoxState::StoppedByUser => true,
            BoxState::Awake | BoxState::Waking | BoxState::Error => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    /// One glyph, drawn in the rail tile and the pinned tab.
    pub icon: String,
    /// Hex `#rrggbb` accent for the selected tile border.
    pub color: String,
    pub state: BoxState,
    /// Human text for the S12/S13 banner, e.g. "Restoring from archive…".
    pub state_detail: Option<String>,
    /// 0.0–1.0 while `state == Waking`.
    pub wake_progress: Option<f32>,
    pub model: String,
    pub effort: Effort,
    pub spend_today_usd: f64,
    pub spend_cap_usd: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub const ALL: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub id: Id,
    pub project_id: Id,
    pub title: String,
    pub is_pinned: bool,
    /// S15 system tab "Projects ⇄".
    pub is_system: bool,
    pub unread: bool,
    pub last_message_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Queued,
    Streaming,
    Done,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    Reply,
    Act,
    Spawn,
    Wake,
    #[serde(other)]
    Other,
}

/// `turn` is written by the box as `{ "kind": "reply" }` and by older
/// fixtures as a bare string; accept both.
fn de_turn<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<TurnKind>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => {
            serde_json::from_value(serde_json::Value::String(s)).ok()
        }
        Some(serde_json::Value::Object(map)) => map
            .get("kind")
            .cloned()
            .and_then(|k| serde_json::from_value(k).ok()),
        Some(_) => None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// `call` object of a tool part, as `arbos_core::doc::ChatDoc::add_tool_part` writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

impl ToolCall {
    /// `input_line` cut to ~60 chars for dense lists.
    pub fn call_input_short(&self) -> String {
        let line = self.input_line();
        let mut out: String = line.chars().take(60).collect();
        if line.chars().count() > 60 {
            out.push('…');
        }
        out
    }

    /// One-line input for the card.
    pub fn input_line(&self) -> String {
        match &self.input {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" "),
            other => other.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    #[serde(rename = "ref", default)]
    pub blob_ref: Option<String>,
}

/// One row of `messages[].parts` (`crdt-sync.md` §6.2). Unknown kinds decode
/// to [`Part::Unknown`] and render as nothing, per the "additive only" rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Part {
    Text {
        id: Id,
        #[serde(default)]
        text: String,
    },
    Reasoning {
        id: Id,
        #[serde(default)]
        reasoning: String,
    },
    Tool {
        id: Id,
        call: ToolCall,
        /// ≤ 160 chars (`crdt-sync.md` §6.2).
        #[serde(default)]
        output_summary: String,
        #[serde(default)]
        output_ref: Option<String>,
        #[serde(default)]
        is_error: bool,
        /// Client-visible progress (S17). `None` means finished.
        #[serde(default)]
        status: Option<ToolStatus>,
        #[serde(default)]
        duration_ms: Option<u64>,
        /// Set when this call spawned a subagent; such a call never folds (S7).
        #[serde(default)]
        subagent_name: Option<String>,
    },
    Attachment {
        id: Id,
        attachment: Attachment,
    },
    Error {
        id: Id,
        code: String,
        message: String,
    },
    /// S4 "Arbos is deciding…", S6 "Working in sandbox…", S8 question, S12 wake note.
    Status {
        id: Id,
        code: StatusKind,
        #[serde(default)]
        message: String,
    },
    /// Collapsed diff card.
    Diff {
        id: Id,
        path: String,
        added: u32,
        removed: u32,
        blob_ref: Option<String>,
    },
    /// Collapsed file card.
    File {
        id: Id,
        path: String,
        lines: u32,
        blob_ref: Option<String>,
    },
    /// S16 browser card.
    Browser {
        id: Id,
        title: String,
        url: String,
        last_action: String,
        live: bool,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Deciding,
    SubagentWorking,
    /// Box: "{name} · {status} · {secs}s" when a subagent finishes.
    SubagentDone,
    /// Box: "{tool} {one-line input}" when an act tool starts (S17).
    ToolStart,
    #[serde(alias = "waiting_user")]
    AskUser,
    #[serde(alias = "handed_off")]
    Handoff,
    Note,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: Id,
    #[serde(default)]
    pub chat_id: Id,
    pub role: Role,
    #[serde(default)]
    pub sender: String,
    pub status: MessageStatus,
    #[serde(default, deserialize_with = "de_turn")]
    pub turn: Option<TurnKind>,
    /// Routing outcome on the assistant message: reply | act | spawn | wake.
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub subagent_id: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// Token usage as the box reports it (shape varies by harness).
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
    #[serde(default)]
    pub return_contract: Option<serde_json::Value>,
    #[serde(default)]
    pub waiting_user: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub completed_at: Option<u64>,
    /// True until the relay `ACK`s the client's push (S3 faint echo). Client-only.
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub parts: Vec<Part>,
}

impl Message {
    pub fn is_streaming(&self) -> bool {
        self.status == MessageStatus::Streaming
    }

    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            if let Part::Text { text, .. } = part {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn text_len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Text { text, .. } => text.len(),
                _ => 0,
            })
            .sum()
    }
}

/// One entry of the project doc `threads` map (T1 index, `crdt-sync.md` §6.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub last_turn_at: Option<i64>,
}

/// The parts of the project doc the client renders.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectDocView {
    pub threads: Vec<ThreadInfo>,
    pub status: serde_json::Value,
    pub settings: serde_json::Value,
    pub memory_t1: String,
}

/// Connection to the relay, S14.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,
    Reconnecting,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id() -> Id {
    ulid::Ulid::new().to_string()
}
