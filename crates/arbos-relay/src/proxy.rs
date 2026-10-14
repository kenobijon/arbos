//! Model proxy (D2): OpenAI-compatible and Anthropic-compatible passthrough.
//! The box authenticates with its box token; the relay adds the real provider
//! key, streams bytes through untouched, and meters tokens + USD per project.

use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::auth::Principal;
use crate::state::AppState;

#[derive(Debug, Clone, Copy)]
enum Api {
    OpenAi,
    Anthropic,
}

#[derive(Default, Debug)]
struct Meter {
    model: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: Option<f64>,
}

fn start_of_utc_day_ms() -> i64 {
    let now = arbos_core::ids::now_ms();
    now - now.rem_euclid(86_400_000)
}

/// Rough USD per million tokens when the upstream does not report cost (Anthropic direct).
fn price(model: &str) -> (f64, f64) {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        (4.0, 20.0)
    } else if m.contains("sonnet") {
        (2.0, 10.0)
    } else if m.contains("haiku") {
        (0.1, 0.5)
    } else if m.contains("mini") || m.contains("nano") || m.contains("flash") {
        (0.2, 1.25)
    } else {
        (1.0, 5.0)
    }
}

impl Meter {
    fn usd(&self) -> f64 {
        if let Some(c) = self.cost {
            return c;
        }
        let (i, o) = price(&self.model);
        (self.input as f64 * i
            + self.output as f64 * o
            + self.cache_read as f64 * i * 0.1
            + self.cache_write as f64 * i * 1.25)
            / 1_000_000.0
    }

    fn absorb_openai_usage(&mut self, u: &Value) {
        if let Some(n) = u.get("prompt_tokens").and_then(Value::as_i64) {
            self.input = n;
        }
        if let Some(n) = u.get("completion_tokens").and_then(Value::as_i64) {
            self.output = n;
        }
        if let Some(n) = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
        {
            self.cache_read = n;
            self.input -= n.min(self.input);
        }
        if let Some(n) = u
            .pointer("/prompt_tokens_details/cache_write_tokens")
            .and_then(Value::as_i64)
        {
            self.cache_write = n;
        }
        if let Some(c) = u.get("cost").and_then(Value::as_f64) {
            self.cost = Some(c);
        }
    }

    fn absorb_anthropic_usage(&mut self, u: &Value) {
        if let Some(n) = u.get("input_tokens").and_then(Value::as_i64) {
            self.input = n;
        }
        if let Some(n) = u.get("output_tokens").and_then(Value::as_i64) {
            self.output = n;
        }
        if let Some(n) = u.get("cache_read_input_tokens").and_then(Value::as_i64) {
            self.cache_read = n;
        }
        if let Some(n) = u.get("cache_creation_input_tokens").and_then(Value::as_i64) {
            self.cache_write = n;
        }
    }

    /// Scan one SSE event or JSON body for usage.
    fn scan(&mut self, api: Api, text: &str) {
        for line in text.lines() {
            let payload = line
                .strip_prefix("data:")
                .map(str::trim)
                .unwrap_or(line.trim());
            if payload.is_empty() || payload == "[DONE]" || !payload.starts_with('{') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            if let Some(m) = v.get("model").and_then(Value::as_str) {
                self.model = m.to_owned();
            }
            match api {
                Api::OpenAi => {
                    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                        self.absorb_openai_usage(u);
                    }
                }
                Api::Anthropic => {
                    if let Some(u) = v.pointer("/message/usage") {
                        self.absorb_anthropic_usage(u);
                        if let Some(m) = v.pointer("/message/model").and_then(Value::as_str) {
                            self.model = m.to_owned();
                        }
                    }
                    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                        self.absorb_anthropic_usage(u);
                    }
                }
            }
        }
    }
}

fn project_of(p: &Principal) -> Option<String> {
    match p {
        Principal::Box { project_id } => Some(project_id.clone()),
        Principal::Device { .. } | Principal::Admin => None,
    }
}

const PASS_HEADERS: &[&str] = &[
    "anthropic-version",
    "anthropic-beta",
    "accept",
    "content-type",
    "x-title",
    "http-referer",
];

async fn forward(
    state: AppState,
    principal: Principal,
    headers: HeaderMap,
    api: Api,
    path_tail: &str,
    mut body: Bytes,
    project_override: Option<String>,
) -> Response {
    let project_id = project_of(&principal)
        .or(project_override)
        .unwrap_or_else(|| "relay-admin".to_owned());
    if let Ok(Some(p)) = state.db.project(&project_id) {
        let today = state
            .db
            .usage_since(&project_id, start_of_utc_day_ms())
            .map(|u| u.usd)
            .unwrap_or(0.0);
        if today >= p.spend_cap_daily {
            let _ = state.db.enqueue_push(
                &project_id,
                "spend_cap",
                &json!({ "spend_today": today, "cap": p.spend_cap_daily }),
            );
            return (StatusCode::PAYMENT_REQUIRED, json!({ "error": { "type": "spend_cap", "message": format!("daily spend cap ${:.2} reached", p.spend_cap_daily) } }).to_string()).into_response();
        }
    }

    let mut req_json: Option<Value> = serde_json::from_slice(&body).ok();
    let mut model = req_json
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let is_stream = req_json
        .as_ref()
        .and_then(|v| v.get("stream"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (url, key, key_header): (String, String, &str) = match api {
        Api::OpenAi => {
            let base = state.cfg.openai_base.trim_end_matches('/').to_owned();
            let key = if base.contains("openrouter") {
                state.cfg.openrouter_key.clone()
            } else {
                state
                    .cfg
                    .openai_key
                    .clone()
                    .or(state.cfg.openrouter_key.clone())
            };
            let Some(key) = key else {
                return (
                    StatusCode::BAD_GATEWAY,
                    "relay has no OpenAI-compatible provider key",
                )
                    .into_response();
            };
            if is_stream {
                if let Some(v) = req_json.as_mut() {
                    v["stream_options"] = json!({ "include_usage": true });
                    if base.contains("openrouter") {
                        v["usage"] = json!({ "include": true });
                    }
                    body = Bytes::from(v.to_string());
                }
            } else if base.contains("openrouter") {
                if let Some(v) = req_json.as_mut() {
                    v["usage"] = json!({ "include": true });
                    body = Bytes::from(v.to_string());
                }
            }
            (format!("{base}/{path_tail}"), key, "authorization")
        }
        Api::Anthropic => {
            let base = state.cfg.anthropic_base.trim_end_matches('/').to_owned();
            if base.contains("openrouter") {
                let Some(key) = state.cfg.openrouter_key.clone() else {
                    return (StatusCode::BAD_GATEWAY, "relay has no OpenRouter key")
                        .into_response();
                };
                if !model.contains('/') && !model.is_empty() {
                    model = format!("anthropic/{model}");
                    if let Some(v) = req_json.as_mut() {
                        v["model"] = json!(model);
                        body = Bytes::from(v.to_string());
                    }
                }
                (format!("{base}/{path_tail}"), key, "authorization")
            } else {
                let Some(key) = state.cfg.anthropic_key.clone() else {
                    return (StatusCode::BAD_GATEWAY, "relay has no Anthropic key").into_response();
                };
                (format!("{base}/{path_tail}"), key, "x-api-key")
            }
        }
    };

    let mut req = state.http.post(&url).body(body);
    for h in PASS_HEADERS {
        if let Some(v) = headers.get(*h) {
            req = req.header(*h, v);
        }
    }
    req = if key_header == "authorization" {
        req.bearer_auth(&key)
    } else {
        req.header("x-api-key", key)
    };
    if matches!(api, Api::Anthropic) && headers.get("anthropic-version").is_none() {
        req = req.header("anthropic-version", "2023-06-01");
    }
    if headers.get("content-type").is_none() {
        req = req.header("content-type", "application/json");
    }

    let t0 = Instant::now();
    let upstream = match req.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream: {e}")).into_response(),
    };
    let status = upstream.status();
    let mut resp_headers = HeaderMap::new();
    for (k, v) in upstream.headers() {
        let name = k.as_str();
        if name == "content-type"
            || name == "cache-control"
            || name.starts_with("x-ratelimit")
            || name == "openai-processing-ms"
        {
            resp_headers.insert(HeaderName::from_bytes(name.as_bytes()).unwrap(), v.clone());
        }
    }
    resp_headers.insert("x-arbos-proxied", HeaderValue::from_static("1"));

    let api_name = match api {
        Api::OpenAi => "openai",
        Api::Anthropic => "anthropic",
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let db = state.db.clone();
    let pid = project_id.clone();
    tokio::spawn(async move {
        let mut meter = Meter {
            model: model.clone(),
            ..Default::default()
        };
        let mut ttfb: Option<i64> = None;
        let mut stream = upstream.bytes_stream();
        let mut carry = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => {
                    if ttfb.is_none() {
                        ttfb = Some(t0.elapsed().as_millis() as i64);
                    }
                    carry.push_str(&String::from_utf8_lossy(&b));
                    if let Some(idx) = carry.rfind('\n') {
                        let (done, rest) = carry.split_at(idx + 1);
                        meter.scan(api, done);
                        carry = rest.to_owned();
                    }
                    if tx.send(Ok(b)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e))).await;
                    break;
                }
            }
        }
        if !carry.is_empty() {
            meter.scan(api, &carry);
        }
        if status.is_success() {
            let _ = db.record_usage(
                &pid,
                api_name,
                &meter.model,
                meter.input,
                meter.output,
                meter.cache_read,
                meter.cache_write,
                meter.usd(),
                t0.elapsed().as_millis() as i64,
                ttfb,
            );
        }
    });
    let body = Body::from_stream(tokio_stream_wrap(rx));
    (
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        resp_headers,
        body,
    )
        .into_response()
}

fn tokio_stream_wrap(
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

/// `POST /proxy/v1/chat/completions`, `/proxy/v1/responses`, `/proxy/v1/embeddings`, … (OpenAI-compatible).
pub async fn openai(
    State(state): State<AppState>,
    principal: Principal,
    Path(tail): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let project_override = headers
        .get("x-arbos-project")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    forward(
        state,
        principal,
        headers,
        Api::OpenAi,
        &tail,
        body,
        project_override,
    )
    .await
}

/// `POST /proxy/anthropic/v1/messages` (Anthropic-compatible; Claude Code's `ANTHROPIC_BASE_URL` lands here).
pub async fn anthropic(
    State(state): State<AppState>,
    principal: Principal,
    Path(tail): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let project_override = headers
        .get("x-arbos-project")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    forward(
        state,
        principal,
        headers,
        Api::Anthropic,
        &tail,
        body,
        project_override,
    )
    .await
}

/// `GET /proxy/v1/models` passthrough (pi's model discovery).
pub async fn openai_models(State(state): State<AppState>, _principal: Principal) -> Response {
    let base = state.cfg.openai_base.trim_end_matches('/');
    let key = if base.contains("openrouter") {
        state.cfg.openrouter_key.clone()
    } else {
        state.cfg.openai_key.clone()
    };
    let Some(key) = key else {
        return (StatusCode::BAD_GATEWAY, "no key").into_response();
    };
    match state
        .http
        .get(format!("{base}/models"))
        .bearer_auth(key)
        .send()
        .await
    {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let b = r.bytes().await.unwrap_or_default();
            (status, [("content-type", "application/json")], b).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}
