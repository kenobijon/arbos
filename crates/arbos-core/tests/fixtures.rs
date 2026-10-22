//! Decode checked-in `fixtures/` snapshots: chat doc rows and wire frames.

use std::path::PathBuf;

use arbos_core::doc::{message_text, project_reactions, ChatDoc};
use arbos_core::wire::{Ack, Frame, FrameType, Hello, PushHeader, RowHeader, State};
use serde_json::{json, Value};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load(name: &str) -> Vec<u8> {
    std::fs::read(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

#[test]
fn fixtures_chat_doc_and_frames() {
    let mut doc = ChatDoc::new(Some(99), false);
    doc.import(&load("chat_doc.bin")).unwrap();

    let expected: Vec<Value> = serde_json::from_slice(&load("chat_doc_expected.json")).unwrap();
    let messages = doc.projected_rows();
    assert_eq!(messages.len(), expected.len());

    for (i, exp) in expected.iter().enumerate() {
        let json = &messages[i];
        assert_eq!(json["id"], exp["id"], "row {i} id");
        assert_eq!(json["role"], exp["role"], "row {i} role");
        assert_eq!(message_text(json), exp["text"], "row {i} text");
        if !exp["greeting"].is_null() {
            assert_eq!(json["greeting"], exp["greeting"], "row {i} greeting");
        }
        if !exp["answersQuestionId"].is_null() {
            assert_eq!(
                json["answersQuestionId"], exp["answersQuestionId"],
                "row {i} answersQuestionId"
            );
        }
        let hidden_count = json
            .get("hiddenParts")
            .and_then(|h| h.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(hidden_count, exp["hiddenCount"], "row {i} hiddenCount");
        assert_eq!(json["reactions"], exp["reactions"], "row {i} reactions");
    }

    // Spot-check kinds the fixture was built to cover.
    assert_eq!(expected[0]["id"], "msg-user");
    assert_eq!(expected[0]["reactions"]["👍"]["user:ken"], true);
    assert_eq!(expected[0]["hiddenCount"], 0);
    assert_eq!(expected[1]["id"], "msg-assistant");
    assert_eq!(expected[1]["role"], "assistant");
    assert_eq!(expected[1]["hiddenCount"], 1);
    assert_eq!(expected[2]["id"], "msg-system");
    assert_eq!(expected[3]["greeting"], true);
    assert_eq!(expected[4]["answersQuestionId"], "q-1");
    assert_eq!(expected[5]["reactions"]["🎉"]["user:legacy"], true);

    assert_frames();
}

/// Older shapes readers still accept (step 9b-1): Swift-client placeholders, no reactions
/// map, turn string/object, flat attachment, plain-string text, status+question.
#[test]
fn fixtures_chat_doc_legacy_still_read() {
    let mut doc = ChatDoc::new(Some(97), false);
    doc.import(&load("chat_doc_legacy.bin")).unwrap();

    let expected: Vec<Value> =
        serde_json::from_slice(&load("chat_doc_legacy_expected.json")).unwrap();
    let messages = doc.projected_rows();
    assert_eq!(messages.len(), expected.len());
    assert_eq!(expected.len(), 7);

    for (i, exp) in expected.iter().enumerate() {
        let json = &messages[i];
        assert_eq!(json["id"], exp["id"], "row {i} id");
        assert_eq!(json["role"], exp["role"], "row {i} role");
        assert_eq!(message_text(json), exp["text"], "row {i} text");
        let hidden_count = json
            .get("hiddenParts")
            .and_then(|h| h.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(hidden_count, exp["hiddenCount"], "row {i} hiddenCount");
        let got_reactions = json.get("reactions").cloned().unwrap_or(json!({}));
        assert_eq!(got_reactions, exp["reactions"], "row {i} reactions");
    }

    assert_eq!(expected[0]["id"], "msg-ios-user");
    assert_eq!(expected[0]["text"], "hello from ios");
    let ios = &messages[0];
    assert_eq!(ios["replyTo"], "m0");
    assert_eq!(ios["mentions"][0], "agent:p1");
    assert_eq!(ios["parts"][0]["attachment"]["name"], "pic.png");

    assert_eq!(expected[1]["id"], "msg-no-reactions");
    assert!(messages[1].get("reactions").is_none());

    assert_eq!(messages[2]["turn"], "reply");
    assert_eq!(messages[3]["turn"]["kind"], "reply");
    assert_eq!(messages[4]["parts"][0]["name"], "flat.png");
    assert!(messages[4]["parts"][0].get("attachment").is_none());
    assert_eq!(expected[5]["text"], "plain string text body");
    assert_eq!(messages[6]["parts"][0]["kind"], "question");
    assert_eq!(messages[6]["parts"][1]["code"], "waiting_user");

    // set_reaction on a pre-0.6.64 row (no map) creates the map; toggle off clears it.
    doc.set_reaction("msg-no-reactions", "👍", "user:x", true)
        .unwrap();
    let mut row = doc.to_json()["messages"][1].clone();
    project_reactions(&mut row);
    assert_eq!(row["reactions"]["👍"]["user:x"], true);
    doc.set_reaction("msg-no-reactions", "👍", "user:x", false)
        .unwrap();
    let mut row = doc.to_json()["messages"][1].clone();
    project_reactions(&mut row);
    let empty = row
        .get("reactions")
        .and_then(|r| r.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(true);
    assert!(empty, "toggle off clears the reaction");

    // Nested emoji-bucket toggle (legacy reaction shape already in chat_doc.bin).
    let mut nested = ChatDoc::new(Some(96), false);
    nested.import(&load("chat_doc.bin")).unwrap();
    let mut before = nested.to_json()["messages"][5].clone();
    project_reactions(&mut before);
    assert_eq!(before["reactions"]["🎉"]["user:legacy"], true);
    nested
        .set_reaction("msg-legacy-react", "🎉", "user:legacy", false)
        .unwrap();
    let mut after = nested.to_json()["messages"][5].clone();
    project_reactions(&mut after);
    let cleared = after
        .get("reactions")
        .and_then(|r| r.as_object())
        .and_then(|o| o.get("🎉"))
        .and_then(|b| b.as_object())
        .map(|b| b.is_empty())
        .unwrap_or(true);
    assert!(cleared, "legacy nested reaction toggle clears the sender");
}

/// Rows written by the Mac fixture's old `insert_message` (full serde `Message`, with
/// `chatId` / `pending` / `hiddenParts` placeholders) must still read through core.
#[test]
fn fixtures_mac_placeholder_rows_still_read() {
    let mut doc = ChatDoc::new(Some(98), false);
    doc.import(&load("mac_placeholder_rows.bin")).unwrap();

    let expected: Vec<Value> =
        serde_json::from_slice(&load("mac_placeholder_rows_expected.json")).unwrap();
    assert_eq!(expected.len(), 2);
    let rows = doc.projected_rows();
    assert_eq!(rows.len(), expected.len());
    let views = doc.messages();
    assert_eq!(views.len(), expected.len());

    let raw = doc.to_json();
    let raw_rows = raw["messages"].as_array().expect("messages list");
    for (i, exp) in expected.iter().enumerate() {
        assert_eq!(rows[i]["id"], exp["id"], "row {i} id");
        assert_eq!(rows[i]["role"], exp["role"], "row {i} role");
        assert_eq!(message_text(&rows[i]), exp["text"], "row {i} text");
        assert_eq!(views[i].id, exp["id"].as_str().unwrap(), "row {i} view id");
        assert_eq!(
            views[i].text(),
            exp["text"].as_str().unwrap(),
            "row {i} view text"
        );

        let obj = raw_rows[i].as_object().expect("row object");
        assert_eq!(obj["chatId"], "chat-mac-legacy", "row {i} chatId");
        assert_eq!(obj["pending"], false, "row {i} pending");
        assert_eq!(
            obj["hiddenParts"],
            Value::Array(Vec::new()),
            "row {i} hiddenParts"
        );
    }
    assert_eq!(doc.meta()["chatId"], "chat-mac-legacy");
}

fn assert_frames() {
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
