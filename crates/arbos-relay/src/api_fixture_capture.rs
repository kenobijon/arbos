//! Fixture tests: each projects/chats helper's DTO JSON matches the captured wire shape.

#![cfg(test)]

use serde_json::{json, Value};

use arbos_core::api::{
    AccountUserDto, CreditsDto, MachineImageDto, MachineSpecDto, MachineStatusDto,
    MachineWorkspaceDto, MeDeviceDto, MeFeaturesDto, MeLimitsDto, MeMachineDto, MeResponseDto,
    PairResponseDto, TokenResponseDto, UnreadChatDto, UnreadDto, UserCardDto, VoiceCatalogDto,
    VoiceDto,
};
use arbos_core::wire::BoxState;

use crate::api::{chat_dto, chat_dto_for, project_dto};
use crate::auth::Principal;
use crate::db::{Chat, Project};
use crate::db_people::{ChatWithMembers, PeopleChat};
use crate::people::{self, tests_support::person};
use crate::state::test_support::test_state;
use crate::state::{AppState, AuthMode};

const FIXED_AT: i64 = 1_700_000_000_000;

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api")
}

fn load(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture json")
}

fn assert_dto(name: &str, dto: impl serde::Serialize) {
    let got = serde_json::to_value(dto).expect("serialize dto");
    let path = fixtures_dir().join(name);
    if !path.exists() || std::env::var_os("UPDATE_API_FIXTURES").is_some() {
        let pretty = serde_json::to_string_pretty(&got).expect("pretty");
        std::fs::write(&path, format!("{pretty}\n")).expect("write fixture");
    }
    let want = load(name);
    assert_eq!(got, want, "fixture {name}");
}

fn fix_user_created_at(state: &AppState, user_id: &str) {
    state
        .db
        .with(|c| {
            c.execute(
                "UPDATE users SET created_at = ?2 WHERE id = ?1",
                rusqlite::params![user_id, FIXED_AT],
            )
        })
        .unwrap();
}

fn fix_member_joined(state: &AppState, chat_id: &str, member: &str) {
    state
        .db
        .with(|c| {
            c.execute(
                "UPDATE people_members SET joined_at = ?3 WHERE chat_id = ?1 AND member = ?2",
                rusqlite::params![chat_id, member, FIXED_AT],
            )
        })
        .unwrap();
}

fn insert_project(
    state: &AppState,
    id: &str,
    owner: &str,
    name: &str,
    chat_id: &str,
    avatar_color_index: Option<i64>,
) -> Project {
    let p = Project {
        id: id.into(),
        name: name.into(),
        icon: "Ab".into(),
        instructions: "be helpful".into(),
        models: json!({
            "talking_model": { "id": "anthropic/claude-haiku-5.5", "effort": "off" },
            "subagent_model": { "id": "anthropic/claude-sonnet-5.5", "effort": "medium" },
            "decision_model": { "id": "anthropic/claude-haiku-5.5", "effort": "off" }
        }),
        state: "hibernated_light".into(),
        created_at: FIXED_AT,
        last_active_at: FIXED_AT,
        spend_cap_daily: 25.0,
        color: "#8b7cf6".into(),
        user_id: owner.to_owned(),
        workspace_state: "stopped".into(),
        avatar: Some("oak".into()),
        avatar_color: Some("teal".into()),
        description: "A fixture agent".into(),
        primary_chat_id: Some(chat_id.into()),
        avatar_color_index,
        avatar_leaf: None,
        avatar_leaf_color: None,
    };
    state.db.insert_project(&p).unwrap();
    state
        .db
        .insert_chat(&Chat {
            id: chat_id.into(),
            project_id: id.into(),
            title: "Primary".into(),
            is_pinned: true,
            position: 0,
            created_at: FIXED_AT,
            doc_epoch: 1,
            kind: "chat".into(),
            participants: Vec::new(),
            forked_from: None,
        })
        .unwrap();
    p
}

fn fixture_state() -> (AppState, String, String, Principal, Project) {
    let state = test_state(AuthMode::Github);
    let (alice, alice_dev) = person(&state, 101);
    let (bob, _) = person(&state, 102);
    fix_user_created_at(&state, &alice);
    fix_user_created_at(&state, &bob);
    let p = insert_project(
        &state,
        "proj_fixture",
        &alice,
        "Fixture",
        "chat_primary",
        Some(3),
    );
    (state, alice, bob, alice_dev, p)
}

#[tokio::test]
async fn project_json_matches_fixture() {
    let (state, _, _, _, p) = fixture_state();
    assert_dto("project_json.json", project_dto(&state, &p).await);
}

#[test]
fn chat_json_matches_fixture() {
    let chat = Chat {
        id: "chat_fixture".into(),
        project_id: "proj_fixture".into(),
        title: "Hello".into(),
        is_pinned: false,
        position: 1,
        created_at: FIXED_AT,
        doc_epoch: 1,
        kind: "chat".into(),
        participants: Vec::new(),
        forked_from: None,
    };
    assert_dto("chat_json.json", chat_dto(&chat));
}

#[tokio::test]
async fn chat_json_for_matches_fixture() {
    let (state, alice, _, _, _) = fixture_state();
    let chat = Chat {
        id: "chat_fixture".into(),
        project_id: "proj_fixture".into(),
        title: "Hello".into(),
        is_pinned: false,
        position: 1,
        created_at: FIXED_AT,
        doc_epoch: 1,
        kind: "chat".into(),
        participants: Vec::new(),
        forked_from: None,
    };
    assert_dto(
        "chat_json_for.json",
        chat_dto_for(&state, &chat, Some(&alice)),
    );
}

#[tokio::test]
async fn member_json_matches_fixtures() {
    let (state, alice, bob, _, p) = fixture_state();
    let dm = PeopleChat {
        id: "people_dm".into(),
        kind: "dm".into(),
        created_by: alice.clone(),
        created_at: FIXED_AT,
        title: None,
        image_key: None,
    };
    state.db.insert_people_chat(&dm).unwrap();
    state
        .db
        .add_member(
            &dm.id,
            &format!("user:{alice}"),
            "owner",
            &alice,
            &json!({}),
        )
        .unwrap();
    state
        .db
        .add_member(&dm.id, &format!("user:{bob}"), "member", &alice, &json!({}))
        .unwrap();
    fix_member_joined(&state, &dm.id, &format!("user:{alice}"));
    fix_member_joined(&state, &dm.id, &format!("user:{bob}"));
    let human = state
        .db
        .members_of(&dm.id)
        .unwrap()
        .into_iter()
        .find(|m| m.user_id() == Some(alice.as_str()))
        .unwrap();
    assert_dto("member_json_human.json", people::member_dto(&state, &human));

    let agent_chat = PeopleChat {
        id: "people_agent".into(),
        kind: "group".into(),
        created_by: alice.clone(),
        created_at: FIXED_AT,
        title: Some("With agent".into()),
        image_key: None,
    };
    state.db.insert_people_chat(&agent_chat).unwrap();
    state
        .db
        .add_member(
            &agent_chat.id,
            &format!("user:{alice}"),
            "owner",
            &alice,
            &json!({}),
        )
        .unwrap();
    state
        .db
        .add_member(
            &agent_chat.id,
            &format!("agent:{}", p.id),
            "agent",
            &alice,
            &json!({ "reply_policy": "mention", "restrict_guests": false }),
        )
        .unwrap();
    fix_member_joined(&state, &agent_chat.id, &format!("user:{alice}"));
    fix_member_joined(&state, &agent_chat.id, &format!("agent:{}", p.id));
    let agent = state
        .db
        .members_of(&agent_chat.id)
        .unwrap()
        .into_iter()
        .find(|m| m.project_id() == Some(p.id.as_str()))
        .unwrap();
    assert_dto("member_json_agent.json", people::member_dto(&state, &agent));
}

#[tokio::test]
async fn people_chat_json_matches_fixtures() {
    let (state, alice, bob, alice_dev, _) = fixture_state();
    let dm = PeopleChat {
        id: "people_dm".into(),
        kind: "dm".into(),
        created_by: alice.clone(),
        created_at: FIXED_AT,
        title: None,
        image_key: None,
    };
    state.db.insert_people_chat(&dm).unwrap();
    state
        .db
        .add_member(
            &dm.id,
            &format!("user:{alice}"),
            "owner",
            &alice,
            &json!({}),
        )
        .unwrap();
    state
        .db
        .add_member(&dm.id, &format!("user:{bob}"), "member", &alice, &json!({}))
        .unwrap();
    fix_member_joined(&state, &dm.id, &format!("user:{alice}"));
    fix_member_joined(&state, &dm.id, &format!("user:{bob}"));
    let cm = ChatWithMembers {
        chat: state.db.people_chat(&dm.id).unwrap().unwrap(),
        members: state.db.members_of(&dm.id).unwrap(),
    };
    let device_id = match &alice_dev {
        Principal::Device { id, .. } => id.clone(),
        _ => "dev_fixture".into(),
    };
    assert_dto(
        "people_chat_json_with_me.json",
        people::people_chat_dto(&state, &cm, &alice, &device_id),
    );
    assert_dto(
        "people_chat_json_without_me.json",
        people::people_chat_dto(&state, &cm, "gh_999", &device_id),
    );
}

#[tokio::test]
async fn project_members_json_matches_fixture() {
    let (state, _, _, _, p) = fixture_state();
    assert_dto(
        "project_members_json.json",
        people::project_members_dto(&state, &p),
    );
}

#[tokio::test]
async fn group_json_matches_fixture() {
    let (state, alice, bob, _, p) = fixture_state();
    let other = insert_project(&state, "proj_other", &bob, "Other", "chat_other", None);
    let _ = alice;
    let group = Chat {
        id: "group_fixture".into(),
        project_id: p.id.clone(),
        title: "Group · Other".into(),
        is_pinned: false,
        position: 2,
        created_at: FIXED_AT,
        doc_epoch: 1,
        kind: "group".into(),
        participants: vec![p.id.clone(), other.id.clone()],
        forked_from: None,
    };
    state.db.insert_chat(&group).unwrap();
    assert_dto(
        "group_json.json",
        crate::groups::group_dto(&state, &group).await,
    );
}

#[test]
fn user_card_fixtures() {
    let state = test_state(AuthMode::Github);
    // Legacy device-token account (no GitHub login).
    state
        .db
        .with(|c| {
            c.execute(
                "INSERT INTO users(id, snapshot, spend_cap_usd, created_at) VALUES ('u_mac', NULL, 0, ?1)",
                rusqlite::params![FIXED_AT],
            )
        })
        .unwrap();
    state
        .db
        .insert_device("dev-mac", "Jacob's Mac", "tok", "u_mac", "paircode")
        .unwrap();
    let legacy = state.db.user_card("u_mac").unwrap().unwrap();
    assert_dto("user_card_legacy.json", legacy);

    let (alice, _) = person(&state, 301);
    fix_user_created_at(&state, &alice);
    let card = state.db.user_card(&alice).unwrap().unwrap();
    let with_presence = people::with_presence(&state, card);
    assert_dto("user_card_presence.json", with_presence);

    let github = UserCardDto {
        id: "gh_999001".into(),
        handle: "ghostuser".into(),
        avatar_url: Some("https://avatars.githubusercontent.com/u/999001?v=4".into()),
        name: Some("Ghost".into()),
        status: "github".into(),
        created_at: None,
        test: None,
        last_seen_at: None,
        online: None,
        avatar_color_index_effective: None,
    };
    assert_dto("user_card_github.json", github);
}

#[test]
fn me_and_machine_unread_fixtures() {
    let me_no_machine = MeResponseDto {
        user: AccountUserDto {
            id: "gh_301".into(),
            github_id: Some(Some(301)),
            login: Some(Some("user301".into())),
            avatar_url: Some(None),
            name: Some(None),
            created_at: Some(FIXED_AT),
            last_login_at: Some(Some(FIXED_AT)),
        },
        device: Some(MeDeviceDto {
            id: "dev_fixture".into(),
            name: "Test Mac".into(),
            created_at: FIXED_AT,
            revoked_at: None,
            last_seen_at: Some(FIXED_AT),
            kind: "github".into(),
        }),
        credits: CreditsDto {
            balance_usd: 20.0,
            granted_usd: 20.0,
            spent_usd: 0.0,
            spent_today_usd: 0.0,
            daily_cap_usd: None,
            allow_negative: false,
            machine_today_usd: 0.0,
            machine_total_usd: 0.0,
        },
        limits: MeLimitsDto {
            read_only: false,
            read_only_reason: None,
            signup: "open".into(),
            auth_mode: "github".into(),
        },
        features: MeFeaturesDto {
            vault_connect: Some(false),
            leaf_avatars: Some(true),
        },
        machine: MeMachineDto {
            exists: false,
            state: BoxState::Asleep,
            since: 0,
            provider: None,
            sandbox_id: None,
            always_on: false,
            held: false,
            waitlisted: false,
            waitlist_position: None,
            evicted: false,
        },
    };
    assert_dto("me_without_machine.json", me_no_machine.clone());

    let mut me_with = me_no_machine;
    me_with.machine = MeMachineDto {
        exists: true,
        state: BoxState::Awake,
        since: FIXED_AT,
        provider: Some("gvisor".into()),
        sandbox_id: Some("sb_fixture".into()),
        always_on: false,
        held: false,
        waitlisted: false,
        waitlist_position: None,
        evicted: false,
    };
    assert_dto("me_with_machine.json", me_with);

    let machine = MachineStatusDto {
        user_id: "gh_301".into(),
        state: BoxState::Asleep,
        since: 0,
        provider: None,
        sandbox_id: None,
        spec: MachineSpecDto {
            cpu: 4,
            mem_gib: 8,
            disk_gib: 10,
            class: String::new(),
        },
        spend_today_usd: 0.0,
        spend_cap_usd: 0.0,
        workspaces: vec![MachineWorkspaceDto {
            project_id: "proj_fixture".into(),
            state: "stopped".into(),
        }],
        migrated_from: Vec::new(),
        image: MachineImageDto {
            current: None,
            default: String::new(),
        },
        image_outdated: false,
        upgrade: json!({ "in_progress": false }),
        always_on: false,
        held: false,
        machine_today_usd: 0.0,
    };
    assert_dto("machine_status.json", machine);

    let unread = UnreadDto {
        chats: vec![UnreadChatDto {
            chat_id: "people_dm".into(),
            project_id: "people".into(),
            unread: 2,
        }],
        total: 2,
    };
    assert_dto("unread.json", unread);
}

/// Pair / token / healthz / voices / transcription mock (step 6b-2d-1).
/// Tokens and device ids are obviously fake — never log them.
#[test]
fn pair_token_health_voices_transcription_fixtures() {
    // Pairing reply shape (relay builds PairResponseDto).
    assert_dto(
        "pair_response.json",
        PairResponseDto {
            device_id: "dev_fixture".into(),
            device_token: "adv_fixture_not_a_real_token".into(),
        },
    );

    let token = TokenResponseDto {
        access_token: "fixture_access_token_not_real".into(),
        token_type: "Bearer".into(),
        expires_in: 3600,
        refresh_token: "arf_fixture_refresh_token_not_real".into(),
        device_id: "dev_fixture".into(),
        user: AccountUserDto {
            id: "gh_301".into(),
            github_id: Some(Some(301)),
            login: Some(Some("user301".into())),
            avatar_url: Some(None),
            name: Some(Some("Fixture User".into())),
            created_at: Some(FIXED_AT),
            last_login_at: Some(Some(FIXED_AT)),
        },
    };
    assert_dto("token_response.json", &token);
    // Refresh grace path returns the same TokenResponseDto JSON as issue_tokens.
    assert_dto(
        "token_response_refresh_grace.json",
        crate::signin::token_response_json(&token),
    );

    let state = test_state(AuthMode::Github);
    assert_dto("healthz.json", crate::api::healthz_dto(&state));

    let voices = VoiceCatalogDto {
        default_voice: "marin".into(),
        voices: vec![VoiceDto {
            id: "marin".into(),
            name: "Marin".into(),
            description: Some("Warm and clear.".into()),
            seconds: Some(2.5),
            sample_url: "http://127.0.0.1:7707/voices/marin/sample.mp3".into(),
        }],
    };
    assert_dto("voices.json", &voices);
    // Builder path with a catalogue on disk must match the same shape.
    {
        let dir = state.cfg.data_dir.join("voices-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("voices.json"),
            r#"{
  "default": "marin",
  "voices": [
    {
      "id": "marin",
      "name": "Marin",
      "description": "Warm and clear.",
      "seconds": 2.5,
      "line": "hello",
      "model": "fixture"
    }
  ]
}
"#,
        )
        .unwrap();
        let mut voice_cfg = (*state.voice).clone();
        voice_cfg.voices_dir = dir;
        voice_cfg.default_voice = "marin".into();
        let mut state = state.clone();
        state.voice = std::sync::Arc::new(voice_cfg);
        state.cfg = std::sync::Arc::new({
            let mut c = (*state.cfg).clone();
            c.public_url = "http://127.0.0.1:7707".into();
            c
        });
        let built = crate::voice::voices_catalog_dto(&state);
        assert_eq!(built, voices, "voices_catalog_dto matches fixture DTO");
    }

    let mock = crate::proxy::transcription_mock_json(1.5);
    assert_dto("transcription_mock.json", mock);
}

#[tokio::test]
async fn create_project_accepts_avatar_color_index() {
    use axum::extract::State;
    use axum::Json;

    use crate::api::{create_project, CreateProjectBody};

    let state = test_state(AuthMode::Github);
    let (alice, alice_dev) = person(&state, 201);
    let r = create_project(
        State(state.clone()),
        alice_dev,
        Json(CreateProjectBody {
            name: "Coloured".into(),
            icon: None,
            color: None,
            instructions: None,
            models: None,
            provider: None,
            avatar: None,
            avatar_color: None,
            description: None,
            avatar_color_index: Some(4),
            avatar_leaf: None,
            avatar_leaf_color: None,
            source_project_id: None,
        }),
    )
    .await
    .expect("create");
    assert_eq!(r.status(), axum::http::StatusCode::OK);
    let p = state
        .db
        .projects_of_user(&alice)
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Coloured")
        .expect("project");
    assert_eq!(p.avatar_color_index, Some(4));
}
