//! Shared HTTP response shapes for projects and chats (crdt-sync).
//!
//! No I/O. The relay builds these and serializes them; clients will decode the
//! same JSON. Regenerate Swift with `./scripts/gen-swift-types.sh`.
//!
//! typeshare does not accept bare `i64` (JS number precision); fields use
//! `serialized_as = "I54"` so Swift gets `Int64`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use typeshare::typeshare;

use crate::wire::BoxState;

/// Owner card on a project (`project_json` → `owner`), after presence fields.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectOwnerDto {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub handle: String,
    pub avatar: Option<String>,
    pub avatar_color: Option<String>,
    pub avatar_url: Option<String>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_color_index: Option<i64>,
    #[typeshare(serialized_as = "I54")]
    pub avatar_color_index_effective: i64,
    #[typeshare(serialized_as = "Option<I54>")]
    pub last_seen_at: Option<i64>,
    pub online: bool,
}

/// Machine line on a project (`project_json` → `machine`).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineDto {
    #[typeshare(serialized_as = "String")]
    pub state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub since: i64,
    pub class: String,
    pub snapshot: Option<String>,
    pub image_outdated: bool,
    pub always_on: bool,
}

/// Project record as clients see it (`project_json`), plus optional `list_projects` extras.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub instructions: String,
    pub avatar: String,
    pub avatar_color: String,
    pub state: String,
    #[serde(rename = "box")]
    #[typeshare(serialized_as = "String")]
    pub box_state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub box_since: i64,
    #[typeshare(skip)]
    pub models: Value,
    pub spend_today_usd: f64,
    pub spend_cap_usd: f64,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub last_active_at: i64,
    pub description: String,
    pub voice: Option<String>,
    pub vision_enabled: bool,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_color_index: Option<i64>,
    #[typeshare(serialized_as = "I54")]
    pub avatar_color_index_effective: i64,
    /// D92 leaf avatar: the owner's picks (`null` = automatic) and the pair in effect.
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color: Option<i64>,
    /// Always sent by relays with D92; optional so older relays still decode.
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_effective: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color_effective: Option<i64>,
    pub primary_chat_id: Option<String>,
    pub owner: ProjectOwnerDto,
    pub model: Option<String>,
    pub machine: MachineDto,
    #[typeshare(skip)]
    pub activity: Value,
    /// `list_projects` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
    /// `list_projects` only (string user id overwrites `owner` on the wire after serialize).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_role: Option<String>,
    /// `list_projects` shared rows only; `None` omits, `Some(None)` is JSON null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<Option<String>>,
}

/// Project chat (`chat_json` / `chat_json_for`).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub is_pinned: bool,
    pub is_system: bool,
    pub kind: String,
    pub participants: Vec<String>,
    #[typeshare(serialized_as = "I54")]
    pub position: i64,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub doc_epoch: i64,
    pub room: String,
    /// Present only for `chat_json_for` (null or a timestamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub archived_at: Option<Option<i64>>,
    /// Present only for `chat_json_for`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<ChatMeDto>,
}

/// `me` on a project chat or a people chat.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMeDto {
    /// People chats always send this (null when the viewer is not a member). Project chats omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub archived_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_receipts: Option<bool>,
}

/// Nested agent project card inside a people-chat member.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMemberProjectDto {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub avatar: String,
    pub avatar_color: String,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_color_index: Option<i64>,
    #[typeshare(serialized_as = "I54")]
    pub avatar_color_index_effective: i64,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color: Option<i64>,
    /// Always sent by relays with D92; optional so older relays still decode.
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_effective: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color_effective: Option<i64>,
    pub description: String,
    pub model: Option<String>,
    pub owner: ProjectOwnerDto,
}

/// People-chat member: human and agent fields on one struct (absent keys for the other variant).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMemberDto {
    pub member: String,
    pub role: String,
    pub added_by: String,
    #[typeshare(serialized_as = "I54")]
    pub joined_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Human: always present (null or URL). Agent: omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub last_seen_at: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_receipts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub avatar_color_index: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_color_index_effective: Option<i64>,
    /// Agent: D92 leaf picks and the pair in effect. Human: omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub avatar_leaf: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub avatar_leaf_color: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_effective: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color_effective: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ChatMemberProjectDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent: owning user id string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_handle: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(skip)]
    pub settings: Option<Value>,
}

/// People chat (`people::chat_json`).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeopleChatDto {
    pub id: String,
    #[typeshare(serialized_as = "Option<I54>")]
    pub archived_at: Option<i64>,
    pub kind: String,
    pub created_by: String,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    pub title: String,
    pub image_key: Option<String>,
    pub pending: bool,
    pub members: Vec<ChatMemberDto>,
    pub room: String,
    #[typeshare(serialized_as = "I54")]
    pub head_seq: i64,
    #[typeshare(serialized_as = "I54")]
    pub last_activity_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub read_seq: i64,
    pub unread: bool,
    pub lock_state: String,
    pub me: ChatMeDto,
}

/// One row in `project_members_json` → `members`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMemberDto {
    pub id: String,
    pub handle: Option<String>,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
    pub status: String,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    pub user_id: String,
    pub role: String,
    #[typeshare(serialized_as = "I54")]
    pub joined_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub last_seen_at: Option<i64>,
    pub online: bool,
    #[typeshare(serialized_as = "I54")]
    pub avatar_color_index_effective: i64,
}

/// `project_members_json`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMembersDto {
    pub project_id: String,
    pub owner: String,
    pub members: Vec<ProjectMemberDto>,
}

/// One entry in `group_json` → `participants_detail`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupParticipantDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub color: String,
    #[serde(rename = "box")]
    #[typeshare(serialized_as = "String")]
    pub box_state: BoxState,
}

/// Group chat (`group_json`).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub is_pinned: bool,
    pub is_system: bool,
    pub kind: String,
    pub participants: Vec<String>,
    #[typeshare(serialized_as = "I54")]
    pub position: i64,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub doc_epoch: i64,
    pub room: String,
    pub participants_detail: Vec<GroupParticipantDto>,
}

/// Sandbox block on `create_project`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProjectSandboxDto {
    pub provider: String,
    pub sandbox_id: Option<String>,
    pub device_id: Option<String>,
    pub box_url: Option<String>,
}

/// Room URLs on `create_project`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProjectRoomsDto {
    pub project: String,
    pub pinned_chat: String,
}

/// `POST /projects` success body.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProjectResponseDto {
    pub project: ProjectDto,
    pub pinned_chat: ChatDto,
    pub sandbox: CreateProjectSandboxDto,
    pub rooms: CreateProjectRoomsDto,
}

/// `GET /projects/{id}` success body.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetProjectResponseDto {
    pub project: ProjectDto,
    pub chats: Vec<ChatDto>,
    #[serde(rename = "box")]
    #[typeshare(serialized_as = "String")]
    pub box_state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub box_since: i64,
    #[typeshare(skip)]
    pub spend_today: Value,
    pub skills_rev: String,
}

/// `POST /projects/{id}/chats` success body.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateChatResponseDto {
    pub chat: ChatDto,
    pub room: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
}
