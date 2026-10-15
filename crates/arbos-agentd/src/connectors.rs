//! Connector tools (F1): `connectors_list` and `connector_call`. The box never holds a
//! service token; every call goes to the relay broker (`POST /connectors/call`), which signs
//! the upstream request and returns a redacted reply. Actions of kind `write`/`send` pass
//! through the approval gate first.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::agent::Agent;
use crate::mcp::{err_result, materialize_handle, text_result};

const CATALOG_TTL: Duration = Duration::from_secs(60);
const INLINE_MAX: usize = 16_000;

// ---- approval stub -------------------------------------------------------------------------
// TODO(feat-approve): replace with agent::approval (`Agent::request_approval`, same shapes).

/// What the user is asked to approve (F2 shape: title, summary, risk, payload).
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub title: String,
    pub summary: String,
    /// `send | destructive | money | credential | computer | other`.
    pub risk: &'static str,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Rejected { reason: String },
    Expired,
}

impl ApprovalOutcome {
    fn state(&self) -> &'static str {
        match self {
            ApprovalOutcome::Approved => "approved",
            ApprovalOutcome::Rejected { .. } => "rejected",
            ApprovalOutcome::Expired => "expired",
        }
    }
}

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

/// Stub gate until the approve feature lands: approves only when `ARBOS_APPROVAL_STUB=auto`,
/// otherwise rejects. Writes the audit entries and an `approval` part (hidden) so the
/// decision is visible in the Details drawer.
pub async fn request_approval(
    agent: &Agent,
    chat_id: &str,
    req: ApprovalRequest,
) -> ApprovalOutcome {
    let approval_id = ulid();
    audit(
        agent,
        "approval_requested",
        json!({ "approvalId": approval_id, "chatId": chat_id, "title": req.title, "risk": req.risk, "payload": req.payload }),
    );
    let outcome = match std::env::var("ARBOS_APPROVAL_STUB").as_deref() {
        Ok("auto") => ApprovalOutcome::Approved,
        Ok("expire") => ApprovalOutcome::Expired,
        _ => ApprovalOutcome::Rejected {
            reason: "approval gate not available yet (feature approve not merged); set ARBOS_APPROVAL_STUB=auto to auto-approve in development".into(),
        },
    };
    audit(
        agent,
        "approval_decided",
        json!({ "approvalId": approval_id, "chatId": chat_id, "state": outcome.state(), "decidedBy": "stub" }),
    );
    let msg_id = agent
        .turn
        .lock()
        .await
        .as_ref()
        .filter(|t| t.chat_id == chat_id)
        .map(|t| t.assistant_msg_id.clone());
    if let Some(msg_id) = msg_id {
        if let Some(host) = agent.sync.host(chat_id).await {
            let mut h = host.lock().await;
            h.doc.add_hidden_part(
                &msg_id,
                "approval",
                &req.summary,
                &json!({
                    "approvalId": approval_id, "title": req.title, "summary": req.summary, "risk": req.risk,
                    "payload": req.payload, "state": outcome.state(), "decidedAt": now_ms(), "decidedBy": "stub",
                }),
            );
        }
    }
    outcome
}

// ---- tools ---------------------------------------------------------------------------------

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({ "name": "connectors_list", "description": "External accounts the user connected (Gmail/Calendar, Slack, GitHub, Harness Comms) with the actions each one offers and their kind (read | write | send). write/send actions ask the user for approval.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "connector_call", "description": "Act on an external service as the user through the relay (the box never sees a token). provider and action come from connectors_list; args follow that action's schema. Reads run at once; write/send actions wait for the user's approval and fail when it is refused.",
                "inputSchema": { "type": "object", "properties": { "provider": { "type": "string" }, "action": { "type": "string" }, "args": { "type": "object" } }, "required": ["provider", "action"] } }),
    ]
}

async fn relay_get(agent: &Agent, path: &str) -> Result<Value, String> {
    let r = agent
        .http_client()
        .get(format!("{}{}", agent.cfg.relay_url, path))
        .bearer_auth(&agent.cfg.box_token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("relay {path}: HTTP {status} {text}"));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn catalog_cache() -> &'static Mutex<Option<(Instant, Value)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

async fn catalog(agent: &Agent) -> Result<Value, String> {
    if let Some((at, v)) = catalog_cache().lock().expect("catalog lock").as_ref() {
        if at.elapsed() < CATALOG_TTL {
            return Ok(v.clone());
        }
    }
    let v = relay_get(agent, "/connectors/catalog").await?;
    *catalog_cache().lock().expect("catalog lock") = Some((Instant::now(), v.clone()));
    Ok(v)
}

fn action_spec<'a>(catalog: &'a Value, provider: &str, action: &str) -> Option<&'a Value> {
    catalog["providers"]
        .as_array()?
        .iter()
        .find(|p| p["provider"].as_str() == Some(provider))?["actions"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(action))
}

pub async fn connectors_list(agent: &Agent) -> Value {
    let (cat, list) = match (catalog(agent).await, relay_get(agent, "/connectors").await) {
        (Ok(c), Ok(l)) => (c, l),
        (Err(e), _) | (_, Err(e)) => return err_result(e),
    };
    let items = list["items"].as_array().cloned().unwrap_or_default();
    let mut connected = Vec::new();
    let mut available = Vec::new();
    for p in cat["providers"].as_array().cloned().unwrap_or_default() {
        let name = p["provider"].as_str().unwrap_or("").to_owned();
        let cn = items
            .iter()
            .find(|c| c["provider"].as_str() == Some(name.as_str()));
        let actions: Vec<Value> = p["actions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| json!({ "name": x["name"], "kind": x["kind"], "description": x["description"], "args": x["args"]["properties"], "required": x["args"]["required"] }))
                    .collect()
            })
            .unwrap_or_default();
        match cn {
            Some(c) if c["status"] == "connected" => connected.push(json!({
                "provider": name, "label": c["label"], "status": c["status"], "mock": c["mock"], "actions": actions,
            })),
            Some(c) => available.push(json!({ "provider": name, "label": p["label"], "status": c["status"], "note": c["last_error"] })),
            None => available.push(json!({ "provider": name, "label": p["label"], "status": "not_connected" })),
        }
    }
    text_result(
        json!({
            "mock": list["mock"],
            "connected": connected,
            "not_connected": available,
            "note": "write/send actions ask the user for approval first. Connect accounts in Arbos → Settings → Connectors.",
        })
        .to_string(),
    )
}

pub async fn connector_call(agent: &Agent, args: &Value, chat_id: &str) -> Value {
    let provider = args["provider"].as_str().unwrap_or("").trim().to_owned();
    let action = args["action"].as_str().unwrap_or("").trim().to_owned();
    let call_args = match args.get("args") {
        Some(Value::Object(o)) => Value::Object(o.clone()),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        _ => json!({}),
    };
    if provider.is_empty() || action.is_empty() {
        return err_result("provider and action are required (see connectors_list)");
    }
    let cat = match catalog(agent).await {
        Ok(c) => c,
        Err(e) => return err_result(e),
    };
    let Some(spec) = action_spec(&cat, &provider, &action) else {
        return err_result(format!(
            "unknown action {provider}.{action}; call connectors_list for the catalog"
        ));
    };
    let kind = spec["kind"].as_str().unwrap_or("read");
    if matches!(kind, "write" | "send") {
        let outcome = request_approval(
            agent,
            chat_id,
            ApprovalRequest {
                title: format!("{provider}: {action}"),
                summary: summarize(&provider, &action, &call_args),
                risk: "send",
                payload: json!({ "provider": provider, "action": action, "args": call_args }),
            },
        )
        .await;
        match outcome {
            ApprovalOutcome::Approved => {}
            ApprovalOutcome::Rejected { reason } => {
                return err_result(format!("{provider}.{action} not approved: {reason}"));
            }
            ApprovalOutcome::Expired => {
                return err_result(format!("{provider}.{action}: approval expired"));
            }
        }
    }
    let _ = agent.store.t3_append(
        chat_id,
        None,
        Some(chat_id),
        "connector_call",
        Some("assistant"),
        &format!("{provider}.{action} {}", clip(&call_args.to_string(), 400)),
        None,
    );
    let r = agent
        .http_client()
        .post(format!("{}/connectors/call", agent.cfg.relay_url))
        .bearer_auth(&agent.cfg.box_token)
        .json(&json!({ "provider": provider, "action": action, "args": call_args }))
        .send()
        .await;
    let (status, text) = match r {
        Ok(r) => {
            let st = r.status();
            (st, r.text().await.unwrap_or_default())
        }
        Err(e) => return err_result(format!("relay unreachable: {e}")),
    };
    if !status.is_success() {
        return err_result(format!("{provider}.{action}: {text}"));
    }
    if text.len() > INLINE_MAX {
        let handle = materialize_handle(
            agent,
            chat_id,
            &format!("{provider}.{action} result"),
            &text,
        );
        return text_result(
            json!({ "handle": handle, "preview": clip(&text, 4000), "note": "full result by handle (peek)" })
                .to_string(),
        );
    }
    text_result(text)
}

fn summarize(provider: &str, action: &str, args: &Value) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or("").to_owned();
    match (provider, action) {
        ("google", "send") | ("google", "create_draft") => {
            format!(
                "Email to {} — {}: {}",
                s("to"),
                s("subject"),
                clip(&s("body"), 200)
            )
        }
        ("google", "create_event") => format!(
            "Calendar event {} ({} → {})",
            s("summary"),
            s("start"),
            s("end")
        ),
        ("slack", "post_message") => {
            format!("Slack post in {}: {}", s("channel"), clip(&s("text"), 200))
        }
        ("github", "create_issue") => format!("GitHub issue in {}: {}", s("repo"), s("title")),
        ("github", "comment") => format!(
            "GitHub comment on {}#{}: {}",
            s("repo"),
            args["number"],
            clip(&s("body"), 200)
        ),
        ("harness_comms", "send") => {
            let target = if !s("chat_id").is_empty() {
                s("chat_id")
            } else {
                s("to")
            };
            format!("Message to {target}: {}", clip(&s("text"), 200))
        }
        ("harness_comms", "draft") => format!("Draft: {}", clip(&s("text"), 200)),
        _ => format!("{provider}.{action} {}", clip(&args.to_string(), 200)),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
