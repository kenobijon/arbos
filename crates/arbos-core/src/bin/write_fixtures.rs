//! One-shot writer for `crates/arbos-core/fixtures/` golden files.
//!
//! ```text
//! cargo run -p arbos-core --bin write_fixtures
//! ```

use std::fs;
use std::path::PathBuf;

use arbos_core::doc::{ChatDoc, MessageStatus, UserMessageOpts};
use arbos_core::wire::{Ack, BoxState, Frame, FrameType, Hello, PushHeader, RowHeader, State};
use arbos_core::PROTO_VERSION;
use loro::{Container, LoroList, LoroMap, LoroText, ValueOrContainer};
use serde_json::json;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn write_frames(dir: &std::path::Path) {
    let hello = Frame::new(
        FrameType::Hello,
        serde_json::to_value(Hello {
            cursor: 0,
            device: "dev:fixture".into(),
            room: "proj/chat".into(),
            proto: PROTO_VERSION,
        })
        .unwrap(),
        Vec::new(),
    );
    let state = Frame::new(
        FrameType::State,
        serde_json::to_value(State {
            head_seq: 3,
            seq_floor: 0,
            checkpoint_seq: 0,
            checkpoint_size: 0,
            row_count: 0,
            row_bytes: 0,
            box_state: BoxState::Awake,
        })
        .unwrap(),
        Vec::new(),
    );
    let row = Frame::new(
        FrameType::Row,
        serde_json::to_value(RowHeader {
            seq: 1,
            device: "dev:fixture".into(),
            batch_id: "batch-row-1".into(),
        })
        .unwrap(),
        b"row-payload".to_vec(),
    );
    let rows_done = Frame::new(FrameType::RowsDone, json!({ "headSeq": 3 }), Vec::new());
    let push = Frame::new(
        FrameType::Push,
        serde_json::to_value(PushHeader {
            batch_id: "batch-push-1".into(),
        })
        .unwrap(),
        b"push-payload".to_vec(),
    );
    let ack = Frame::new(
        FrameType::Ack,
        serde_json::to_value(Ack {
            batch_id: "batch-push-1".into(),
            seq: 2,
            dup: false,
        })
        .unwrap(),
        Vec::new(),
    );

    for (name, frame) in [
        ("frame_hello.bin", hello),
        ("frame_state.bin", state),
        ("frame_row.bin", row),
        ("frame_rows_done.bin", rows_done),
        ("frame_push.bin", push),
        ("frame_ack.bin", ack),
    ] {
        fs::write(dir.join(name), frame.encode()).expect(name);
    }
}

fn write_chat_doc(dir: &std::path::Path) {
    // Fixed peer so regenerations of the same logical content stay closer.
    let mut doc = ChatDoc::new(Some(42), false);
    doc.init_meta("chat-fixture", "proj-fixture", 1);

    doc.transact(|d| {
        d.append_user_message(
            "msg-user",
            "hello from user",
            "dev:fixture",
            &UserMessageOpts {
                sender: Some("user:ken".into()),
                ..UserMessageOpts::default()
            },
        )
        .unwrap();
        // Flat reactions on the user row (`emoji|sender` → true).
        d.set_reaction("msg-user", "👍", "user:ken", true).unwrap();
    });

    let part_id = doc.start_assistant_message("msg-assistant", "msg-user", "dev:fixture");
    doc.append_text("msg-assistant", &part_id, "assistant says hi");
    doc.finish_message("msg-assistant", MessageStatus::Done);

    doc.transact(|d| {
        d.append_system_line(
            "msg-system",
            "Sam joined the call",
            "dev:fixture",
            Some("call-1"),
        )
        .unwrap();
        d.append_greeting(
            "msg-greeting",
            "Hi, I'm Arbos.",
            "dev:fixture",
            "proj-fixture",
            "Arbos",
        )
        .unwrap();
        d.append_user_answer(
            "msg-answer",
            "42",
            "q-1",
            &json!("forty-two"),
            "dev:fixture",
        );
    });

    // Legacy nested emoji-bucket reactions on a dedicated row (no flat keys).
    {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("legacy row");
        m.insert("id", "msg-legacy-react").ok();
        m.insert("role", "user").ok();
        m.insert("sender", "user:legacy").ok();
        m.insert("status", "done").ok();
        m.insert("deviceId", "dev:fixture").ok();
        m.insert("createdAt", 1_700_000_000_000i64).ok();
        let reactions = m
            .insert_container("reactions", LoroMap::new())
            .expect("reactions");
        let bucket = reactions
            .insert_container("🎉", LoroMap::new())
            .expect("bucket");
        bucket.insert("user:legacy", true).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", "part-legacy").ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, "legacy reactions row").ok();
        doc.doc.commit();
    }

    let snap = doc.snapshot();
    fs::write(dir.join("chat_doc.bin"), &snap).expect("chat_doc.bin");

    // Side-car expected shape (kinds / text / projected reactions) for the Rust
    // test and the upcoming Swift check.
    let list = doc.messages_list();
    let mut expected = Vec::new();
    for i in 0..list.len() {
        let Some(ValueOrContainer::Container(Container::Map(m))) = list.get(i) else {
            continue;
        };
        let mut json = serde_json::to_value(m.get_deep_value()).unwrap();
        arbos_core::doc::project_hidden_parts(&mut json);
        arbos_core::doc::project_reactions(&mut json);
        expected.push(json!({
            "id": json["id"],
            "role": json["role"],
            "text": message_text(&json),
            "greeting": json.get("greeting").cloned().unwrap_or(json!(null)),
            "answersQuestionId": json.get("answersQuestionId").cloned().unwrap_or(json!(null)),
            "reactions": json.get("reactions").cloned().unwrap_or(json!({})),
        }));
    }
    fs::write(
        dir.join("chat_doc_expected.json"),
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .expect("expected json");
}

fn message_text(row: &serde_json::Value) -> String {
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

fn main() {
    let dir = fixtures_dir();
    fs::create_dir_all(&dir).expect("fixtures dir");
    write_frames(&dir);
    write_chat_doc(&dir);
    eprintln!("wrote fixtures to {}", dir.display());
}
