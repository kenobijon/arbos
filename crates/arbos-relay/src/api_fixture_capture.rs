//! Fixture tests: each projects/chats helper's DTO JSON matches the captured wire shape.

#![cfg(test)]

use serde_json::{json, Value};

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
    };
    state.db.insert_chat(&group).unwrap();
    assert_dto(
        "group_json.json",
        crate::groups::group_dto(&state, &group).await,
    );
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
