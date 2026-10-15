//! Vault tools (F8): `vault_list`, `vault_request`, `egress`, `browser_fill`. The box never holds
//! a credential: the model works with placeholders (`arbos-secret://<item>/<field>`, also exposed
//! to child processes as `ARBOS_SECRET_<NAME>`), the relay swaps them in on `POST /egress` for the
//! item's allowed hosts only, and logins are typed into agent-browser by agentd itself, with the
//! values held for a few seconds and registered for write-time redaction (D19). Every use is
//! audited (names, hosts, URLs; never values).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::oneshot;

use arbos_core::approval::{ApprovalOutcome, ApprovalRequest, Risk};
use arbos_core::ids::now_ms;

use crate::agent::Agent;
use crate::approval::WaitMode;
use crate::mcp::{err_result, text_result};

const POLL: Duration = Duration::from_secs(2);
const DEFAULT_WAIT_S: u64 = 180;
/// Plaintext fields for a browser fill live at most this long in agentd.
const FILL_BUDGET: Duration = Duration::from_secs(30);
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

/// One approved request this agentd knows about. Scoped by project: a D70 user box runs one
/// agentd for several workspaces, and an approval is per project on the relay too.
#[derive(Debug, Clone)]
struct Grant {
    project_id: String,
    request_id: String,
    name: String,
    kind: String,
    use_kind: String,
    env: String,
    /// field → `arbos-secret://name/field`.
    placeholders: BTreeMap<String, String>,
    hosts: Vec<String>,
}

fn grants() -> &'static Mutex<Vec<Grant>> {
    static G: OnceLock<Mutex<Vec<Grant>>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(Vec::new()))
}

fn grant_for(project_id: &str, name: &str, use_kind: &str) -> Option<Grant> {
    grants()
        .lock()
        .expect("grants")
        .iter()
        .rev()
        .find(|g| g.project_id == project_id && g.name == name && g.use_kind == use_kind)
        .cloned()
}

fn remember_grant(g: Grant) {
    let mut v = grants().lock().expect("grants");
    v.retain(|x| !(x.project_id == g.project_id && x.name == g.name && x.use_kind == g.use_kind));
    v.push(g);
}

/// A request the user has not decided yet, keyed by (project, item, use). A repeated `vault_request`
/// for the same item resumes waiting instead of posting a second card.
#[derive(Debug, Clone)]
struct PendingRequest {
    request_id: String,
    approval_id: Option<String>,
}

type PendingMap = BTreeMap<(String, String, String), PendingRequest>;

fn pending() -> &'static Mutex<PendingMap> {
    static P: OnceLock<Mutex<PendingMap>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// `ARBOS_SECRET_<NAME>` (one field) or `ARBOS_SECRET_<NAME>_<FIELD>` → placeholder, for every
/// approved `env` grant. Child processes get these; a Daytona box may already carry the same
/// names as Daytona Secrets, which win.
pub fn grant_envs(project_id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for g in grants().lock().expect("grants").iter() {
        if g.use_kind != "env" || g.project_id != project_id {
            continue;
        }
        for (k, v) in g.env_pairs() {
            if std::env::var_os(&k).is_none() {
                out.push((k, v));
            }
        }
    }
    out
}

impl Grant {
    fn env_pairs(&self) -> Vec<(String, String)> {
        if self.placeholders.len() == 1 {
            return self
                .placeholders
                .values()
                .map(|p| (self.env.clone(), p.clone()))
                .collect();
        }
        self.placeholders
            .iter()
            .map(|(f, p)| {
                (
                    format!("{}_{}", self.env, f.to_ascii_uppercase()),
                    p.clone(),
                )
            })
            .collect()
    }

    fn describe(&self) -> Value {
        let env: BTreeMap<String, String> = self.env_pairs().into_iter().collect();
        json!({
            "status": "approved", "requestId": self.request_id, "item": self.name, "kind": self.kind, "use": self.use_kind,
            "placeholders": self.placeholders, "env": env, "allowed_hosts": self.hosts,
            "how": match self.use_kind.as_str() {
                "browser_fill" => "Call browser_fill(name, url) to have Arbos type the login into the page; you never see the values.".to_owned(),
                _ => "Put the placeholder (or $ENV name) in a header or body and call the egress tool; the relay swaps in the real value only for the allowed hosts and redacts it from the reply. Never try to read the value.".to_owned(),
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
    vec![
        json!({ "name": "vault_list", "description": "Credentials the user keeps in the Arbos vault: names, kinds (login | secret | api_key), field names and allowed hosts. Values are never shown; use vault_request to get a placeholder you can send through egress, or browser_fill for logins.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "vault_request", "description": "Ask the user to let you use a vault item. use = 'header' or 'env' for API keys/secrets (you get a placeholder and an ARBOS_SECRET_<NAME> env name for subagent shells, usable only through the egress tool), 'browser_fill' for logins (then call browser_fill). Posts an approval card and waits for the user's decision (the card, or Settings → Vault); returns pending when they have not answered yet (call again later to resume waiting).",
                "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "purpose": { "type": "string", "description": "one sentence shown to the user" }, "use": { "type": "string", "enum": ["header", "env", "browser_fill"] } }, "required": ["name", "purpose"] } }),
        json!({ "name": "egress", "description": "HTTP request performed by the relay with vault placeholders (arbos-secret://item/field or $ARBOS_SECRET_NAME) substituted in headers/body for the item's allowed hosts only. Returns status, headers and body with secret values redacted. Use this instead of curl whenever a request needs a vault credential.",
                "inputSchema": { "type": "object", "properties": { "url": { "type": "string" }, "method": { "type": "string" }, "headers": { "type": "object", "additionalProperties": { "type": "string" } }, "body": { "type": ["string", "object", "null"] } }, "required": ["url"] } }),
        json!({ "name": "browser_fill", "description": "Arbos opens url in the box browser and types the vault login's username and password into the sign-in form (approval needed; the values are never shown to you). Afterwards use the browser as usual (click Sign in, read the page).",
                "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "url": { "type": "string" } }, "required": ["name", "url"] } }),
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

async fn relay_post(agent: &Agent, path: &str, body: &Value) -> Result<Value, String> {
    let r = agent
        .http_client()
        .post(format!("{}{}", agent.cfg.relay_url, path))
        .bearer_auth(&agent.cfg.box_token)
        .json(body)
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

async fn items(agent: &Agent) -> Result<Vec<Value>, String> {
    let v = relay_get(agent, "/vault/items").await?;
    Ok(v["items"].as_array().cloned().unwrap_or_default())
}

pub async fn vault_list(agent: &Agent) -> Value {
    let list = match items(agent).await {
        Ok(l) => l,
        Err(e) => return err_result(e),
    };
    let granted: Vec<Value> = grants()
        .lock()
        .expect("grants")
        .iter()
        .filter(|g| g.project_id == agent.cfg.project_id)
        .map(|g| json!({ "item": g.name, "use": g.use_kind, "env": g.env }))
        .collect();
    let items: Vec<Value> = list
        .iter()
        .map(|it| {
            json!({ "name": it["name"], "kind": it["kind"], "fields": it["fields"], "allowed_hosts": it["allowed_hosts"], "env": it["env"], "mock": it["mock"] })
        })
        .collect();
    text_result(
        json!({ "items": items, "granted": granted, "note": "Values are never readable. vault_request(name, purpose, use) asks the user; approved items work through egress (placeholders) or browser_fill (logins)." })
            .to_string(),
    )
}

async fn relay_status(agent: &Agent, request_id: &str) -> Result<String, String> {
    let v = relay_get(agent, &format!("/vault/requests/{request_id}")).await?;
    Ok(v["status"].as_str().unwrap_or("pending").to_owned())
}

/// Wait for the decision. The relay row (`vault_requests`, decided with a device token) is the
/// authority: Settings → Vault writes it directly, and a client that approves the chat card
/// forwards the decision there (`vaultRequestId` in the card payload). Whichever way the user
/// answers, the other surface is settled too: a relay decision flips the card, a rejected card
/// ends the wait at once. `Ok(None)` when the budget runs out with the request still pending.
async fn wait_decision(
    agent: &Arc<Agent>,
    request_id: &str,
    approval_id: Option<&str>,
    mut card: Option<oneshot::Receiver<ApprovalOutcome>>,
    budget: Duration,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    loop {
        let status = relay_status(agent, request_id).await?;
        if status != "pending" {
            if let Some(id) = approval_id {
                let outcome = match status.as_str() {
                    "approved" => ApprovalOutcome::Approved,
                    "denied" => ApprovalOutcome::Rejected {
                        reason: Some("denied in Settings → Vault".into()),
                    },
                    _ => ApprovalOutcome::Expired,
                };
                // Already decided from the card itself is fine: first decision wins.
                let _ = agent.decide_approval(id, outcome, "arbos:vault").await;
            }
            return Ok(Some(status));
        }
        if start.elapsed() >= budget {
            return Ok(None);
        }
        match card.as_mut() {
            Some(rx) => {
                tokio::select! {
                    res = rx => {
                        card = None;
                        match res {
                            Ok(ApprovalOutcome::Approved) => {
                                // The client forwards the approval to the relay; keep polling.
                            }
                            Ok(ApprovalOutcome::Rejected { .. }) => return Ok(Some("denied".into())),
                            Ok(ApprovalOutcome::Expired) | Err(_) => return Ok(Some("expired".into())),
                        }
                    }
                    _ = tokio::time::sleep(POLL) => {}
                }
            }
            None => tokio::time::sleep(POLL).await,
        }
    }
}

/// Create the relay request, post the approval card, wait for the user. Shared by
/// `vault_request` and `browser_fill`.
async fn obtain_grant(
    agent: &Arc<Agent>,
    chat_id: &str,
    name: &str,
    purpose: &str,
    use_kind: &str,
) -> Result<Grant, Value> {
    if let Some(g) = grant_for(&agent.cfg.project_id, name, use_kind) {
        return Ok(g);
    }
    let list = items(agent).await.map_err(err_result)?;
    let Some(item) = list.iter().find(|it| it["name"].as_str() == Some(name)) else {
        let names: Vec<&str> = list.iter().filter_map(|it| it["name"].as_str()).collect();
        return Err(err_result(format!(
            "no vault item named {name}; available: {}",
            if names.is_empty() {
                "(none; the user adds items in Settings → Vault)".to_owned()
            } else {
                names.join(", ")
            }
        )));
    };
    let kind = item["kind"].as_str().unwrap_or("secret").to_owned();
    if use_kind == "browser_fill" && kind != "login" {
        return Err(err_result(format!(
            "{name} is a {kind}; browser_fill needs a login item"
        )));
    }
    let hosts: Vec<String> = item["allowed_hosts"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| h.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let placeholders: BTreeMap<String, String> = item["placeholders"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let key = (
        agent.cfg.project_id.clone(),
        name.to_owned(),
        use_kind.to_owned(),
    );
    // Resume an undecided request from an earlier call (its card is still in the chat).
    let mut resumed = pending().lock().expect("pending").get(&key).cloned();
    if let Some(p) = &resumed {
        match relay_status(agent, &p.request_id).await {
            Ok(s) if s == "pending" => {}
            _ => {
                pending().lock().expect("pending").remove(&key);
                resumed = None;
            }
        }
    }
    let (request_id, approval_id, card) = match resumed {
        Some(p) => (p.request_id, p.approval_id, None),
        None => {
            let created = relay_post(
                agent,
                "/vault/request",
                &json!({ "name": name, "purpose": purpose, "use": use_kind, "chatId": chat_id }),
            )
            .await
            .map_err(err_result)?;
            let request_id = created["id"].as_str().unwrap_or("").to_owned();
            agent.append_audit(
                "vault_requested",
                json!({ "requestId": request_id, "item": name, "kind": kind, "use": use_kind, "purpose": purpose, "chatId": chat_id }),
            );
            let _ = agent.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                "vault_request",
                Some("assistant"),
                &format!("vault_request {name} ({use_kind}): {purpose}"),
                None,
            );
            // One approval card (S18 shape) per request. `vaultRequestId` lets the client forward
            // the card's decision to the relay row, which stays the authority (`mcp.rs` skips its
            // generic gate for the vault tools so this is the only card).
            let (tx, rx) = oneshot::channel();
            let req = ApprovalRequest {
                title: format!("Use vault item {name}"),
                summary: format!("{purpose} — {use_kind}, hosts: {}", hosts.join(", ")),
                risk: Risk::Credential,
                payload: json!({ "kind": "vault_request", "vaultRequestId": request_id, "item": name, "itemKind": kind, "use": use_kind, "allowed_hosts": hosts }),
            };
            let approval_id = match agent
                .begin_approval(chat_id, req, WaitMode::Inline, Some(tx))
                .await
            {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(error = %e, item = name, "vault: approval card could not be posted; Settings → Vault still decides");
                    None
                }
            };
            pending().lock().expect("pending").insert(
                key.clone(),
                PendingRequest {
                    request_id: request_id.clone(),
                    approval_id: approval_id.clone(),
                },
            );
            (request_id, approval_id, Some(rx))
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
            json!({ "status": "pending", "requestId": request_id, "item": name, "use": use_kind,
                "note": "The user has not decided yet (the card in this chat, or Arbos → Settings → Vault). Tell them you are waiting for approval and end the turn; calling vault_request again with the same name resumes waiting and returns at once once it is approved." })
                .to_string(),
        ));
    };
    pending().lock().expect("pending").remove(&key);
    agent.append_audit(
        "vault_decided",
        json!({ "requestId": request_id, "item": name, "use": use_kind, "status": status, "decidedBy": "user" }),
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
                "vault_request",
                &format!("vault {name} ({use_kind}): {status}"),
                &json!({ "requestId": request_id, "item": name, "use": use_kind, "status": status, "decidedAt": now_ms() }),
            );
        }
    }
    if status != "approved" {
        return Err(err_result(format!(
            "vault request for {name} was {status} by the user"
        )));
    }
    let g = Grant {
        project_id: agent.cfg.project_id.clone(),
        request_id,
        name: name.to_owned(),
        kind,
        use_kind: use_kind.to_owned(),
        env: item["env"].as_str().unwrap_or("ARBOS_SECRET").to_owned(),
        placeholders,
        hosts,
    };
    remember_grant(g.clone());
    Ok(g)
}

pub async fn vault_request(agent: &Arc<Agent>, args: &Value, chat_id: &str) -> Value {
    let name = args["name"].as_str().unwrap_or("").trim().to_owned();
    let purpose = args["purpose"].as_str().unwrap_or("").trim().to_owned();
    let use_kind = args["use"].as_str().unwrap_or("header").trim().to_owned();
    if name.is_empty() {
        return err_result("name is required (see vault_list)");
    }
    if !matches!(use_kind.as_str(), "env" | "header" | "browser_fill") {
        return err_result("use must be header, env or browser_fill");
    }
    match obtain_grant(agent, chat_id, &name, &purpose, &use_kind).await {
        Ok(g) => text_result(g.describe().to_string()),
        Err(v) => v,
    }
}

/// `$ARBOS_SECRET_X` / `${ARBOS_SECRET_X}` → the granted placeholder, so shell-style references work.
fn expand_env_refs(project_id: &str, text: &str) -> String {
    let mut out = text.to_owned();
    for g in grants().lock().expect("grants").iter() {
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
            .filter(|t| t.starts_with("arbos-secret://"))
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
        .json(&json!({ "url": url, "method": method, "headers": headers, "body": body, "placeholders": placeholders }))
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
        return err_result(format!("egress: {text}"));
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
    let grant = match obtain_grant(
        agent,
        chat_id,
        &name,
        &format!("sign in at {url}"),
        "browser_fill",
    )
    .await
    {
        Ok(g) => g,
        Err(v) => return v,
    };
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    if !grant.hosts.is_empty()
        && !grant.hosts.iter().any(|h| {
            let h = h.to_ascii_lowercase();
            host.eq_ignore_ascii_case(&h)
                || h.strip_prefix("*.")
                    .is_some_and(|suf| host.to_ascii_lowercase().ends_with(suf))
        })
    {
        return err_result(format!(
            "{host} is not an allowed host for {name} (allowed: {})",
            grant.hosts.join(", ")
        ));
    }
    // The values arrive now and are gone when this function returns.
    let values = match relay_post(
        agent,
        &format!("/vault/requests/{}/values", grant.request_id),
        &json!({}),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            // The approval window may have closed; drop the grant so the next call asks again.
            grants()
                .lock()
                .expect("grants")
                .retain(|g| g.request_id != grant.request_id);
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
        json!({ "item": name, "url": url, "filled": filled, "simulated": simulated, "ms": started.elapsed().as_millis() as i64, "chatId": chat_id }),
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
        json!({ "ok": !filled.is_empty(), "item": name, "url": url, "filled": filled, "simulated": simulated, "notes": notes,
            "note": "Arbos typed the values; they are not available to you. Continue in the browser (submit the form, read the result)." })
            .to_string(),
    )
}
