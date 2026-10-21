//! Decode checked-in `fixtures/` snapshots: chat doc rows and wire frames.

use std::path::PathBuf;

use arbos_core::doc::{project_hidden_parts, project_reactions, ChatDoc};
use arbos_core::wire::{Ack, Frame, FrameType, Hello, PushHeader, RowHeader, State};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn message_text(row: &Value) -> String {
    row.get("parts")
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("kind").and_then(|k| k.as_str()) == Some("text"))
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[test]
fn fixtures_chat_doc_and_frames() {
    let mut doc = ChatDoc::new(Some(99), false);
    doc.import(&load("chat_doc.bin")).unwrap();

    let expected: Vec<Value> = serde_json::from_slice(&load("chat_doc_expected.json")).unwrap();
    let messages = doc.to_json()["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(messages.len(), expected.len());

    for (i, exp) in expected.iter().enumerate() {
        let mut json = messages[i].clone();
        project_hidden_parts(&mut json);
        project_reactions(&mut json);
        assert_eq!(json["id"], exp["id"], "row {i} id");
        assert_eq!(json["role"], exp["role"], "row {i} role");
        assert_eq!(message_text(&json), exp["text"], "row {i} text");
        if !exp["greeting"].is_null() {
            assert_eq!(json["greeting"], exp["greeting"], "row {i} greeting");
        }
        if !exp["answersQuestionId"].is_null() {
            assert_eq!(
                json["answersQuestionId"], exp["answersQuestionId"],
                "row {i} answersQuestionId"
            );
        }
        assert_eq!(json["reactions"], exp["reactions"], "row {i} reactions");
    }

    // Spot-check kinds the fixture was built to cover.
    assert_eq!(expected[0]["id"], "msg-user");
    assert_eq!(expected[0]["reactions"]["👍"]["user:ken"], true);
    assert_eq!(expected[1]["id"], "msg-assistant");
    assert_eq!(expected[1]["role"], "assistant");
    assert_eq!(expected[2]["id"], "msg-system");
    assert_eq!(expected[3]["greeting"], true);
    assert_eq!(expected[4]["answersQuestionId"], "q-1");
    assert_eq!(expected[5]["reactions"]["🎉"]["user:legacy"], true);

    let hello = Frame::decode(load("frame_hello.bin").into())
        .unwrap()
        .expect("hello");
    assert_eq!(hello.ty, FrameType::Hello);
    let h: Hello = serde_json::from_value(hello.header).unwrap();
    assert_eq!(h.device, "dev:fixture");
    assert_eq!(h.room, "proj/chat");
    assert_eq!(h.cursor, 0);

    let state = Frame::decode(load("frame_state.bin").into())
        .unwrap()
        .expect("state");
    assert_eq!(state.ty, FrameType::State);
    let s: State = serde_json::from_value(state.header).unwrap();
    assert_eq!(s.head_seq, 3);
    assert_eq!(s.box_state, arbos_core::wire::BoxState::Awake);

    let row = Frame::decode(load("frame_row.bin").into())
        .unwrap()
        .expect("row");
    assert_eq!(row.ty, FrameType::Row);
    let rh: RowHeader = serde_json::from_value(row.header).unwrap();
    assert_eq!(rh.seq, 1);
    assert_eq!(rh.batch_id, "batch-row-1");
    assert_eq!(row.payload.as_ref(), b"row-payload");

    let done = Frame::decode(load("frame_rows_done.bin").into())
        .unwrap()
        .expect("rows_done");
    assert_eq!(done.ty, FrameType::RowsDone);
    assert_eq!(done.header["headSeq"], 3);

    let push = Frame::decode(load("frame_push.bin").into())
        .unwrap()
        .expect("push");
    assert_eq!(push.ty, FrameType::Push);
    let ph: PushHeader = serde_json::from_value(push.header).unwrap();
    assert_eq!(ph.batch_id, "batch-push-1");
    assert_eq!(push.payload.as_ref(), b"push-payload");

    let ack = Frame::decode(load("frame_ack.bin").into())
        .unwrap()
        .expect("ack");
    assert_eq!(ack.ty, FrameType::Ack);
    let a: Ack = serde_json::from_value(ack.header).unwrap();
    assert_eq!(a.batch_id, "batch-push-1");
    assert_eq!(a.seq, 2);
}
