//! Small model calls from agentd's own Rust code (memory decisions, compaction, sub_llm),
//! always through the relay proxy with the box token (D2).

use std::time::Instant;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::config::Config;

pub struct Llm {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Llm {
    pub fn new(cfg: &Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: format!("{}/proxy/v1", cfg.relay_url),
            token: cfg.box_token.clone(),
        }
    }

    /// One non-streaming chat completion. Returns `(text, latency_ms)`.
    pub async fn complete(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<(String, i64)> {
        let t0 = Instant::now();
        let body = json!({
            "model": model,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
            "max_tokens": max_tokens,
            "temperature": 0,
        });
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(anyhow!("model call failed: HTTP {status}: {}", v));
        }
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok((text, t0.elapsed().as_millis() as i64))
    }

    /// A forced-JSON call: strips code fences and parses the first JSON object.
    pub async fn complete_json(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<(Value, i64)> {
        let (text, ms) = self
            .complete(
                model,
                &format!("{system}\nAnswer with one JSON object and nothing else."),
                user,
                max_tokens,
            )
            .await?;
        let v = parse_json_loose(&text).ok_or_else(|| {
            anyhow!(
                "model did not return JSON: {}",
                text.chars().take(200).collect::<String>()
            )
        })?;
        Ok((v, ms))
    }
}

/// Find the first `{…}` or `[…]` in model output and parse it.
pub fn parse_json_loose(text: &str) -> Option<Value> {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let start = t.find(['{', '['])?;
    let open = t.as_bytes()[start] as char;
    let close = if open == '{' { '}' } else { ']' };
    let end = t.rfind(close)?;
    serde_json::from_str(&t[start..=end]).ok()
}

/// Extract the return-contract JSON block from a subagent's final reply (design §6.7).
pub fn extract_return_contract(reply: &str) -> Option<Value> {
    let idx = reply.rfind("```json")?;
    let rest = &reply[idx + 7..];
    let end = rest.find("```")?;
    serde_json::from_str::<Value>(rest[..end].trim())
        .ok()
        .filter(|v| v.get("status").is_some())
}
