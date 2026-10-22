//! Map relay HTTP DTOs (`arbos_core::api`) onto Mac model types.
//!
//! Free-form JSON fields on the DTOs (`models`, `spend_today`, member
//! `settings`) stay as [`serde_json::Value`]; only the talking-model id /
//! effort, spend today, and reply policy are read from them.

use arbos_core::api::{
    AccountUserDto, ChatDto, ChatMemberDto, CreateChatResponseDto, GetProjectResponseDto, GroupDto,
    MachineStatusDto, MeResponseDto, PeopleChatDto, ProjectMemberDto, ProjectMembersDto,
    ProjectOwnerDto, UserCardDto,
};
use arbos_core::wire::BoxState as WireBox;
use serde_json::Value;

use crate::model::{
    AccountView, AgentMember, BoxState, Chat, ChatKind, Effort, Machine, MachineState, Person,
    Project, ProjectMachine, ReplyPolicy, SpaceKind,
};
use crate::sync::credentials::AccountUser;

/// Wire box word → the string `box_state_from` already understands.
fn wire_box_word(state: WireBox) -> &'static str {
    match state {
        WireBox::Awake => "awake",
        WireBox::Waking => "waking",
        WireBox::Asleep => "asleep",
        WireBox::Error => "error",
    }
}

fn box_state_from(state: &str, box_state: &str) -> BoxState {
    match state {
        "awake" => BoxState::Awake,
        "waking" => BoxState::Waking,
        "hibernated_light" => BoxState::HibernatedLight,
        "hibernated_deep" => BoxState::HibernatedDeep,
        "stopped_by_user" => BoxState::StoppedByUser,
        "error" => BoxState::Error,
        _ => match box_state {
            "awake" => BoxState::Awake,
            "waking" => BoxState::Waking,
            "error" => BoxState::Error,
            _ => BoxState::HibernatedLight,
        },
    }
}

fn effort_from(v: &Value) -> Effort {
    match v.as_str().unwrap_or("") {
        "high" | "xhigh" | "max" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    }
}

fn chat_kind(s: &str) -> ChatKind {
    serde_json::from_value(Value::String(s.to_owned())).unwrap_or_default()
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn color_index(effective: i64) -> u8 {
    (effective.rem_euclid(7)) as u8
}

impl From<&ProjectOwnerDto> for Person {
    fn from(o: &ProjectOwnerDto) -> Self {
        Person {
            id: o.id.clone(),
            handle: o.handle.clone(),
            name: non_empty(&o.name),
            avatar_url: o.avatar_url.clone().unwrap_or_default(),
            avatar_color_index: Some(color_index(o.avatar_color_index_effective)),
            status: "member".into(),
            role: String::new(),
            last_seen_at: o.last_seen_at,
            online: o.online,
        }
    }
}

impl From<&UserCardDto> for Person {
    fn from(c: &UserCardDto) -> Self {
        Person {
            id: c.id.clone(),
            handle: c.handle.clone(),
            name: c.name.clone().filter(|n| !n.is_empty()),
            avatar_url: c.avatar_url.clone().unwrap_or_default(),
            avatar_color_index: c.avatar_color_index_effective.map(color_index),
            status: if c.status.is_empty() {
                "member".into()
            } else {
                c.status.clone()
            },
            role: String::new(),
            last_seen_at: c.last_seen_at.flatten(),
            online: c.online.unwrap_or(false),
        }
    }
}

impl From<&AccountUserDto> for AccountUser {
    fn from(u: &AccountUserDto) -> Self {
        AccountUser {
            id: u.id.clone(),
            login: u.login.clone().flatten().unwrap_or_default(),
            avatar_url: u.avatar_url.clone().flatten().unwrap_or_default(),
            name: u.name.clone().flatten().unwrap_or_default(),
        }
    }
}

/// `GET /me` → Settings account header.
pub fn account_view_from_me(dto: &MeResponseDto) -> AccountView {
    AccountView {
        id: dto.user.id.clone(),
        login: dto.user.login.clone().flatten().unwrap_or_default(),
        name: dto.user.name.clone().flatten().unwrap_or_default(),
        avatar_url: dto.user.avatar_url.clone().flatten().unwrap_or_default(),
        device_name: dto
            .device
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_default(),
        balance_usd: dto.credits.balance_usd,
        spent_today_usd: dto.credits.spent_today_usd,
        daily_cap_usd: dto.credits.daily_cap_usd,
    }
}

fn machine_state_from_word(word: &str) -> MachineState {
    match word {
        "awake" => MachineState::Awake,
        "waking" => MachineState::Waking,
        "error" => MachineState::Error,
        _ => MachineState::Asleep,
    }
}

/// Apply `GET /machine` onto the live machine snapshot (keeps waitlist/provider when absent).
pub fn apply_machine_status(m: &mut Machine, dto: &MachineStatusDto) {
    let word = match dto.state {
        WireBox::Awake => "awake",
        WireBox::Waking => "waking",
        WireBox::Asleep => "asleep",
        WireBox::Error => "error",
    };
    let state = machine_state_from_word(word);
    let since = (dto.since > 0).then_some(dto.since as u64);
    if m.state != state {
        m.since = since.or(Some(crate::model::now_ms()));
    } else if since.is_some() {
        m.since = since;
    }
    m.state = state;
    m.detail = None;
    m.progress = None;
    m.spec = Some(format!(
        "{} vCPU · {} GiB · {} GiB",
        dto.spec.cpu, dto.spec.mem_gib, dto.spec.disk_gib
    ));
    if !dto.spec.class.is_empty() {
        m.class = Some(dto.spec.class.clone());
    }
    m.cpu = Some(dto.spec.cpu as u32);
    m.mem_gib = Some(dto.spec.mem_gib as u32);
    if let Some(p) = dto.provider.as_deref().filter(|s| !s.is_empty()) {
        m.provider = Some(p.to_owned());
    }
    if let Some(img) = dto.image.current.as_deref().filter(|s| !s.is_empty()) {
        m.image = Some(img.to_owned());
    }
    m.spend_today_usd = dto.spend_today_usd;
    m.spend_cap_usd = dto.spend_cap_usd;
    m.always_on = dto.always_on;
}

fn person_from_project_member(m: &ProjectMemberDto) -> Person {
    let id = if m.user_id.is_empty() {
        m.id.clone()
    } else {
        m.user_id.clone()
    };
    Person {
        id: id.clone(),
        handle: m.handle.clone().unwrap_or(id),
        name: m.name.clone().filter(|n| !n.is_empty()),
        avatar_url: m.avatar_url.clone().unwrap_or_default(),
        avatar_color_index: Some(color_index(m.avatar_color_index_effective)),
        status: if m.status.is_empty() {
            "member".into()
        } else {
            m.status.clone()
        },
        role: if m.role.is_empty() {
            "member".into()
        } else {
            m.role.clone()
        },
        last_seen_at: m.last_seen_at,
        online: m.online,
    }
}

fn machine_from_dto(m: &arbos_core::api::MachineDto) -> ProjectMachine {
    ProjectMachine {
        state: wire_box_word(m.state).to_owned(),
        since: Some(m.since),
        class: m.class.clone(),
        snapshot: m.snapshot.clone(),
        image_outdated: m.image_outdated,
        always_on: m.always_on,
    }
}

/// `GET /projects/{id}` (+ optional members) → rail [`Project`].
pub(crate) fn project_from_dto(
    detail: &GetProjectResponseDto,
    members: Option<&ProjectMembersDto>,
) -> Project {
    let p = &detail.project;
    let talking = p
        .models
        .get("talking_model")
        .cloned()
        .unwrap_or(Value::Null);
    let people = members
        .map(|m| m.members.iter().map(person_from_project_member).collect())
        .unwrap_or_default();
    Project {
        id: p.id.clone(),
        name: if p.name.is_empty() {
            "Project".into()
        } else {
            p.name.clone()
        },
        icon: if p.icon.is_empty() {
            "Ω".into()
        } else {
            p.icon.clone()
        },
        color: "#8b7cf6".into(),
        state: box_state_from(&p.state, wire_box_word(detail.box_state)),
        state_detail: None,
        wake_progress: None,
        // Empty when the relay omits talking_model; refresh_registry fills from
        // the previous project, then the default id.
        model: talking
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| p.model.clone().filter(|s| !s.is_empty()))
            .unwrap_or_default(),
        effort: effort_from(talking.get("effort").unwrap_or(&Value::Null)),
        spend_today_usd: detail.spend_today.as_f64().unwrap_or(0.0),
        spend_cap_usd: p.spend_cap_usd,
        kind: SpaceKind::Project,
        agents: Vec::new(),
        pending: false,
        people,
        shared: p.shared.unwrap_or(false),
        owner_handle: p.owner_handle.clone().flatten().filter(|h| !h.is_empty()),
        avatar: non_empty(&p.avatar),
        avatar_color: non_empty(&p.avatar_color),
        description: non_empty(&p.description),
        owner_person: Some(Person::from(&p.owner)),
        machine_info: Some(machine_from_dto(&p.machine)),
        muted: false,
        primary_chat: p.primary_chat_id.as_deref().and_then(non_empty),
        voice: p.voice.as_deref().and_then(non_empty),
        vision_enabled: p.vision_enabled,
        avatar_color_index: Some(color_index(p.avatar_color_index_effective)),
        avatar_color_override: p.avatar_color_index.map(color_index),
        image_url: None,
    }
}

/// `ChatDto` / create-chat nested chat → [`Chat`].
pub(crate) fn chat_from_dto(dto: &ChatDto) -> Chat {
    Chat {
        id: dto.id.clone(),
        project_id: dto.project_id.clone(),
        title: if dto.title.is_empty() {
            "New chat".into()
        } else {
            dto.title.clone()
        },
        is_pinned: dto.is_pinned,
        is_system: dto.is_system,
        kind: chat_kind(&dto.kind),
        participants: dto.participants.clone(),
        unread: false,
        last_message_at: dto.created_at.max(0) as u64,
        archived_at: dto
            .archived_at
            .flatten()
            .filter(|t| *t > 0)
            .map(|t| t as u64),
    }
}

/// `GroupDto` → [`Chat`] (same fields the old `parse_chat` read).
pub(crate) fn chat_from_group_dto(dto: &GroupDto) -> Chat {
    Chat {
        id: dto.id.clone(),
        project_id: dto.project_id.clone(),
        title: if dto.title.is_empty() {
            "New chat".into()
        } else {
            dto.title.clone()
        },
        is_pinned: dto.is_pinned,
        is_system: dto.is_system,
        kind: chat_kind(&dto.kind),
        participants: dto.participants.clone(),
        unread: false,
        last_message_at: dto.created_at.max(0) as u64,
        archived_at: None,
    }
}

/// Nested chat on `CreateChatResponseDto`.
pub(crate) fn chat_from_create(dto: &CreateChatResponseDto) -> Chat {
    chat_from_dto(&dto.chat)
}

fn agent_from_member(m: &ChatMemberDto, pid: &str) -> AgentMember {
    let project = m.project.as_ref();
    let name = project
        .map(|p| p.name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("Agent")
        .to_owned();
    let reply_policy = match m
        .settings
        .as_ref()
        .and_then(|s| s.get("reply_policy"))
        .and_then(Value::as_str)
    {
        Some("every_message") => ReplyPolicy::EveryMessage,
        _ => ReplyPolicy::Mention,
    };
    AgentMember {
        project_id: pid.to_owned(),
        handle: m
            .handle
            .clone()
            .unwrap_or_else(|| Project::mention_slug(&name)),
        icon: project
            .map(|p| p.icon.as_str())
            .filter(|i| !i.is_empty())
            .unwrap_or("Ω")
            .to_owned(),
        color: project
            .map(|p| p.color.as_str())
            .filter(|c| !c.is_empty())
            .unwrap_or("#8b7cf6")
            .to_owned(),
        name,
        owner_id: m.owner.clone().unwrap_or_default(),
        owner_handle: m.owner_handle.clone().flatten().unwrap_or_default(),
        description: m
            .description
            .clone()
            .filter(|d| !d.is_empty())
            .or_else(|| project.and_then(|p| non_empty(&p.description))),
        model: project.and_then(|p| p.model.as_deref()).and_then(non_empty),
        owner: project.map(|p| Person::from(&p.owner)),
        reply_policy,
        avatar: project.and_then(|p| non_empty(&p.avatar)),
        avatar_color: project
            .and_then(|p| non_empty(&p.avatar_color))
            .or_else(|| m.avatar_color.as_deref().and_then(non_empty)),
        avatar_color_index: project
            .map(|p| color_index(p.avatar_color_index_effective))
            .or(m.avatar_color_index_effective.map(color_index)),
    }
}

/// `GET /chats` item → a rail space (kind dm/group) and its one chat.
pub(crate) fn people_space_from(
    dto: &PeopleChatDto,
    me_id: &str,
    me_handle: &str,
    http_url: &str,
    token: &str,
) -> (Project, Chat) {
    let id = dto.id.clone();
    let mut people: Vec<Person> = Vec::new();
    let mut agents: Vec<AgentMember> = Vec::new();
    let mut i_own = false;
    for m in &dto.members {
        if let Some(uid) = m.member.strip_prefix("user:") {
            if uid == me_id {
                i_own = m.role == "owner";
                continue;
            }
            people.push(Person {
                id: uid.to_owned(),
                handle: m.handle.clone().unwrap_or_else(|| uid.to_owned()),
                name: m.name.clone().flatten(),
                avatar_url: m.avatar_url.clone().flatten().unwrap_or_default(),
                status: m.status.clone().unwrap_or_else(|| "member".into()),
                role: if m.role.is_empty() {
                    "owner".into()
                } else {
                    m.role.clone()
                },
                last_seen_at: m.last_seen_at.flatten(),
                online: m.online.unwrap_or(false),
                avatar_color_index: m.avatar_color_index_effective.map(color_index),
            });
        } else if let Some(pid) = m.member.strip_prefix("agent:") {
            agents.push(agent_from_member(m, pid));
        }
    }
    let people_owner: Option<String> = people
        .iter()
        .find(|p| p.role == "owner")
        .map(|p| p.handle.clone());
    let kind = match dto.kind.as_str() {
        "group" => SpaceKind::Group,
        _ => SpaceKind::Dm,
    };
    let title = if dto.title.is_empty() {
        people
            .first()
            .map(|p| p.handle.clone())
            .unwrap_or_else(|| "chat".into())
    } else {
        dto.title.clone()
    };
    let pending = dto.pending || people.iter().all(|p| p.status == "invited");
    let icon = people
        .first()
        .map(|p| p.initial())
        .unwrap_or_else(|| "@".into());
    let space = Project {
        id: id.clone(),
        name: title.clone(),
        icon,
        color: "#8b7cf6".into(),
        state: BoxState::Awake,
        state_detail: None,
        wake_progress: None,
        model: String::new(),
        effort: Effort::Low,
        spend_today_usd: 0.0,
        spend_cap_usd: 0.0,
        kind,
        people,
        agents,
        pending,
        shared: false,
        owner_handle: if i_own {
            Some(me_handle.to_owned())
        } else {
            people_owner
        },
        avatar: None,
        avatar_color: None,
        description: None,
        owner_person: None,
        machine_info: None,
        muted: dto.me.muted.unwrap_or(false),
        primary_chat: None,
        voice: None,
        vision_enabled: true,
        avatar_color_index: None,
        avatar_color_override: None,
        image_url: dto
            .image_key
            .as_deref()
            .and_then(non_empty)
            .map(|k| format!("{http_url}/blob/people:{id}/{k}?token={token}")),
    };
    let chat = Chat {
        id: id.clone(),
        project_id: id,
        title,
        is_pinned: true,
        is_system: false,
        kind: ChatKind::Chat,
        participants: Vec::new(),
        unread: dto.unread,
        last_message_at: dto.last_activity_at.max(0) as u64,
        archived_at: dto.archived_at.filter(|t| *t > 0).map(|t| t as u64),
    };
    (space, chat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbos_core::api::{ChatMemberDto, MeResponseDto, ProjectDto, UnreadDto};
    use std::fs;
    use std::path::PathBuf;

    fn fixture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../arbos-relay/tests/fixtures/api")
            .join(name);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn load_json<T: serde::de::DeserializeOwned>(name: &str) -> T {
        serde_json::from_str(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    #[test]
    fn person_from_user_card_fixtures() {
        let legacy: UserCardDto = load_json("user_card_legacy.json");
        let p = Person::from(&legacy);
        assert_eq!(p.id, "u_mac");
        assert_eq!(p.handle, "jacobs-mac");
        assert_eq!(p.name.as_deref(), Some("Jacob's Mac"));

        let presence: UserCardDto = load_json("user_card_presence.json");
        let p = Person::from(&presence);
        assert_eq!(p.handle, "user301");
        assert!(!p.online);
        assert_eq!(p.avatar_color_index, Some(2));

        let github: UserCardDto = load_json("user_card_github.json");
        let p = Person::from(&github);
        assert_eq!(p.status, "github");
        assert_eq!(p.handle, "ghostuser");
    }

    #[test]
    fn machine_and_account_from_fixtures() {
        let status: MachineStatusDto = load_json("machine_status.json");
        let mut m = Machine::default();
        apply_machine_status(&mut m, &status);
        assert_eq!(m.state, MachineState::Asleep);
        assert_eq!(m.cpu, Some(4));
        assert_eq!(m.mem_gib, Some(8));

        let me: MeResponseDto = load_json("me_with_machine.json");
        let view = account_view_from_me(&me);
        assert_eq!(view.id, "gh_301");
        assert_eq!(view.login, "user301");
        assert_eq!(view.device_name, "Test Mac");
        assert!((view.balance_usd - 20.0).abs() < f64::EPSILON);

        let unread: UnreadDto = load_json("unread.json");
        assert_eq!(unread.total, 2);
        assert_eq!(unread.chats[0].chat_id, "people_dm");
    }

    #[test]
    fn project_from_dto_fixture() {
        let project: ProjectDto = load_json("project_json.json");
        let members: ProjectMembersDto = load_json("project_members_json.json");
        let detail = GetProjectResponseDto {
            box_state: project.box_state,
            box_since: project.box_since,
            spend_today: serde_json::json!(project.spend_today_usd),
            skills_rev: String::new(),
            chats: Vec::new(),
            project,
        };
        let p = project_from_dto(&detail, Some(&members));
        assert_eq!(p.id, "proj_fixture");
        assert_eq!(p.name, "Fixture");
        assert_eq!(
            p.owner_person.as_ref().map(|o| o.handle.as_str()),
            Some("user101")
        );
        assert!((p.spend_cap_usd - 25.0).abs() < f64::EPSILON);
        assert_eq!(p.primary_chat.as_deref(), Some("chat_primary"));
        assert_eq!(p.people.len(), 1);
    }

    #[test]
    fn chat_from_dto_fixture() {
        let dto: ChatDto = load_json("chat_json_for.json");
        let chat = chat_from_dto(&dto);
        assert_eq!(chat.id, "chat_fixture");
        assert_eq!(chat.project_id, "proj_fixture");
        assert_eq!(chat.title, "Hello");
        assert!(chat.archived_at.is_none());
    }

    #[test]
    fn people_space_from_fixtures() {
        let with_me: PeopleChatDto = load_json("people_chat_json_with_me.json");
        let (space, _) = people_space_from(&with_me, "gh_101", "user101", "http://relay", "tok");
        assert_eq!(space.kind, SpaceKind::Dm);
        assert_eq!(space.name, "user102");
        assert_eq!(space.people.len(), 1);
        assert!(space.agents.is_empty());

        let without_me: PeopleChatDto = load_json("people_chat_json_without_me.json");
        let (space, _) = people_space_from(&without_me, "", "", "http://relay", "tok");
        assert_eq!(space.kind, SpaceKind::Dm);
        assert_eq!(space.name, "user101, user102");
        assert_eq!(space.people.len(), 2);

        let mut with_agent = with_me;
        let agent: ChatMemberDto = load_json("member_json_agent.json");
        with_agent.members.push(agent);
        let (space, _) = people_space_from(&with_agent, "gh_101", "user101", "http://relay", "tok");
        assert_eq!(space.agents.len(), 1);
        assert_eq!(space.agents[0].name, "Fixture");
    }

    #[test]
    fn group_dto_participants_fixture() {
        let dto: GroupDto = load_json("group_json.json");
        assert_eq!(dto.participants, vec!["proj_fixture", "proj_other"]);
        let chat = chat_from_group_dto(&dto);
        assert_eq!(chat.id, "group_fixture");
        assert_eq!(chat.participants, vec!["proj_fixture", "proj_other"]);
    }
}
