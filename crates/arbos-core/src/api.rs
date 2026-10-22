//! Shared HTTP response shapes for projects, chats, people, me, machine,
//! unread, and calls (crdt-sync).
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

/// Public person card (`user_card`, directory, search, GitHub fallback, + presence).
///
/// Optional keys stay omitted when a builder does not set them
/// (`skip_serializing_if`), so no card gains keys it did not have before.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UserCardDto {
    pub id: String,
    pub handle: String,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<bool>,
    /// Absent before `with_presence`; after it, `Some(None)` is JSON null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub last_seen_at: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_color_index_effective: Option<i64>,
}

/// `user` on `/auth/token`, `/me`, and profile update replies.
///
/// Full rows always send `github_id`/`login`/… (null when unset). The
/// fallback `{ "id" }` omits those keys (`None` outer Option).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountUserDto {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub github_id: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<Option<I54>>")]
    pub last_login_at: Option<Option<i64>>,
}

/// Credits block on `GET /me`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreditsDto {
    pub balance_usd: f64,
    pub granted_usd: f64,
    pub spent_usd: f64,
    pub spent_today_usd: f64,
    pub daily_cap_usd: Option<f64>,
    pub allow_negative: bool,
    pub machine_today_usd: f64,
    pub machine_total_usd: f64,
}

/// `limits` on `GET /me`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeLimitsDto {
    pub read_only: bool,
    pub read_only_reason: Option<String>,
    pub signup: String,
    pub auth_mode: String,
}

/// `machine` summary on `GET /me`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeMachineDto {
    pub exists: bool,
    #[typeshare(serialized_as = "String")]
    pub state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub since: i64,
    pub provider: Option<String>,
    pub sandbox_id: Option<String>,
    pub always_on: bool,
    pub held: bool,
    pub waitlisted: bool,
    #[typeshare(serialized_as = "Option<I54>")]
    pub waitlist_position: Option<i64>,
    pub evicted: bool,
}

/// Device row on `GET /me` (the caller's device when known).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeDeviceDto {
    pub id: String,
    pub name: String,
    #[typeshare(serialized_as = "I54")]
    pub created_at: i64,
    #[typeshare(serialized_as = "Option<I54>")]
    pub revoked_at: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub last_seen_at: Option<i64>,
    pub kind: String,
}

/// Remote feature flags on `GET /me` (and `GET /secrets`). Every key is optional on the
/// wire so a client decodes an older relay that does not know a flag; the relay always sends
/// all of them. Key names are a contract with shipped apps: only add, never rename.
#[typeshare]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeFeaturesDto {
    /// #1265: the Connect 1Password flow (default off).
    #[serde(default)]
    pub vault_connect: Option<bool>,
    /// D92: agents without a photo draw a leaf, not initials (default on).
    #[serde(default)]
    pub leaf_avatars: Option<bool>,
}

/// `GET /me` success body.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeResponseDto {
    pub user: AccountUserDto,
    pub device: Option<MeDeviceDto>,
    pub credits: CreditsDto,
    pub limits: MeLimitsDto,
    #[serde(default)]
    pub features: MeFeaturesDto,
    pub machine: MeMachineDto,
}

/// `PATCH /me` and avatar route replies: `user`, optional `avatar_url`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeUserResponseDto {
    pub user: AccountUserDto,
    /// Present on avatar PUT/DELETE (`null` when cleared to no GitHub picture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<Option<String>>,
}

/// `GET /users` directory page.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsersPageDto {
    pub users: Vec<UserCardDto>,
    #[typeshare(serialized_as = "I54")]
    pub total: i64,
    #[typeshare(serialized_as = "I54")]
    pub offset: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub next_offset: Option<i64>,
}

/// One unread row on `GET /me/unread`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnreadChatDto {
    pub chat_id: String,
    pub project_id: String,
    #[typeshare(serialized_as = "I54")]
    pub unread: i64,
}

/// `GET /me/unread`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnreadDto {
    pub chats: Vec<UnreadChatDto>,
    #[typeshare(serialized_as = "I54")]
    pub total: i64,
}

/// Spec line on `GET /machine`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineSpecDto {
    #[typeshare(serialized_as = "I54")]
    pub cpu: i64,
    #[typeshare(serialized_as = "I54")]
    pub mem_gib: i64,
    #[typeshare(serialized_as = "I54")]
    pub disk_gib: i64,
    pub class: String,
}

/// One workspace row on `GET /machine`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineWorkspaceDto {
    pub project_id: String,
    pub state: String,
}

/// Image block on `GET /machine`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineImageDto {
    pub current: Option<String>,
    pub default: String,
}

/// `GET /machine` success body.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineStatusDto {
    pub user_id: String,
    #[typeshare(serialized_as = "String")]
    pub state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub since: i64,
    pub provider: Option<String>,
    pub sandbox_id: Option<String>,
    pub spec: MachineSpecDto,
    pub spend_today_usd: f64,
    pub spend_cap_usd: f64,
    pub workspaces: Vec<MachineWorkspaceDto>,
    pub migrated_from: Vec<String>,
    pub image: MachineImageDto,
    pub image_outdated: bool,
    #[typeshare(skip)]
    pub upgrade: Value,
    pub always_on: bool,
    pub held: bool,
    pub machine_today_usd: f64,
}

/// `POST /machine/start|stop|restart` reply.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineStateDto {
    #[typeshare(serialized_as = "String")]
    pub state: BoxState,
    #[typeshare(serialized_as = "I54")]
    pub since: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<bool>,
}

/// `sender{}` on calls, pushes, and LiveKit participant metadata (`Sender::json`).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SenderDto {
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
    /// D92: agents get the leaf pair in effect; people send JSON null.
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_effective: Option<i64>,
    #[typeshare(serialized_as = "Option<I54>")]
    pub avatar_leaf_color_effective: Option<i64>,
}

/// One device under `reachable[].devices` on a start-call reply.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReachableDeviceDto {
    pub device_id: String,
    pub name: String,
    pub kind: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    pub polled_within_30s: bool,
    #[typeshare(serialized_as = "Option<I54>")]
    pub last_seen_at: Option<i64>,
    pub voip_token: bool,
    pub push_token: bool,
}

/// One callee in `reachable[]` on a 201 start-call reply.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallReachabilityDto {
    pub user_id: String,
    pub handle: String,
    pub name: String,
    pub any_live: bool,
    pub any_voip: bool,
    pub any_push: bool,
    pub devices: Vec<ReachableDeviceDto>,
}

/// Call object (`call_json`): token fields only when the relay mints a join token.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallDto {
    pub call_id: String,
    pub chat_id: String,
    pub project_id: String,
    pub kind: String,
    pub room: String,
    pub livekit_url: Option<String>,
    #[typeshare(serialized_as = "I54")]
    pub max_participants: i64,
    pub started_by: SenderDto,
    pub participants: Vec<SenderDto>,
    pub agents: Vec<SenderDto>,
    #[typeshare(serialized_as = "I54")]
    pub started_at: i64,
    #[typeshare(serialized_as = "Option<I54>")]
    pub ended_at: Option<i64>,
    pub reason: Option<String>,
    pub started_device: Option<String>,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(serialized_as = "Option<I54>")]
    pub token_expires_at: Option<i64>,
}

/// `POST …/calls` success: new call (201) or join the live one (200).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartCallResponseDto {
    pub call: CallDto,
    pub joined: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<Vec<CallReachabilityDto>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[typeshare(skip)]
    pub agents_dispatched: Option<Value>,
}

/// `{ "call": … }` envelope (`GET /calls/{id}`, join, active-call).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallEnvelopeDto {
    pub call: CallDto,
}

/// `POST /calls/{id}/leave`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaveCallResponseDto {
    pub ended: bool,
    #[typeshare(serialized_as = "I54")]
    pub duration_s: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub already_ended: Option<bool>,
}

/// `POST /devices/{id}/voip`.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoipRegisterResponseDto {
    pub device_id: String,
    pub push: bool,
}

/// `/me/pushes` `call_started` payload.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallStartedPayloadDto {
    pub call: CallDto,
    pub sender: SenderDto,
}

/// `/me/pushes` `call_ended` payload.
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallEndedPayloadDto {
    pub call_id: String,
    pub chat_id: String,
    pub project_id: String,
    #[typeshare(serialized_as = "I54")]
    pub duration_s: i64,
    pub reason: String,
    pub writer_device: Option<String>,
}

/// `/me/pushes` `call_missed` payload (`from` is the starter).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallMissedPayloadDto {
    pub call_id: String,
    pub chat_id: String,
    pub project_id: String,
    pub kind: String,
    pub from: SenderDto,
    #[typeshare(serialized_as = "I54")]
    pub started_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub ended_at: i64,
    #[typeshare(serialized_as = "I54")]
    pub duration_s: i64,
    pub reason: Option<String>,
}

/// PushKit VoIP wake payload (`caller` is the starter).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRingPayloadDto {
    pub call_id: String,
    pub chat_id: String,
    pub project_id: String,
    pub kind: String,
    pub room: String,
    pub livekit_url: Option<String>,
    pub caller: SenderDto,
    #[typeshare(serialized_as = "I54")]
    pub started_at: i64,
}

/// APNs `ARBOS_CALL` alert body without `aps` (`sender` on started/missed only).
#[typeshare]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallAlertDto {
    pub event: String,
    pub chat_id: String,
    pub project_id: String,
    pub call: CallDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<SenderDto>,
}
