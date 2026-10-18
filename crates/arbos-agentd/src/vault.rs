//! Secrets tools (D83): `secrets_list`, `secrets_request`, `egress`, `browser_fill` (the F8 names
//! `vault_list` / `vault_request` stay as aliases for one release). The box never holds a
//! secret: the model works with placeholders (`arbos-secret://<NAME>/<field>`, also exposed to
//! child processes as `$<NAME>`), the relay swaps them in on `POST /egress` for the agent's
//! allowed hosts only, and logins are typed into agent-browser by agentd itself, with the values
//! held for a few seconds and registered for write-time redaction (D19). Every use is audited
//! (names, hosts, URLs; never values).
//!
//! Standing Access comes from the relay (`GET /box/secrets`) and is refreshed on the pushes
//! `secret_access_changed`, `secrets_paused`, `secret_value_replaced`, `secret_decided`, so a
//! switch flipped in Settings reaches this process within one push poll (5 s).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::oneshot;

use arbos_core::approval::{ApprovalOutcome, ApprovalRequest, Risk};
use arbos_core::ids::{now_ms, ulid};

use crate::agent::Agent;
use crate::approval::WaitMode;
use crate::mcp::{err_result, text_result};

const POLL: Duration = Duration::from_secs(2);
const DEFAULT_WAIT_S: u64 = 180;
/// Plaintext fields for a browser fill live at most this long in agentd.
const FILL_BUDGET: Duration = Duration::from_secs(30);
/// After a refusal the agent is told to stop; it may ask again after this long (D76 cool-down).
const DENIED_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const USERNAME_SELECTORS: [&str; 7] = [
    "input[autocomplete=username]",
    "input[type=email]",
    "input[name=username]",
    "input[name=user]",
    "input[name=email]",
    "input[name=login]",
    "input[type=text]",
];
const PASSWORD_SELECTORS: [&str; 2] = [
    "input[autocomplete=current-password]",
    "input[type=password]",
];

/// One secret this project may use right now (an enabled Access on the relay), or one the
/// owner allowed once. Scoped by project: a D70 user box runs one agentd for several workspaces.
#[derive(Debug, Clone)]
struct Grant {
    project_id: String,
    /// The request that granted it (one-time grants and browser fills need it for `values`).
    request_id: Option<String>,
    /// Env name = the secret's name (`STRIPE_KEY`), D83 decision 1.
    name: String,
    kind: String,
    /// field → `arbos-secret://NAME/field`.
    placeholders: BTreeMap<String, String>,
    hosts: Vec<String>,
    note: String,
    /// `all` or `chat:<id>`.
    scope: String,
    /// The relay's connected-vault name (`Arbos` for the built-in one).
    vault_name: String,
}

#[derive(Default)]
struct State {
    grants: Vec<Grant>,
    /// Projects whose owner paused every secret.
    paused: Vec<String>,
    /// (project, name) → when a denial was last heard; the agent does not re-ask inside the cool-down.
    denied_at: BTreeMap<(String, String), Instant>,
    /// Requests a tool call is waiting on: request id → waker fired by `secret_decided`.
    waiting: BTreeMap<String, oneshot::Sender<String>>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn grant_for(project_id: &str, name: &str) -> Option<Grant> {
    state()
        .lock()
        .expect("secrets state")
        .grants
        .iter()
        .rev()
        .find(|g| g.project_id == project_id && g.name == name)
        .cloned()
}

fn remember_grant(g: Grant) {
    let mut st = state().lock().expect("secrets state");
    st.grants
        .retain(|x| !(x.project_id == g.project_id && x.name == g.name));
    st.grants.push(g);
}

fn is_paused(project_id: &str) -> bool {
    state()
        .lock()
        .expect("secrets state")
        .paused
        .iter()
        .any(|p| p == project_id)
}

/// `$NAME` (one field) or `$NAME_FIELD` → placeholder, for every grant of the project. Child
/// processes get these; a Daytona box may already carry the same names as Daytona Secrets,
/// which win. Nothing while the owner paused secrets.
pub fn grant_envs(project_id: &str) -> Vec<(String, String)> {
    if is_paused(project_id) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for g in state().lock().expect("secrets state").grants.iter() {
        if g.project_id != project_id {
            continue;
        }
        for (k, v) in g.env_pairs() {
            if std::env::var_os(&k).is_none() && !out.iter().any(|(x, _)| *x == k) {
                out.push((k, v));
            }
        }
    }
    out
}

impl Grant {
    fn from_item(project_id: &str, item: &Value) -> Option<Self> {
        let name = item["env"].as_str()?.to_owned();
        let placeholders: BTreeMap<String, String> = item["placeholders"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let hosts = item["hosts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let scope = match item["scope"]["kind"].as_str() {
            Some("chat") => format!(
                "chat:{}",
                item["scope"]["chat_id"].as_str().unwrap_or_default()
            ),
            _ => "all".to_owned(),
        };
        Some(Grant {
            project_id: project_id.to_owned(),
            request_id: None,
            name,
            kind: item["kind"].as_str().unwrap_or("key").to_owned(),
            placeholders,
            hosts,
            note: item["note"].as_str().unwrap_or("").to_owned(),
            scope,
            vault_name: item["vault_name"].as_str().unwrap_or("Arbos").to_owned(),
        })
    }

    fn env_pairs(&self) -> Vec<(String, String)> {
        if self.placeholders.len() == 1 {
            return self
                .placeholders
                .values()
                .map(|p| (self.name.clone(), p.clone()))
                .collect();
        }
        self.placeholders
            .iter()
            .map(|(f, p)| {
                (
                    format!("{}_{}", self.name, f.to_ascii_uppercase()),
                    p.clone(),
                )
            })
            .collect()
    }

    fn describe(&self, status: &str) -> Value {
        let env: BTreeMap<String, String> = self.env_pairs().into_iter().collect();
        json!({
            "status": status, "requestId": self.request_id, "name": self.name, "kind": self.kind,
            "placeholders": self.placeholders, "env": env, "hosts": self.hosts, "note": self.note, "scope": self.scope,
            "vault": self.vault_name,
            "how": match self.kind.as_str() {
                "login" => "Call browser_fill(name, url) to have Arbos type the login into the page; you never see the values.".to_owned(),
                _ => format!("Put the placeholder (or ${}) in a header or body and call the egress tool; the relay swaps in the real value only for the allowed hosts and redacts it from the reply. Never try to read, print, encode or copy the value.", self.name),
            },
        })
    }
}

fn mock_on() -> bool {
    std::env::var("ARBOS_VAULT_MOCK").is_ok_and(|v| v == "1")
}

fn wait_budget() -> Duration {
    Duration::from_secs(
        std::env::var("ARBOS_VAULT_WAIT_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_WAIT_S),
    )
}

pub fn tool_specs() -> Vec<Value> {
    let list = json!({ "name": "secrets_list", "description": "Secrets the user keeps in Arbos (and in connected vaults such as 1Password): names, kinds (key | login | text), field names, allowed hosts, and whether this agent has access. Values are never shown. A secret you have access to is already in your environment as $NAME holding a placeholder; otherwise call secrets_request.",
        "inputSchema": { "type": "object", "properties": {} } });
    let request = json!({ "name": "secrets_request", "description": "Ask the owner to let you use a secret. name = the secret's name (or an op:// reference), purpose = one sentence shown on the card, use = 'header' or 'env' for keys (you get a placeholder usable only through egress or as $NAME in a shell), 'browser_fill' for logins (then call browser_fill), host = the host you will use it at. Returns at once when the owner already gave you access; otherwise posts a card and waits for the answer (Allow once / Allow for this chat / Always allow / Deny).",
        "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "purpose": { "type": "string", "description": "one sentence shown to the user" }, "use": { "type": "string", "enum": ["header", "env", "browser_fill"] }, "host": { "type": "string", "description": "the host you will send it to, e.g. api.stripe.com" } }, "required": ["name", "purpose"] } });
    let mut alias_list = list.clone();
    alias_list["name"] = json!("vault_list");
    let mut alias_request = request.clone();
    alias_request["name"] = json!("vault_request");
    vec![
        list,
        request,
        json!({ "name": "egress", "description": "HTTP request performed by the relay with secret placeholders (arbos-secret://NAME/field, $NAME, or op:// references) substituted in headers/body for the secret's allowed hosts only. Returns status, headers and body with secret values redacted. Use this instead of curl whenever a request needs a secret.",
                "inputSchema": { "type": "object", "properties": { "url": { "type": "string" }, "method": { "type": "string" }, "headers": { "type": "object", "additionalProperties": { "type": "string" } }, "body": { "type": ["string", "object", "null"] } }, "required": ["url"] } }),
        json!({ "name": "browser_fill", "description": "Arbos opens url in the box browser and types the login's username and password into the sign-in form (access needed; the values are never shown to you). Afterwards use the browser as usual (click Sign in, read the page).",
                "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "url": { "type": "string" } }, "required": ["name", "url"] } }),
        alias_list,
        alias_request,
    ]
}

async fn relay_get(agent: &Agent, path: &str) -> Result<Value, String> {
    let r = agent
        .http_client()
        .get(format!("{}{}", agent.cfg.relay_url, path))
        .bearer_auth(&agent.cfg.box_token)
        .header("x-arbos-project", &agent.cfg.project_id)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(relay_error(status, &text, path));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// `(status, body)` → one line for the model: the relay's `error.message` when it has one.
fn relay_error(status: reqwest::StatusCode, text: &str, path: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    match v["error"]["message"].as_str() {
        Some(m) => m.to_owned(),
        None => format!("relay {path}: HTTP {status} {text}"),
    }
}

async fn relay_post(agent: &Agent, path: &str, body: &Value) -> Result<(u16, Value), String> {
    let r = agent
        .http_client()
        .post(format!("{}{}", agent.cfg.relay_url, path))
        .bearer_auth(&agent.cfg.box_token)
        .header("x-arbos-project", &agent.cfg.project_id)
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(relay_error(status, &text, path));
    }
    Ok((
        status.as_u16(),
        serde_json::from_str(&text).map_err(|e| e.to_string())?,
    ))
}

/// Pull `GET /box/secrets` and rebuild this project's standing grants. Called at workspace start
/// and on every secrets push.
pub async fn refresh_grants(agent: &Agent) -> Result<Vec<Value>, String> {
    let v = relay_get(agent, "/box/secrets").await?;
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let paused = v["paused"].as_bool().unwrap_or(false);
    let project = agent.cfg.project_id.clone();
    let mut st = state().lock().expect("secrets state");
    st.grants
        .retain(|g| g.project_id != project || g.request_id.is_some());
    for it in &items {
        if it["enabled"].as_bool().unwrap_or(false) {
            if let Some(g) = Grant::from_item(&project, it) {
                st.grants
                    .retain(|x| !(x.project_id == project && x.name == g.name));
                st.grants.push(g);
            }
        }
    }
    st.paused.retain(|p| *p != project);
    if paused {
        st.paused.push(project);
    }
    Ok(items)
}

/// A secrets push from the relay (`secret_access_changed`, `secrets_paused`,
/// `secret_value_replaced`, `secret_decided`, `secret_notice`).
pub async fn on_push(agent: &Arc<Agent>, kind: &str, payload: &Value) {
    match kind {
        "secret_decided" => {
            let id = payload["request_id"].as_str().unwrap_or("").to_owned();
            let status = payload["status"].as_str().unwrap_or("").to_owned();
            let waker = state().lock().expect("secrets state").waiting.remove(&id);
            if let Some(tx) = waker {
                let _ = tx.send(status);
            }
            let _ = refresh_grants(agent).await;
        }
        "secret_notice" => {
            let text = payload["text"].as_str().unwrap_or("").to_owned();
            if text.is_empty() {
                return;
            }
            let chat_hint = payload["chat_id"].as_str().unwrap_or("").to_owned();
            let chat_id = agent.approval_chat(&chat_hint).await.unwrap_or(chat_hint);
            if let Some(host) = agent.sync.host(&chat_id).await {
                let h = host.lock().await;
                h.doc.append_system_note(
                    &ulid(),
                    &format!("agent:{}", agent.cfg.project_id),
                    &text,
                    &agent.cfg.device_id,
                );
            }
            agent.append_audit(
                "secret_notice",
                json!({ "text": text, "host": payload["host"], "name": payload["env"] }),
            );
        }
        _ => {
            // `secret_access_changed`, `secrets_paused`, `secret_value_replaced`: the relay is
            // the authority; take its current view (≤ 5 s after the switch, PUSH_POLL).
            if let Err(e) = refresh_grants(agent).await {
                tracing::warn!(error = %e, kind, "secrets: refresh after push failed");
            }
            agent.append_audit(
                "secrets_changed",
                json!({ "kind": kind, "env": payload["env"], "enabled": payload["enabled"], "paused": payload["paused"] }),
            );
        }
    }
}

pub async fn secrets_list(agent: &Agent) -> Value {
    let list = match refresh_grants(agent).await {
        Ok(l) => l,
        Err(e) => return err_result(e),
    };
    let paused = is_paused(&agent.cfg.project_id);
    let items: Vec<Value> = list
        .iter()
        .map(|it| {
            let access = it["enabled"].as_bool().unwrap_or(false);
            json!({ "name": it["env"], "kind": it["kind"], "fields": it["fields"], "placeholders": it["placeholders"],
                "hosts": it["hosts"], "note": it["note"], "vault": it["vault_name"], "ref": it["ref"],
                "access": if paused { "paused" } else if access { "on" } else { "off" },
                "ask_every_time": it["ask_every_time"] })
        })
        .collect();
    text_result(
        json!({ "paused": paused, "items": items,
            "note": if paused { "Secrets are paused by the owner: every use fails until they turn Pause all secrets off. Stop and say so." }
                    else { "Values are never readable. A secret with access on is already $NAME in your shell (a placeholder) and usable through egress; for one with access off call secrets_request(name, purpose, use, host)." } })
            .to_string(),
    )
}

async fn relay_status(agent: &Agent, request_id: &str) -> Result<String, String> {
    let v = relay_get(agent, &format!("/box/secrets/requests/{request_id}")).await?;
    Ok(v["request"]["status"]
        .as_str()
        .unwrap_or("pending")
        .to_owned())
}

/// Flip the chat card to match the relay's decision. Already decided from the card itself is
/// fine: first decision wins.
async fn settle_card(agent: &Arc<Agent>, approval_id: Option<&str>, status: &str) {
    let Some(id) = approval_id else { return };
    let outcome = match status {
        "allowed_once" | "allowed_chat" | "allowed_always" => ApprovalOutcome::Approved,
        "denied" => ApprovalOutcome::Rejected {
            reason: Some("denied by the owner".into()),
        },
        _ => ApprovalOutcome::Expired,
    };
    let _ = agent.decide_approval(id, outcome, "arbos:secrets").await;
}

/// Wait for the decision. The relay row is the authority: Settings writes it directly, and a
/// client that answers the chat card forwards the decision there (`secretRequestId` in the card
/// payload). The relay's `secret_decided` push ends the wait at once; polling covers the rest.
/// `Ok(None)` when the budget runs out with the request still pending.
async fn wait_decision(
    agent: &Arc<Agent>,
    request_id: &str,
    approval_id: Option<&str>,
    mut card: Option<oneshot::Receiver<ApprovalOutcome>>,
    budget: Duration,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    let (tx, mut pushed) = oneshot::channel::<String>();
    state()
        .lock()
        .expect("secrets state")
        .waiting
        .insert(request_id.to_owned(), tx);
    let result = loop {
        let status = relay_status(agent, request_id).await?;
        if status != "pending" {
            settle_card(agent, approval_id, &status).await;
            break Ok(Some(status));
        }
        if start.elapsed() >= budget {
            break Ok(None);
        }
        tokio::select! {
            res = &mut pushed => {
                if let Ok(status) = res {
                    if status != "pending" {
                        settle_card(agent, approval_id, &status).await;
                        break Ok(Some(status));
                    }
                }
            }
            res = async { match card.as_mut() { Some(rx) => rx.await, None => std::future::pending().await } } => {
                card = None;
                match res {
                    // The client forwards the approval to the relay; keep polling.
                    Ok(ApprovalOutcome::Approved) => {}
                    Ok(ApprovalOutcome::Rejected { .. }) => break Ok(Some("denied".into())),
                    Ok(ApprovalOutcome::Expired) | Err(_) => break Ok(Some("expired".into())),
                }
            }
            _ = tokio::time::sleep(POLL) => {}
        }
    };
    state()
        .lock()
        .expect("secrets state")
        .waiting
        .remove(request_id);
    result
}

/// Find the standing grant or ask the relay; post the card when the relay says a decision is
/// needed; wait for it. Shared by `secrets_request` and `browser_fill`.
async fn obtain_grant(
    agent: &Arc<Agent>,
    chat_hint: &str,
    name: &str,
    purpose: &str,
    use_kind: &str,
    host: Option<&str>,
) -> Result<Grant, Value> {
    // Access cards belong on the pinned main chat (same rule as `approval_chat`).
    let chat_id = agent
        .approval_chat(chat_hint)
        .await
        .unwrap_or_else(|| chat_hint.to_owned());
    let chat_id = chat_id.as_str();
    let project = agent.cfg.project_id.clone();
    if is_paused(&project) {
        return Err(err_result("Secrets are paused by the owner. Stop and tell the user; do not ask again until they turn Pause all secrets off."));
    }
    let name = name
        .strip_prefix("arbos-secret://")
        .and_then(|r| r.split('/').next())
        .unwrap_or(name)
        .trim_start_matches('$')
        .to_owned();
    let cooled = state()
        .lock()
        .expect("secrets state")
        .denied_at
        .get(&(project.clone(), name.clone()))
        .is_some_and(|t| t.elapsed() < DENIED_COOLDOWN);
    if cooled {
        return Err(err_result(format!(
            "{name}: the owner denied this in the last 10 minutes. Do not ask again now; tell the user what you needed it for."
        )));
    }
    let host_ok = |g: &Grant| {
        host.is_none()
            || g.hosts.is_empty()
            || host.is_some_and(|h| {
                g.hosts.iter().any(|a| {
                    let a = a.to_ascii_lowercase();
                    h.eq_ignore_ascii_case(&a)
                        || a.strip_prefix("*.")
                            .is_some_and(|suf| h.to_ascii_lowercase().ends_with(suf))
                })
            })
    };
    if let Some(g) = grant_for(&project, &name) {
        if g.request_id.is_none() && host_ok(&g) && use_kind != "browser_fill" {
            return Ok(g);
        }
    }
    let (status_code, created) = relay_post(
        agent,
        "/box/secrets/request",
        &json!({ "name": name, "purpose": purpose, "use": use_kind, "host": host, "chat_id": chat_id }),
    )
    .await
    .map_err(err_result)?;
    let request = &created["request"];
    let request_id = request["id"].as_str().unwrap_or("").to_owned();
    if status_code == 200 {
        // Standing Access covers it: no card.
        let _ = refresh_grants(agent).await;
        let mut g = grant_for(&project, &name)
            .or_else(|| Grant::from_item(&project, &created["item"]))
            .ok_or_else(|| err_result(format!("{name}: no such secret")))?;
        g.request_id = Some(request_id);
        remember_grant(g.clone());
        return Ok(g);
    }
    let resumed = created["resumed"].as_bool().unwrap_or(false);
    let kind = {
        let items = refresh_grants(agent).await.unwrap_or_default();
        items
            .iter()
            .find(|it| it["env"].as_str() == Some(name.as_str()))
            .and_then(|it| it["kind"].as_str())
            .unwrap_or("key")
            .to_owned()
    };
    if use_kind == "browser_fill" && kind != "login" {
        return Err(err_result(format!(
            "{name} is a {kind}; browser_fill needs a Login"
        )));
    }
    agent.append_audit(
        "secret_requested",
        json!({ "requestId": request_id, "name": name, "kind": kind, "use": use_kind, "host": host, "purpose": purpose, "chatId": chat_id, "resumed": resumed }),
    );
    let _ = agent.store.t3_append(
        chat_id,
        None,
        Some(chat_id),
        "secret_request",
        Some("assistant"),
        &format!("secrets_request {name} ({use_kind}): {purpose}"),
        None,
    );
    let (approval_id, card) = if resumed {
        (None, None)
    } else {
        // One card per request, four answers (D76 A–D). `secretRequestId` lets the client
        // forward the answer to the relay row, which stays the authority (`mcp.rs` skips its
        // generic gate for the secrets tools so this is the only card).
        let (tx, rx) = oneshot::channel();
        let agent_name = agent.cfg.project_name.clone();
        let title = match (kind.as_str(), host) {
            ("login", Some(h)) => format!("Let {agent_name} sign in to {h} with {name}?"),
            ("login", None) => format!("Let {agent_name} sign in with {name}?"),
            (_, Some(h)) => format!("Let {agent_name} use {name} at {h}?"),
            (_, None) => format!("Let {agent_name} use {name}?"),
        };
        let req = ApprovalRequest {
            title,
            summary: purpose.to_owned(),
            risk: Risk::Credential,
            payload: json!({ "kind": "secret_request", "secretRequestId": request_id, "name": name, "secretKind": kind,
                "use": use_kind, "host": host, "purpose": purpose, "agent": agent_name,
                "answers": [
                    { "id": "once", "label": "Allow once" },
                    { "id": "chat", "label": "Allow for this chat" },
                    { "id": "always", "label": format!("Always allow {agent_name}") },
                    { "id": "deny", "label": "Deny" },
                ] }),
        };
        match agent
            .begin_approval(chat_id, req, WaitMode::Inline, Some(tx))
            .await
        {
            Ok(id) => (Some(id), Some(rx)),
            Err(e) => {
                tracing::warn!(error = %e, name = %name, "secrets: approval card could not be posted; Settings → Secrets still decides");
                (None, None)
            }
        }
    };

    let decided = wait_decision(
        agent,
        &request_id,
        approval_id.as_deref(),
        card,
        wait_budget(),
    )
    .await
    .map_err(err_result)?;
    let Some(status) = decided else {
        return Err(text_result(
            json!({ "status": "pending", "requestId": request_id, "name": name, "use": use_kind,
                "note": "The owner has not decided yet (the card in this chat, or Arbos → Settings → Secrets). Tell them you are waiting for access and end the turn; calling secrets_request again with the same name resumes waiting and returns at once once it is allowed." })
                .to_string(),
        ));
    };
    agent.append_audit(
        "secret_decided",
        json!({ "requestId": request_id, "name": name, "use": use_kind, "status": status, "decidedBy": "owner" }),
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
            host.lock().await.doc.add_hidden_part(
                &msg_id,
                "secret_request",
                &format!("secret {name} ({use_kind}): {status}"),
                &json!({ "requestId": request_id, "name": name, "use": use_kind, "status": status, "decidedAt": now_ms() }),
            );
        }
    }
    if !matches!(
        status.as_str(),
        "allowed_once" | "allowed_chat" | "allowed_always"
    ) {
        if status == "denied" {
            state()
                .lock()
                .expect("secrets state")
                .denied_at
                .insert((project.clone(), name.clone()), Instant::now());
        }
        return Err(err_result(format!(
            "{name}: the owner {} this request. Stop, and tell the user what you needed it for; do not ask again for 10 minutes.",
            if status == "denied" { "denied" } else { "did not answer" }
        )));
    }
    let items = refresh_grants(agent).await.unwrap_or_default();
    let mut g = grant_for(&project, &name)
        .or_else(|| {
            items
                .iter()
                .find(|it| it["env"].as_str() == Some(name.as_str()))
                .and_then(|it| Grant::from_item(&project, it))
        })
        .ok_or_else(|| err_result(format!("{name}: the secret is gone")))?;
    g.request_id = Some(request_id);
    if let Some(h) = host {
        if !g.hosts.iter().any(|x| x == h) {
            g.hosts.push(h.to_owned());
        }
    }
    remember_grant(g.clone());
    Ok(g)
}

pub async fn secrets_request(agent: &Arc<Agent>, args: &Value, chat_id: &str) -> Value {
    let name = args["name"]
        .as_str()
        .or(args["ref"].as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let purpose = args["purpose"].as_str().unwrap_or("").trim().to_owned();
    let use_kind = args["use"].as_str().unwrap_or("header").trim().to_owned();
    let host = args["host"]
        .as_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(|h| {
            reqwest::Url::parse(h)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| h.to_ascii_lowercase())
        });
    if name.is_empty() {
        return err_result("name is required (see secrets_list)");
    }
    if !matches!(use_kind.as_str(), "env" | "header" | "browser_fill") {
        return err_result("use must be header, env or browser_fill");
    }
    match obtain_grant(agent, chat_id, &name, &purpose, &use_kind, host.as_deref()).await {
        Ok(g) => text_result(g.describe("allowed").to_string()),
        Err(v) => v,
    }
}

/// `$NAME` / `${NAME}` → the granted placeholder, so shell-style references work.
fn expand_env_refs(project_id: &str, text: &str) -> String {
    let mut out = text.to_owned();
    for g in state().lock().expect("secrets state").grants.iter() {
        if g.project_id != project_id {
            continue;
        }
        for (k, v) in g.env_pairs() {
            out = out
                .replace(&format!("${{{k}}}"), &v)
                .replace(&format!("${k}"), &v);
        }
    }
    out
}

pub async fn egress(agent: &Arc<Agent>, args: &Value, chat_id: &str) -> Value {
    let url = args["url"].as_str().unwrap_or("").trim().to_owned();
    if url.is_empty() {
        return err_result("url is required");
    }
    let method = args["method"]
        .as_str()
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase();
    let project = agent.cfg.project_id.as_str();
    if is_paused(project) {
        return err_result("Secrets are paused by the owner. Stop and tell the user.");
    }
    let headers: BTreeMap<String, String> = args["headers"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let s = v
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), expand_env_refs(project, &s))
                })
                .collect()
        })
        .unwrap_or_default();
    let body = match &args["body"] {
        Value::Null => Value::Null,
        Value::String(s) => Value::String(expand_env_refs(project, s)),
        other => Value::String(expand_env_refs(project, &other.to_string())),
    };
    let mut placeholders: Vec<String> = Vec::new();
    for text in headers
        .values()
        .cloned()
        .chain(body.as_str().map(str::to_owned))
    {
        for ph in text
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
            .filter(|t| t.starts_with("arbos-secret://") || t.starts_with("op://"))
        {
            if !placeholders.contains(&ph.to_owned()) {
                placeholders.push(ph.to_owned());
            }
        }
    }
    agent.append_audit(
        "egress",
        json!({ "url": url, "method": method, "placeholders": placeholders, "chatId": chat_id }),
    );
    let _ = agent.store.t3_append(
        chat_id,
        None,
        Some(chat_id),
        "egress",
        Some("assistant"),
        &format!(
            "egress {method} {url} placeholders={}",
            placeholders.join(",")
        ),
        None,
    );
    let r = agent
        .http_client()
        .post(format!("{}/egress", agent.cfg.relay_url))
        .bearer_auth(&agent.cfg.box_token)
        .header("x-arbos-project", &agent.cfg.project_id)
        .json(&json!({ "url": url, "method": method, "headers": headers, "body": body, "placeholders": placeholders, "chat_id": chat_id }))
        .send()
        .await;
    let (status, text) = match r {
        Ok(r) => {
            let st = r.status();
            (st, r.text().await.unwrap_or_default())
        }
        Err(e) => return err_result(format!("relay unreachable: {e}")),
    };
    let text = crate::store::redact(&text);
    if !status.is_success() && status.as_u16() != 502 {
        if status.as_u16() == 403 && text.contains("\"paused\"") {
            let _ = refresh_grants(agent).await;
            return err_result("Secrets are paused by the owner. Stop and tell the user; do not retry until they turn Pause all secrets off.");
        }
        return err_result(format!("egress: {}", relay_error(status, &text, "/egress")));
    }
    // A refusal the relay reported inside a 200 (the request went out with the placeholder
    // verbatim) is worth a fresh look at the grants: the switch may have flipped.
    if text.contains("access was turned off") || text.contains("no access for this agent") {
        let _ = refresh_grants(agent).await;
    }
    text_result(text)
}

/// Run `agent-browser` with a timeout; stdout/stderr are redacted before anyone reads them.
async fn agent_browser(args: &[&str], timeout: Duration) -> Result<String, String> {
    let fut = tokio::process::Command::new("agent-browser")
        .args(args)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| "agent-browser timed out".to_owned())?
        .map_err(|e| format!("agent-browser: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = crate::store::redact(&text);
    if out.status.success() {
        Ok(text)
    } else {
        Err(text.chars().take(300).collect())
    }
}

fn agent_browser_available() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("agent-browser").is_file()))
}

pub async fn browser_fill(agent: &Arc<Agent>, args: &Value, chat_id: &str) -> Value {
    let name = args["name"].as_str().unwrap_or("").trim().to_owned();
    let url = args["url"].as_str().unwrap_or("").trim().to_owned();
    if name.is_empty() || url.is_empty() {
        return err_result("name and url are required");
    }
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let grant = match obtain_grant(
        agent,
        chat_id,
        &name,
        &format!("sign in at {url}"),
        "browser_fill",
        Some(&host),
    )
    .await
    {
        Ok(g) => g,
        Err(v) => return v,
    };
    let Some(request_id) = grant.request_id.clone() else {
        return err_result(format!(
            "{name}: no fill window; call secrets_request first"
        ));
    };
    // The values arrive now and are gone when this function returns.
    let values = match relay_post(
        agent,
        &format!("/box/secrets/requests/{request_id}/values"),
        &json!({}),
    )
    .await
    {
        Ok((_, v)) => v,
        Err(e) => {
            // The approval window may have closed; drop the grant so the next call asks again.
            state()
                .lock()
                .expect("secrets state")
                .grants
                .retain(|g| g.request_id.as_deref() != Some(request_id.as_str()));
            return err_result(format!("could not fetch login fields: {e}"));
        }
    };
    let fields: BTreeMap<String, String> = values["fields"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    for v in fields.values() {
        crate::store::redact_live_value(v);
    }
    let started = Instant::now();
    let mut filled: Vec<&str> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let simulated = !agent_browser_available();
    if simulated && !mock_on() {
        return err_result("agent-browser is not installed in this box");
    }
    if simulated {
        // Mock: no browser here; stand in for the form submit with one direct POST so the demo
        // target can show it received the real login, which the model still never sees.
        let form: Vec<(String, String)> = ["username", "password"]
            .iter()
            .filter_map(|k| fields.get(*k).map(|v| ((*k).to_owned(), v.clone())))
            .collect();
        match agent
            .http_client()
            .post(&url)
            .timeout(Duration::from_secs(10))
            .form(&form)
            .send()
            .await
        {
            Ok(r) => {
                notes.push(format!("simulated form submit: HTTP {}", r.status()));
                filled.extend(form.iter().map(|(k, _)| {
                    if k == "username" {
                        "username"
                    } else {
                        "password"
                    }
                }));
            }
            Err(e) => notes.push(format!("simulated form submit failed: {e}")),
        }
    } else {
        match agent_browser(&["open", &url], FILL_BUDGET).await {
            Ok(_) => {}
            Err(e) => return err_result(format!("open {url}: {e}")),
        }
        let remaining = || FILL_BUDGET.saturating_sub(started.elapsed());
        if let Some(u) = fields.get("username") {
            for sel in USERNAME_SELECTORS {
                if remaining().is_zero() {
                    break;
                }
                if agent_browser(&["fill", sel, u], remaining()).await.is_ok() {
                    filled.push("username");
                    break;
                }
            }
        }
        if let Some(p) = fields.get("password") {
            for sel in PASSWORD_SELECTORS {
                if remaining().is_zero() {
                    break;
                }
                if agent_browser(&["fill", sel, p], remaining()).await.is_ok() {
                    filled.push("password");
                    break;
                }
            }
        }
        if filled.is_empty() {
            notes.push("no sign-in fields found on the page".into());
        }
    }
    drop(fields);
    agent.append_audit(
        "browser_fill",
        json!({ "name": name, "url": url, "filled": filled, "simulated": simulated, "ms": started.elapsed().as_millis() as i64, "chatId": chat_id }),
    );
    let _ = agent.store.t3_append(
        chat_id,
        None,
        Some(chat_id),
        "browser_fill",
        Some("assistant"),
        &format!("browser_fill {name} at {url}: filled {}", filled.join(", ")),
        None,
    );
    text_result(
        json!({ "ok": !filled.is_empty(), "name": name, "url": url, "filled": filled, "simulated": simulated, "notes": notes,
            "note": "Arbos typed the values; they are not available to you. Continue in the browser (submit the form, read the result)." })
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env name = the secret's own name (decision 1): one-field secrets are `$NAME`, logins
    /// `$NAME_USERNAME` / `$NAME_PASSWORD`; nothing while paused.
    #[test]
    fn env_pairs_follow_decision_1() {
        let item = json!({ "env": "STRIPE_KEY", "kind": "key", "enabled": true, "hosts": ["api.stripe.com"],
            "placeholders": { "value": "arbos-secret://STRIPE_KEY/value" }, "note": "refunds", "vault_name": "Arbos" });
        let g = Grant::from_item("p1", &item).unwrap();
        assert_eq!(
            g.env_pairs(),
            vec![(
                "STRIPE_KEY".to_owned(),
                "arbos-secret://STRIPE_KEY/value".to_owned()
            )]
        );
        let login = json!({ "env": "GITHUB_LOGIN", "kind": "login", "enabled": true, "hosts": [],
            "placeholders": { "username": "arbos-secret://GITHUB_LOGIN/username", "password": "arbos-secret://GITHUB_LOGIN/password" },
            "scope": { "kind": "chat", "chat_id": "c7" } });
        let g2 = Grant::from_item("p1", &login).unwrap();
        let mut names: Vec<String> = g2.env_pairs().into_iter().map(|(k, _)| k).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["GITHUB_LOGIN_PASSWORD", "GITHUB_LOGIN_USERNAME"]
        );
        assert_eq!(g2.scope, "chat:c7");

        remember_grant(g.clone());
        assert_eq!(grant_envs("p1").len(), 1);
        assert!(grant_envs("p2").is_empty());
        state().lock().unwrap().paused.push("p1".into());
        assert!(grant_envs("p1").is_empty());
        state().lock().unwrap().paused.clear();
        assert_eq!(
            expand_env_refs("p1", "Bearer $STRIPE_KEY and ${STRIPE_KEY}"),
            "Bearer arbos-secret://STRIPE_KEY/value and arbos-secret://STRIPE_KEY/value"
        );
        let described = g.describe("allowed").to_string();
        assert!(described.contains("$STRIPE_KEY") && !described.contains("sk-"));
    }
}
