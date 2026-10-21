//! Backfill (design §4 stage A.4): walk every existing chat, queue its room and members, then
//! replay its history oldest-first as `Message` jobs with the original timestamps. The relay
//! normally never parses Loro bytes; here it rebuilds each chat doc from the room's checkpoint
//! and rows with `arbos_core::doc::ChatDoc` — read-only, in memory, nothing written back.

use serde::Serialize;
use serde_json::Value;

use arbos_core::doc::{message_text, ChatDoc};
use arbos_core::wire;

use super::Job;
use crate::people::PEOPLE_PROJECT;
use crate::state::AppState;

#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub user_id: Option<String>,
    pub chat_id: Option<String>,
    pub project_id: Option<String>,
    pub rooms_only: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub chats: usize,
    pub rooms_queued: usize,
    pub messages_queued: usize,
    pub reads_queued: usize,
    pub skipped_empty: usize,
    pub doc_errors: usize,
}

/// One chat to mirror: `(chat_id, project_id or None for people chats)`.
type Target = (String, Option<String>);

fn targets(state: &AppState, scope: &Scope) -> Vec<Target> {
    if let Some(chat_id) = &scope.chat_id {
        return vec![(
            chat_id.clone(),
            scope.project_id.clone().filter(|p| p != PEOPLE_PROJECT),
        )];
    }
    let mut out: Vec<Target> = Vec::new();
    let users: Vec<String> = match &scope.user_id {
        Some(u) => vec![u.clone()],
        None => state
            .db
            .list_users()
            .unwrap_or_default()
            .into_iter()
            .map(|u| u.id)
            .collect(),
    };
    for u in &users {
        for cm in state
            .db
            .chats_of_member(&format!("user:{u}"))
            .unwrap_or_default()
        {
            out.push((cm.chat.id, None));
        }
        for p in state.db.projects_of_user(u).unwrap_or_default() {
            for c in state.db.list_chats(&p.id).unwrap_or_default() {
                out.push((c.id, Some(p.id.clone())));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Rebuild a chat's doc from the relay's checkpoint and rows; `None` when the room has nothing.
pub fn load_doc(
    state: &AppState,
    chat_id: &str,
    project_id: Option<&str>,
) -> anyhow::Result<Option<ChatDoc>> {
    let room = wire::room_name(project_id.unwrap_or(PEOPLE_PROJECT), chat_id);
    let checkpoint = state.db.checkpoint_bytes(&room)?;
    let mut doc = match &checkpoint {
        Some((_, bytes)) => ChatDoc::from_snapshot(None, false, bytes)?,
        None => ChatDoc::new(None, false),
    };
    let mut after = checkpoint.map(|(seq, _)| seq).unwrap_or(0);
    let mut any = checkpoint_present(&doc);
    loop {
        let rows = state.db.rows_after(&room, after, None, 500)?;
        if rows.is_empty() {
            break;
        }
        for r in rows {
            after = r.seq;
            if doc.import(&r.bytes).is_ok() {
                any = true;
            }
        }
    }
    Ok(any.then_some(doc))
}

fn checkpoint_present(doc: &ChatDoc) -> bool {
    !doc.messages_list().is_empty()
}

/// The sender string a message row carries, normalised for the room: bare `user` is the
/// project owner, bare `agent` the project's agent; `system`/`project:*`/`routine:*` stay
/// system lines.
fn sender_of(row: &Value, owner: Option<&str>, project_id: Option<&str>) -> String {
    let raw = row["sender"].as_str().unwrap_or("");
    let role = row["role"].as_str().unwrap_or("");
    match (role, raw) {
        (_, s) if s.starts_with("user:") || s.starts_with("agent:") => s.to_owned(),
        ("user", _) => owner
            .map(|o| format!("user:{o}"))
            .unwrap_or_else(|| "system".into()),
        ("assistant", _) => project_id
            .map(|p| format!("agent:{p}"))
            .unwrap_or_else(|| "system".into()),
        _ => "system".to_owned(),
    }
}

/// Parts worth carrying besides the text: attachments and visible status/error lines. Tool
/// calls, reasoning and hidden parts stay in the Loro doc (the agent control plane).
fn carried_parts(row: &Value) -> Option<Value> {
    let parts = row["parts"].as_array()?;
    let keep: Vec<Value> = parts
        .iter()
        .filter(|p| p["hidden"].as_bool() != Some(true))
        .filter(|p| {
            matches!(
                p["kind"].as_str(),
                Some("attachment" | "error" | "question" | "approval")
            )
        })
        .cloned()
        .collect();
    (!keep.is_empty()).then_some(Value::Array(keep))
}

/// Queue the rooms and history of every chat in `scope`.
pub fn run(state: &AppState, scope: Scope) -> Report {
    let mut rep = Report::default();
    for (chat_id, project_id) in targets(state, &scope) {
        rep.chats += 1;
        let owner: Option<String> = match &project_id {
            Some(p) => state.db.project(p).ok().flatten().map(|p| p.user_id),
            None => None,
        };
        let doc = match load_doc(state, &chat_id, project_id.as_deref()) {
            Ok(Some(d)) => Some(d),
            Ok(None) => None,
            Err(e) => {
                rep.doc_errors += 1;
                tracing::warn!(chat = %chat_id, error = %e, "matrix backfill: doc rebuild failed");
                None
            }
        };
        // A project tab that never had a message gets no room: it would only be noise.
        if doc.is_none() && project_id.is_some() {
            rep.skipped_empty += 1;
            continue;
        }
        state.matrix.enqueue(
            state,
            Job::SyncRoom {
                chat_id: chat_id.clone(),
                project_id: project_id.clone(),
            },
        );
        rep.rooms_queued += 1;
        if scope.rooms_only {
            continue;
        }
        let Some(doc) = doc else { continue };
        for row in doc.projected_rows() {
            let status = row["status"].as_str().unwrap_or("done");
            if matches!(status, "queued" | "streaming") {
                continue;
            }
            let Some(id) = row["id"].as_str() else {
                continue;
            };
            let text = message_text(&row);
            let parts = carried_parts(&row);
            if text.trim().is_empty() && parts.is_none() {
                continue;
            }
            state.matrix.enqueue(
                state,
                Job::Message {
                    chat_id: chat_id.clone(),
                    project_id: project_id.clone(),
                    message_id: id.to_owned(),
                    sender: sender_of(&row, owner.as_deref(), project_id.as_deref()),
                    text,
                    parts,
                    ts: row["createdAt"].as_i64(),
                },
            );
            rep.messages_queued += 1;
        }
    }
    // Read marks: a member with nothing unread in a chat gets a receipt on its newest event.
    if !scope.rooms_only {
        let users: Vec<String> = match &scope.user_id {
            Some(u) => vec![u.clone()],
            None => state
                .db
                .list_users()
                .unwrap_or_default()
                .into_iter()
                .map(|u| u.id)
                .collect(),
        };
        let wanted: std::collections::HashSet<String> =
            targets(state, &scope).into_iter().map(|(c, _)| c).collect();
        for u in users {
            let unread: std::collections::HashSet<String> = state
                .db
                .unread_for_user(&u)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.unread > 0)
                .map(|c| c.chat_id)
                .collect();
            for cm in state
                .db
                .chats_of_member(&format!("user:{u}"))
                .unwrap_or_default()
            {
                if wanted.contains(&cm.chat.id) && !unread.contains(&cm.chat.id) {
                    state.matrix.read(state, &cm.chat.id, &u);
                    rep.reads_queued += 1;
                }
            }
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn senders_normalise() {
        let r = json!({ "role": "user", "sender": "user" });
        assert_eq!(sender_of(&r, Some("gh_1"), Some("P")), "user:gh_1");
        let r = json!({ "role": "assistant", "sender": "agent" });
        assert_eq!(sender_of(&r, Some("gh_1"), Some("P")), "agent:P");
        let r = json!({ "role": "user", "sender": "user:gh_9" });
        assert_eq!(sender_of(&r, Some("gh_1"), None), "user:gh_9");
        let r = json!({ "role": "system", "sender": "system" });
        assert_eq!(sender_of(&r, Some("gh_1"), Some("P")), "system");
        let r = json!({ "role": "project", "sender": "project:X" });
        assert_eq!(sender_of(&r, None, None), "system");
    }

    #[test]
    fn only_visible_non_tool_parts_are_carried() {
        let row = json!({ "parts": [
            { "kind": "text", "text": "hi" },
            { "kind": "tool", "hidden": true },
            { "kind": "attachment", "attachment": { "name": "a.png" } },
            { "kind": "reasoning", "reasoning": "…" },
        ]});
        let p = carried_parts(&row).unwrap();
        assert_eq!(p.as_array().unwrap().len(), 1);
        assert_eq!(p[0]["kind"], "attachment");
        assert!(carried_parts(&json!({ "parts": [{ "kind": "text", "text": "x" }] })).is_none());
    }
}

#[cfg(test)]
mod doc_tests {
    //! Backfill against a real chat doc: a project chat with a user message and a finished
    //! assistant reply, stored as the relay stores it (one row of Loro bytes), comes out as one
    //! `SyncRoom` job and two `Message` jobs with the original timestamps and senders.

    use arbos_core::doc::{ChatDoc, MessageStatus, UserMessageOpts};
    use arbos_core::ids::ulid;
    use arbos_core::wire;

    use super::*;
    use crate::matrix::{Config, Matrix, Mode};
    use crate::people::tests_support::{person, project_of};
    use crate::state::test_support::test_state;
    use crate::state::AuthMode;

    fn dual(state: &mut AppState) {
        let mut cfg = Config::off();
        cfg.mode = Mode::Dual;
        cfg.as_token_users = "t".into();
        cfg.hs_token_users = "h".into();
        state.matrix = Matrix::new(cfg);
    }

    #[test]
    fn project_chat_history_becomes_jobs() {
        let mut state = test_state(AuthMode::Github);
        dual(&mut state);
        let (owner, _) = person(&state, 2001);
        let project = project_of(&state, &owner, "Research");
        let chat_id = state.db.list_chats(&project).expect("chats")[0].id.clone();

        let mut doc = ChatDoc::new(Some(7), false);
        doc.init_meta(&chat_id, &project, 1);
        let user_msg = ulid();
        doc.append_user_message(
            &user_msg,
            "what is the plan?",
            "dev1",
            &UserMessageOpts::default(),
        )
        .expect("user message");
        let reply = ulid();
        doc.start_assistant_message(&reply, &user_msg, "box1");
        let part = doc.add_text_part(&reply, "text").expect("part");
        doc.append_text(&reply, &part, "Three steps.");
        doc.finish_message(&reply, MessageStatus::Done);
        let streaming = ulid();
        doc.start_assistant_message(&streaming, &user_msg, "box1");
        let bytes = doc.export_updates_since(&Default::default());
        let room = wire::room_name(&project, &chat_id);
        state
            .db
            .append_row(&room, "box1", &ulid(), &bytes)
            .expect("row");

        let rep = run(
            &state,
            Scope {
                user_id: Some(owner.clone()),
                ..Default::default()
            },
        );
        assert_eq!(rep.rooms_queued, 1);
        assert_eq!(rep.messages_queued, 2, "the streaming row is not mirrored");
        let due = state.db.matrix_outbox_due(10).expect("due");
        assert!(
            matches!(&due[0].job, Job::SyncRoom { chat_id: c, project_id: Some(p) } if *c == chat_id && *p == project)
        );
        // Later jobs wait behind the sync (one chat, strict order).
        assert_eq!(due.len(), 1);
        state.db.matrix_outbox_done(due[0].id).expect("done");
        let due = state.db.matrix_outbox_due(10).expect("due");
        let Job::Message {
            sender,
            text,
            ts,
            message_id,
            ..
        } = &due[0].job
        else {
            panic!("message job");
        };
        assert_eq!(sender, &format!("user:{owner}"));
        assert_eq!(text, "what is the plan?");
        assert_eq!(message_id, &user_msg);
        assert!(ts.is_some());
        state.db.matrix_outbox_done(due[0].id).expect("done");
        let due = state.db.matrix_outbox_due(10).expect("due");
        let Job::Message { sender, text, .. } = &due[0].job else {
            panic!("message job");
        };
        assert_eq!(sender, &format!("agent:{project}"));
        assert_eq!(text, "Three steps.");
    }

    #[test]
    fn empty_project_tabs_get_no_room() {
        let mut state = test_state(AuthMode::Github);
        dual(&mut state);
        let (owner, _) = person(&state, 2002);
        project_of(&state, &owner, "Empty");
        let rep = run(
            &state,
            Scope {
                user_id: Some(owner),
                ..Default::default()
            },
        );
        assert_eq!(rep.rooms_queued, 0);
        assert_eq!(rep.skipped_empty, 1);
    }
}
