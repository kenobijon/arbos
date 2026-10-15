//! The project MCP server (design §6.7): Streamable-HTTP JSON-RPC on loopback.
//! Tools: history_search, history_read, read_thread, memory_search, memory_read,
//! peek, sub_llm, remember, recall, projects_list, projects_send, ask_user,
//! spawn_subagent, wake_subagent, connectors_list, connector_call.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::agent::Agent;

type S = Arc<Agent>;

const MAX_HITS: i64 = 20;
const PAGE_TOKENS_CHARS: usize = 32_000; // ≈ 8k tokens

fn tools() -> Value {
    let mut list = json!([
        { "name": "history_search", "description": "Search this Project's whole history (chats, subagent sessions, tool outputs). Ranked hits with snippets; every hit id is readable with history_read.",
          "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "thread_id": { "type": "string" }, "kind": { "type": "string", "enum": ["user","assistant","tool_call","tool_output","compaction","memory","session_entry"] }, "limit": { "type": "integer", "maximum": 20 }, "cursor": { "type": "string" } }, "required": ["query"] } },
        { "name": "history_read", "description": "Read one history entry verbatim by id, or a range {thread_id, from_id, to_id}. Big entries return a handle for peek.",
          "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "range": { "type": "object", "properties": { "thread_id": { "type": "string" }, "from_id": { "type": "string" }, "to_id": { "type": "string" } } }, "cursor": { "type": "string" } } } },
        { "name": "read_thread", "description": "Verbatim turns of one chat or subagent thread (default: last 20).",
          "inputSchema": { "type": "object", "properties": { "thread_id": { "type": "string" }, "from": { "type": "integer" }, "to": { "type": "integer" }, "cursor": { "type": "string" } }, "required": ["thread_id"] } },
        { "name": "memory_search", "description": "Search Project memory (T0 user core, T1 project core, T2 thread memory). Bi-temporal fields included.",
          "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "tiers": { "type": "array", "items": { "type": "string" } }, "include_expired": { "type": "boolean" }, "limit": { "type": "integer", "maximum": 20 } }, "required": ["query"] } },
        { "name": "memory_read", "description": "Read a thread's T2 memory, or memory records by id.",
          "inputSchema": { "type": "object", "properties": { "thread_id": { "type": "string" }, "ids": { "type": "array", "items": { "type": "string" } } } } },
        { "name": "peek", "description": "Read a slice of an offloaded output by handle: lines [from,to], chars [from,to], or grep pattern; max 200 lines.",
          "inputSchema": { "type": "object", "properties": { "handle": { "type": "string" }, "lines": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 }, "chars": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 }, "grep": { "type": "string" }, "max": { "type": "integer", "maximum": 200 } }, "required": ["handle"] } },
        { "name": "sub_llm", "description": "Run a cheap model over a handle or text with an instruction (map-reduce in 60k-char chunks). Returns <= 2,000 chars inline; the full result becomes a new handle.",
          "inputSchema": { "type": "object", "properties": { "handle": { "type": "string" }, "text": { "type": "string" }, "instruction": { "type": "string" }, "chunk": { "type": "integer" } }, "required": ["instruction"] } },
        { "name": "remember", "description": "Save a short named note in this chat's working set (a proposal to project memory). value null forgets it.",
          "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "value": { "type": ["string","null"] } }, "required": ["name"] } },
        { "name": "recall", "description": "Read a note saved with remember.", "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] } },
        { "name": "projects_list", "description": "List the user's other Projects (name, state).", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "projects_send", "description": "Send a message to another Project by name. Async: the reply lands in this Project's 'Projects ⇄' tab.",
          "inputSchema": { "type": "object", "properties": { "to": { "type": "string" }, "text": { "type": "string" }, "wait": { "type": "boolean" } }, "required": ["to","text"] } },
        { "name": "ask_user", "description": "Ask the user one question and wait; the answer arrives as the next message.", "inputSchema": { "type": "object", "properties": { "question": { "type": "string" } }, "required": ["question"] } },
        { "name": "spawn_subagent", "description": "Hand a task to a new subagent (Prime by default). Use mid-act when the task is bigger than 8 tool calls.",
          "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "purpose": { "type": "string" }, "brief": { "type": "string" }, "isolate": { "type": "boolean" } }, "required": ["name","purpose","brief"] } },
        { "name": "wake_subagent", "description": "Continue an existing subagent with a new brief.", "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "brief": { "type": "string" } }, "required": ["id","brief"] } }
    ]);
    if let Some(a) = list.as_array_mut() {
        a.extend(crate::connectors::tool_specs());
    }
    list
}

pub(crate) fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

pub(crate) fn err_result(msg: impl std::fmt::Display) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("error: {msg}") }], "isError": true })
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}…[{} more chars]", &s[..max], s.len() - max)
    }
}

async fn call_tool(a: &Arc<Agent>, name: &str, args: &Value) -> Value {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
    let chat_for_scratch = a
        .turn
        .lock()
        .await
        .as_ref()
        .map(|t| t.chat_id.clone())
        .unwrap_or_else(|| "shared".into());
    let _ = a.store.log_decision(
        "pull",
        None,
        None,
        Some(name),
        &[],
        Some(&clip(&args.to_string(), 200)),
        None,
        None,
        None,
    );
    match name {
        "history_search" => {
            let q = s("query").unwrap_or_default();
            let limit = args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(10)
                .clamp(1, MAX_HITS);
            let offset: i64 = s("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
            match a.store.t3_search(
                &q,
                s("thread_id").as_deref(),
                s("kind").as_deref(),
                limit,
                offset,
            ) {
                Ok(hits) => {
                    let n = hits.len() as i64;
                    let items: Vec<Value> = hits
                        .into_iter()
                        .map(|(r, score, snip)| json!({ "id": r.id, "thread_id": r.thread_id, "subagent_id": r.subagent_id, "kind": r.kind, "at": r.at, "score": -score, "snippet": clip(&snip, 300) }))
                        .collect();
                    let next = if n == limit {
                        Some((offset + n).to_string())
                    } else {
                        None
                    };
                    text_result(json!({ "hits": items, "next_cursor": next }).to_string())
                }
                Err(e) => err_result(e),
            }
        }
        "history_read" => {
            if let Some(id) = s("id") {
                match a.store.t3_get(&id) {
                    Ok(Some(r)) => {
                        if r.text.len() > PAGE_TOKENS_CHARS {
                            let h = materialize_handle(
                                a,
                                &chat_for_scratch,
                                &format!("history {}", r.id),
                                &r.text,
                            );
                            text_result(json!({ "id": r.id, "kind": r.kind, "at": r.at, "text": clip(&r.text, 8000), "handle": h }).to_string())
                        } else {
                            text_result(json!({ "id": r.id, "thread_id": r.thread_id, "kind": r.kind, "role": r.role, "at": r.at, "text": r.text }).to_string())
                        }
                    }
                    Ok(None) => err_result("no such entry"),
                    Err(e) => err_result(e),
                }
            } else if let Some(range) = args.get("range") {
                let tid = range.get("thread_id").and_then(Value::as_str).unwrap_or("");
                match a.store.t3_thread(
                    tid,
                    range.get("from_id").and_then(Value::as_str),
                    range.get("to_id").and_then(Value::as_str),
                    200,
                ) {
                    Ok(rows) => text_result(page_rows(&rows)),
                    Err(e) => err_result(e),
                }
            } else {
                err_result("id or range required")
            }
        }
        "read_thread" => {
            let tid = s("thread_id").unwrap_or_default();
            let limit = args
                .get("to")
                .and_then(Value::as_i64)
                .map(|t| t - args.get("from").and_then(Value::as_i64).unwrap_or(0))
                .unwrap_or(20)
                .clamp(1, 200);
            match a.store.t3_thread(&tid, None, None, limit) {
                Ok(rows) => text_result(page_rows(&rows)),
                Err(e) => err_result(e),
            }
        }
        "memory_search" => {
            let q = s("query").unwrap_or_default();
            let tiers: Vec<String> = args
                .get("tiers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let inc = args
                .get("include_expired")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let limit = args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(10)
                .clamp(1, MAX_HITS);
            match a.store.memory_search(&q, &tiers, inc, limit) {
                Ok(hits) => {
                    let ids: Vec<String> = hits.iter().map(|(m, _)| m.id.clone()).collect();
                    let _ = a.store.memory_touch(&ids);
                    text_result(
                        json!(hits
                            .into_iter()
                            .map(|(m, sc)| json!({ "record": m, "score": -sc }))
                            .collect::<Vec<_>>())
                        .to_string(),
                    )
                }
                Err(e) => err_result(e),
            }
        }
        "memory_read" => {
            if let Some(tid) = s("thread_id") {
                let t2 = a
                    .store
                    .memory_live("T2", Some(&tid), 40)
                    .unwrap_or_default();
                let sub = a.store.subagent(&tid).ok().flatten();
                text_result(json!({ "thread": sub, "t2": t2 }).to_string())
            } else {
                let ids: Vec<String> = args
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                match a.store.memory_by_ids(&ids) {
                    Ok(rows) => text_result(json!(rows).to_string()),
                    Err(e) => err_result(e),
                }
            }
        }
        "peek" => {
            let id = s("handle").unwrap_or_default();
            let Ok(Some(h)) = a.store.handle(&id) else {
                return err_result("unknown handle");
            };
            let Ok(full) = std::fs::read_to_string(&h.path) else {
                return err_result("handle file missing");
            };
            let max = args
                .get("max")
                .and_then(Value::as_i64)
                .unwrap_or(200)
                .clamp(1, 200) as usize;
            let out = if let Some(pat) = s("grep") {
                match regex_lite::Regex::new(&pat) {
                    Ok(re) => full
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| re.is_match(l))
                        .take(max)
                        .map(|(i, l)| format!("{}: {l}", i + 1))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => return err_result(e),
                }
            } else if let Some(c) = args.get("chars").and_then(Value::as_array) {
                let from = c.first().and_then(Value::as_u64).unwrap_or(0) as usize;
                let to = c.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                full.chars()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect()
            } else {
                let (from, to) = args
                    .get("lines")
                    .and_then(Value::as_array)
                    .map(|l| {
                        (
                            l.first().and_then(Value::as_i64).unwrap_or(1).max(1) as usize,
                            l.get(1)
                                .and_then(Value::as_i64)
                                .unwrap_or(max as i64)
                                .max(1) as usize,
                        )
                    })
                    .unwrap_or((1, max));
                full.lines()
                    .enumerate()
                    .skip(from - 1)
                    .take((to + 1 - from).min(max))
                    .map(|(i, l)| format!("{}: {l}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            text_result(clip(&out, 16_000))
        }
        "sub_llm" => {
            let instruction = s("instruction").unwrap_or_default();
            let text = if let Some(h) = s("handle") {
                match a
                    .store
                    .handle(&h)
                    .ok()
                    .flatten()
                    .and_then(|h| std::fs::read_to_string(h.path).ok())
                {
                    Some(t) => t,
                    None => return err_result("unknown handle"),
                }
            } else {
                s("text").unwrap_or_default()
            };
            let chunk = args
                .get("chunk")
                .and_then(Value::as_u64)
                .unwrap_or(60_000)
                .clamp(2_000, 120_000) as usize;
            let chunks: Vec<String> = text
                .as_bytes()
                .chunks(chunk)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            let mut parts = Vec::new();
            for (i, c) in chunks.iter().enumerate().take(32) {
                match a
                    .llm
                    .complete(&a.cfg.talking_model, &instruction, c, 1500)
                    .await
                {
                    Ok((t, _)) => parts.push(format!("[chunk {}]\n{t}", i + 1)),
                    Err(e) => return err_result(e),
                }
            }
            let mut joined = parts.join("\n\n");
            if parts.len() > 1 && joined.len() > 8000 {
                if let Ok((t, _)) = a
                    .llm
                    .complete(
                        &a.cfg.talking_model,
                        &format!(
                            "Reduce these per-chunk results into one answer for: {instruction}"
                        ),
                        &joined,
                        1500,
                    )
                    .await
                {
                    joined = t;
                }
            }
            let handle = materialize_handle(a, &chat_for_scratch, "sub_llm result", &joined);
            text_result(json!({ "result": clip(&joined, 2000), "handle": handle }).to_string())
        }
        "remember" => {
            let n = s("name").unwrap_or_default();
            match a
                .store
                .scratch_set(&chat_for_scratch, &n, s("value").as_deref())
            {
                Ok(()) => text_result(format!("remembered {n}")),
                Err(e) => err_result(e),
            }
        }
        "recall" => {
            let n = s("name").unwrap_or_default();
            text_result(
                a.store
                    .scratch_get(&chat_for_scratch, &n)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("(no note named {n})")),
            )
        }
        "projects_list" => match reqwest::Client::new()
            .get(format!("{}/projects", a.cfg.relay_url))
            .bearer_auth(&a.cfg.box_token)
            .send()
            .await
        {
            Ok(r) => text_result(r.text().await.unwrap_or_default()),
            Err(e) => err_result(e),
        },
        "projects_send" => {
            let to = s("to").unwrap_or_default();
            let text = s("text").unwrap_or_default();
            let body = json!({ "kind": "message", "payload": { "from": a.cfg.project_id, "from_name": a.cfg.project_name, "text": text, "trace_id": arbos_core::ids::ulid(), "depth": 1 } });
            match reqwest::Client::new()
                .post(format!("{}/projects/{}/push", a.cfg.relay_url, to))
                .bearer_auth(&a.cfg.box_token)
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => text_result(
                    json!({ "request_id": body["payload"]["trace_id"], "status": "queued" })
                        .to_string(),
                ),
                Ok(r) => err_result(format!("relay refused: HTTP {}", r.status())),
                Err(e) => err_result(e),
            }
        }
        "ask_user" => {
            let q = s("question").unwrap_or_default();
            match a.hook_ask_user(&q, &chat_for_scratch).await {
                Ok(()) => text_result(
                    "Question posted. Stop here; the user's answer arrives as the next message.",
                ),
                Err(e) => err_result(e),
            }
        }
        "spawn_subagent" => match a
            .spawn_from_tool(
                None,
                &s("name").unwrap_or_default(),
                &s("purpose").unwrap_or_default(),
                &s("brief").unwrap_or_default(),
                args.get("isolate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
            .await
        {
            Ok(t) => text_result(t),
            Err(e) => err_result(e),
        },
        "wake_subagent" => match a
            .spawn_from_tool(s("id"), "", "", &s("brief").unwrap_or_default(), false)
            .await
        {
            Ok(t) => text_result(t),
            Err(e) => err_result(e),
        },
        "connectors_list" => crate::connectors::connectors_list(a).await,
        "connector_call" => crate::connectors::connector_call(a, args, &chat_for_scratch).await,
        _ => err_result(format!("unknown tool {name}")),
    }
}

fn page_rows(rows: &[crate::store::T3Row]) -> String {
    let mut out = Vec::new();
    let mut budget = PAGE_TOKENS_CHARS;
    let mut next: Option<String> = None;
    for r in rows {
        let text = if r.text.len() > 8000 {
            clip(&r.text, 8000)
        } else {
            r.text.clone()
        };
        if text.len() > budget {
            next = Some(r.id.clone());
            break;
        }
        budget -= text.len();
        out.push(json!({ "id": r.id, "kind": r.kind, "role": r.role, "at": r.at, "text": text }));
    }
    json!({ "turns": out, "next_cursor": next }).to_string()
}

pub(crate) fn materialize_handle(a: &Agent, chat_id: &str, note: &str, text: &str) -> String {
    use sha2::Digest;
    let sha = hex::encode(sha2::Sha256::digest(text.as_bytes()));
    let id = format!("h_{}", &sha[..8]);
    let dir = a.cfg.blobs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(&sha);
    let _ = std::fs::write(&path, text);
    let _ = a.store.upsert_handle(
        &id,
        Some(chat_id),
        None,
        "text",
        text.len() as i64,
        text.lines().count() as i64,
        &path.display().to_string(),
        note,
        None,
    );
    id
}

async fn mcp(State(a): State<S>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let tok = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if tok != a.cfg.mcp_token {
        tracing::warn!(got = %tok.chars().take(8).collect::<String>(), "mcp: bad token");
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => json!({ "protocolVersion": "2025-03-26", "capabilities": { "tools": {} }, "serverInfo": { "name": "arbos", "version": env!("CARGO_PKG_VERSION") } }),
        "notifications/initialized" | "notifications/cancelled" => return StatusCode::ACCEPTED.into_response(),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(&a, name, &args).await
        }
        _ => return Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("unknown method {method}") } })).into_response(),
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new().route("/mcp", post(mcp)).with_state(agent)
}
