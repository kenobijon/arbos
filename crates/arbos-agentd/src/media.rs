//! Media and document delivery (F10, D38): `deliver_file` uploads a file the agent produced to the
//! relay blob store and appends an `attachment` part to the current assistant message;
//! `generate_image` asks the relay's image proxy (the OpenAI key stays on the relay, D2), saves
//! the PNG under the project's `out/` folder and delivers it the same way.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::agent::Agent;
use crate::config::Config;
use crate::mcp::{err_result, text_result};

/// The relay accepts 64 MiB bodies; stay under it.
const MAX_BYTES: u64 = 60 * 1024 * 1024;
const DEFAULT_IMAGE_MODEL: &str = "gpt-image-1";

/// One line in the workspace's `.arbos/audit.jsonl` (the file the pi extension and the approval gate write).
fn audit(agent: &Agent, kind: &str, fields: Value) {
    let mut entry = json!({ "kind": kind, "at": now_ms() });
    if let (Some(e), Some(f)) = (entry.as_object_mut(), fields.as_object()) {
        for (k, v) in f {
            e.insert(k.clone(), v.clone());
        }
    }
    let path = agent.cfg.audit_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{entry}");
    }
}

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({ "name": "deliver_file", "description": "Hand a file you produced to the user as a downloadable attachment card in the chat (slides, documents, spreadsheets, PDFs, images, video, audio, archives). Write the file under the out/ folder of your project folder first, then call this with its path. kind is inferred from the extension when omitted.",
                "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Absolute path, or relative to your project folder" }, "title": { "type": "string", "description": "Short caption shown on the card (defaults to the file name)" }, "kind": { "type": "string", "enum": ["image", "video", "slides", "doc", "sheet", "pdf", "audio", "file"] }, "session_id": { "type": "string", "description": "Subagents: your ARBOS_SESSION_ID" } }, "required": ["path"] } }),
        json!({ "name": "generate_image", "description": "Generate an image from a text prompt (through the relay; no key needed here). Saves out/<id>.png in your project folder and delivers it to the user as an attachment card. One call per image.",
                "inputSchema": { "type": "object", "properties": { "prompt": { "type": "string" }, "size": { "type": "string", "enum": ["1024x1024", "1024x1536", "1536x1024"], "description": "default 1024x1024" }, "quality": { "type": "string", "enum": ["low", "medium", "high"], "description": "default medium" }, "title": { "type": "string" }, "session_id": { "type": "string" } }, "required": ["prompt"] } }),
    ]
}

/// `<project folder>/out`: where the agent writes files meant for the user.
pub fn out_dir(cfg: &Config) -> PathBuf {
    cfg.work_dir.join("out")
}

/// Python interpreter that has python-pptx/docx/openpyxl/Pillow/reportlab: the venv named by
/// `ARBOS_MEDIA_VENV` (the image sets `/opt/prime-venv`; `scripts/box-media-deps.sh` for a
/// process box), else plain `python3`.
pub fn media_python() -> String {
    match std::env::var("ARBOS_MEDIA_VENV") {
        Ok(v) if !v.trim().is_empty() => format!("{}/bin/python", v.trim_end_matches('/')),
        _ => "python3".to_owned(),
    }
}

/// "## Producing files" for the main-agent prompt and the subagent pack.
pub fn prompt_section(cfg: &Config) -> String {
    format!(
        "## Producing files\n\nWhen the user wants a document, slides, a spreadsheet, a PDF, an image or a video, make the file and hand it over with `deliver_file(path, title?)`: it appears in the chat as a card with a download button. Write files under `{out}/` with a clear name (`report.pptx`, `budget.xlsx`). Python at `{py}` has python-pptx, python-docx, openpyxl, Pillow and reportlab; `ffmpeg` is on PATH for video and audio. For a picture from a description use `generate_image(prompt, size?)`; it saves and delivers the PNG itself. After delivering, say in one line what the file contains; never paste its contents or the path unless asked.\n\n",
        out = out_dir(cfg).display(),
        py = media_python()
    )
}

/// Attachment kind from the extension (or the MIME type when the extension says nothing).
pub fn kind_for(name: &str, mime: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "heic" | "tiff" => "image",
        "mp4" | "mov" | "webm" | "mkv" | "avi" => "video",
        "pptx" | "ppt" | "key" | "odp" => "slides",
        "docx" | "doc" | "odt" | "rtf" | "md" | "txt" | "html" => "doc",
        "xlsx" | "xls" | "csv" | "ods" | "tsv" => "sheet",
        "pdf" => "pdf",
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "aac" => "audio",
        _ => {
            if mime.starts_with("image/") {
                "image"
            } else if mime.starts_with("video/") {
                "video"
            } else if mime.starts_with("audio/") {
                "audio"
            } else if mime == "application/pdf" {
                "pdf"
            } else {
                "file"
            }
        }
    }
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppt" => "application/vnd.ms-powerpoint",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "html" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

const KINDS: [&str; 8] = [
    "image", "video", "slides", "doc", "sheet", "pdf", "audio", "file",
];

/// Where the card goes: the calling subagent's progress message, else the running main turn's
/// assistant message, else the newest running subagent.
async fn target(a: &Agent, session_id: Option<&str>) -> Option<(String, String)> {
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        if let Some(t) = a.subagent_msgs.lock().await.get(sid) {
            return Some(t.clone());
        }
    }
    {
        let mut tl = a.turn.lock().await;
        if let Some(t) = tl.as_mut() {
            // The next prose opens a new text part so the card keeps its place in the message.
            t.text_part_dirty = true;
            return Some((t.chat_id.clone(), t.assistant_msg_id.clone()));
        }
    }
    a.subagent_msgs
        .lock()
        .await
        .values()
        .max_by_key(|(_, msg)| msg.clone())
        .cloned()
}

async fn upload(
    a: &Arc<Agent>,
    chat_id: &str,
    bytes: Vec<u8>,
    mime: &str,
) -> anyhow::Result<String> {
    use sha2::Digest;
    let key = hex::encode(sha2::Sha256::digest(&bytes));
    // Same slot clients read: people chats live under `people:{chat}`, not the project id.
    let slot = a.blob_slot(chat_id);
    let r = a
        .http_client()
        .put(format!("{}/blob/{}/{}", a.cfg.relay_url, slot, key))
        .bearer_auth(&a.cfg.box_token)
        .header("content-type", mime)
        .body(bytes)
        .send()
        .await?;
    if !r.status().is_success() {
        anyhow::bail!("relay refused the upload: HTTP {}", r.status());
    }
    Ok(key)
}

/// Resolve, read, upload and append the part. Returns the attachment JSON that landed in the doc.
pub async fn deliver(
    a: &Arc<Agent>,
    path: &str,
    title: Option<&str>,
    kind: Option<&str>,
    session_id: Option<&str>,
) -> anyhow::Result<Value> {
    let p = Path::new(path);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        a.cfg.work_dir.join(p)
    };
    let meta = tokio::fs::metadata(&p)
        .await
        .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
    if !meta.is_file() {
        anyhow::bail!("{} is not a file", p.display());
    }
    if meta.len() > MAX_BYTES {
        anyhow::bail!(
            "{} is {} MiB; the limit is {} MiB (compress or split it)",
            p.display(),
            meta.len() / (1024 * 1024),
            MAX_BYTES / (1024 * 1024)
        );
    }
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_owned();
    let mime = mime_for(&name);
    let kind = match kind.filter(|k| KINDS.contains(k)) {
        Some(k) => k.to_owned(),
        None => kind_for(&name, mime).to_owned(),
    };
    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&name)
        .to_owned();
    let Some((chat_id, msg_id)) = target(a, session_id).await else {
        anyhow::bail!("no running turn to attach the file to");
    };
    let bytes = tokio::fs::read(&p).await?;
    let size = bytes.len() as u64;
    let key = upload(a, &chat_id, bytes, mime).await?;
    let attachment = json!({
        "name": name, "mime": mime, "ref": key, "bytes": size, "kind": kind, "title": title,
    });
    let Some(host) = a.sync.host(&chat_id).await else {
        anyhow::bail!("chat {chat_id} is not open on this box");
    };
    {
        let h = host.lock().await;
        if h.doc.add_attachment_part(&msg_id, &attachment).is_none() {
            anyhow::bail!("message {msg_id} not found in chat {chat_id}");
        }
    }
    let _ = a.store.t3_append(
        &chat_id,
        None,
        session_id,
        "attachment",
        Some("assistant"),
        &format!("delivered {name} ({size} bytes, {kind}) as blob {key}"),
        Some(&json!({ "message_id": msg_id, "path": p.display().to_string(), "attachment": attachment })),
    );
    audit(
        a,
        "file_delivered",
        json!({ "chatId": chat_id, "messageId": msg_id, "name": name, "bytes": size, "kind": kind, "ref": key, "path": p.display().to_string() }),
    );
    Ok(attachment)
}

pub async fn deliver_file(a: &Arc<Agent>, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let Some(path) = s("path").filter(|p| !p.trim().is_empty()) else {
        return err_result("path is required");
    };
    match deliver(a, path.trim(), s("title"), s("kind"), s("session_id")).await {
        Ok(att) => text_result(
            json!({ "delivered": true, "name": att["name"], "kind": att["kind"], "bytes": att["bytes"], "ref": att["ref"], "note": "The user sees a card with a download button. Describe the file in one line; do not repeat the path." })
                .to_string(),
        ),
        Err(e) => err_result(e),
    }
}

pub async fn generate_image(a: &Arc<Agent>, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let Some(prompt) = s("prompt").map(str::trim).filter(|p| !p.is_empty()) else {
        return err_result("prompt is required");
    };
    let size = s("size").unwrap_or("1024x1024");
    let quality = s("quality").unwrap_or("medium");
    let model = std::env::var("ARBOS_IMAGE_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_IMAGE_MODEL.to_owned());
    let body =
        json!({ "model": model, "prompt": prompt, "size": size, "quality": quality, "n": 1 });
    let t0 = std::time::Instant::now();
    let resp = match a
        .http_client()
        .post(format!("{}/proxy/v1/images/generations", a.cfg.relay_url))
        .bearer_auth(&a.cfg.box_token)
        .header("x-arbos-project", &a.cfg.project_id)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err_result(format!("relay: {e}")),
    };
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("image generation failed");
        return err_result(format!("HTTP {status}: {msg}"));
    }
    let mock = v
        .get("arbos_mock")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(b64) = v.pointer("/data/0/b64_json").and_then(Value::as_str) else {
        return err_result("relay returned no image data");
    };
    let png = match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(b) => b,
        Err(e) => return err_result(format!("bad image payload: {e}")),
    };
    let dir = out_dir(&a.cfg);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err_result(format!("{}: {e}", dir.display()));
    }
    let path = dir.join(format!("{}.png", ulid().to_lowercase()));
    if let Err(e) = tokio::fs::write(&path, &png).await {
        return err_result(format!("{}: {e}", path.display()));
    }
    let title = s("title")
        .map(str::to_owned)
        .unwrap_or_else(|| prompt.chars().take(80).collect());
    audit(
        a,
        "image_generated",
        json!({ "prompt": prompt, "size": size, "quality": quality, "model": model, "mock": mock, "path": path.display().to_string(), "ms": t0.elapsed().as_millis() as i64, "at": now_ms() }),
    );
    match deliver(
        a,
        &path.display().to_string(),
        Some(&title),
        Some("image"),
        s("session_id"),
    )
    .await
    {
        Ok(att) => text_result(
            json!({ "delivered": true, "path": path.display().to_string(), "bytes": att["bytes"], "ref": att["ref"], "mock": mock, "note": if mock { "ARBOS_IMAGE_MOCK=1: the relay drew a labelled placeholder instead of calling the provider." } else { "The user sees the image inline with a download button." } })
                .to_string(),
        ),
        Err(e) => err_result(format!(
            "image saved to {} but not delivered: {e}",
            path.display()
        )),
    }
}
