//! Memory as decision models (design §6.3, memory-decision-models §5): the write
//! pass (gate → candidates → IGNORE/ADD/UPDATE/MERGE/INVALIDATE) and chat
//! compaction with its write pass (§6.6). Both run off the reply path.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use arbos_core::ids::now_ms;

use crate::config::Config;
use crate::llm::Llm;
use crate::store::{MemoryRow, Store};

pub const COMPACTION_TURNS: i64 = 20;
pub const COMPACTION_TOKENS: usize = 24_000;
pub const VERBATIM_TURNS: usize = 12;
pub const TOOL_OUTPUT_CAP_FOR_SUMMARISER: usize = 2_000;

const WRITE_SYSTEM: &str = r#"You are the memory write decision for a project agent. Decide what, if anything, the project should remember from the event below.
First a gate: is anything here worth storing beyond the raw transcript (a decision, a durable fact, a user preference or correction, a project reference such as a path or URL, or a status that others need)? Transient chatter, tool noise, and things already obvious from the files are not store-worthy.
Then extract at most 8 candidate records. For each candidate compare with the SIMILAR existing records and choose one action:
- IGNORE: not worth storing or already stored.
- ADD: new.
- UPDATE: refines one existing record (give its id in target_ids).
- MERGE: replaces two or more existing records with one (give all ids).
- INVALIDATE: contradicts an existing record (give its id); the new text is stored as a new record.
Fields per candidate: text (one or two self-contained sentences), kind (user|feedback|project|reference|decision|status), scope (user|project|thread), tier (T1 for project-level facts and decisions, T2 for thread-level status and facts), action, target_ids, reason (one line), confidence (0-1), importance (1-10).
Output: {"gate":{"store_worthy":bool,"confidence":0-1},"decisions":[...]}"#;

const COMPACTION_SYSTEM: &str = r#"You write the rolling summary of one chat between a user and a project agent. You receive the previous summary and the turns that are leaving the verbatim window. Write a new summary (at most ~1500 words) that carries the old one forward.
Preserve, in this priority: decisions and their reasons; open items and questions; user corrections; artifacts (file paths, URLs, commit ids, names); who did what (thread ids). Drop pleasantries, tool chatter, superseded drafts.
Handles listed in the working set stay available; record which ones matter, do not restate their content.
Output: {"summary":"...","candidates":[{"text":"...","kind":"decision|project|reference|status|user|feedback","scope":"project|thread"}]}"#;

pub struct Memory {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub llm: Arc<Llm>,
}

impl Memory {
    pub fn new(cfg: Arc<Config>, store: Store, llm: Arc<Llm>) -> Self {
        Self { cfg, store, llm }
    }

    /// T0 + T1 whole, as Markdown lines for the prompt (design §6.4 item 2).
    pub fn core_memory_text(&self) -> String {
        let mut out = String::new();
        let t0 = self.store.memory_live("T0", None, 40).unwrap_or_default();
        let t1 = self.store.memory_live("T1", None, 80).unwrap_or_default();
        if !t0.is_empty() {
            out.push_str("## User core\n");
            for m in t0 {
                out.push_str(&format!("- {}\n", m.text));
            }
        }
        if !t1.is_empty() {
            out.push_str("## Project core\n");
            for m in t1 {
                out.push_str(&format!("- [{}] {}\n", m.kind, m.text));
            }
        }
        let threads = self.store.subagents().unwrap_or_default();
        if !threads.is_empty() {
            out.push_str("## Threads\n");
            for t in threads.iter().take(30) {
                out.push_str(&format!(
                    "- {} · {} · {} · {} · {}\n",
                    t.id,
                    t.name,
                    t.purpose,
                    t.status,
                    t.summary.chars().take(120).collect::<String>()
                ));
            }
        }
        out
    }

    /// Write-side decision over one event (user message, assistant reply, subagent turn end).
    pub async fn write_pass(
        &self,
        event_id: &str,
        thread_id: &str,
        source_role: &str,
        text: &str,
        proposals: &[Value],
    ) -> Result<()> {
        let text: String = text.chars().take(12_000).collect();
        if text.trim().len() < 20 && proposals.is_empty() {
            return Ok(());
        }
        // Similar records: a coarse FTS pass over the event text (top-5 per tier).
        let mut similar: Vec<MemoryRow> = Vec::new();
        for tier in ["T1", "T2"] {
            if let Ok(hits) = self.store.memory_search(
                &text.chars().take(400).collect::<String>(),
                &[tier.to_owned()],
                false,
                5,
            ) {
                similar.extend(hits.into_iter().map(|(m, _)| m));
            }
        }
        let similar_json: Vec<Value> = similar.iter().map(|m| json!({ "id": m.id, "tier": m.tier, "kind": m.kind, "text": m.text, "created_at": m.created_at })).collect();
        let user = json!({
            "event": { "id": event_id, "thread_id": thread_id, "role": source_role, "text": text },
            "proposed_by_subagent": proposals,
            "similar": similar_json,
        })
        .to_string();
        let (v, ms) = match self
            .llm
            .complete_json(&self.cfg.decision_model, WRITE_SYSTEM, &user, 1500)
            .await
        {
            Ok(x) => x,
            Err(e) => {
                // Fail open and count it (design §6.3 logging).
                let _ = self.store.log_decision(
                    "write",
                    Some(event_id),
                    Some(&json!({ "error": e.to_string() })),
                    None,
                    &[],
                    Some("decision model failed"),
                    None,
                    Some(&self.cfg.decision_model),
                    None,
                );
                return Ok(());
            }
        };
        let gate = v.get("gate").cloned().unwrap_or(json!({}));
        let store_worthy = gate
            .get("store_worthy")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !store_worthy {
            self.store.log_decision(
                "write",
                Some(event_id),
                Some(&gate),
                Some("IGNORE"),
                &[],
                Some("gate: not store-worthy"),
                gate.get("confidence").and_then(Value::as_f64),
                Some(&self.cfg.decision_model),
                Some(ms),
            )?;
            return Ok(());
        }
        for d in v
            .get("decisions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(8)
        {
            let action = d
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("IGNORE")
                .to_ascii_uppercase();
            let targets: Vec<String> = d
                .get("target_ids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let text = d
                .get("text")
                .or(d.get("new_text"))
                .or(d.get("candidate"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            let kind = d.get("kind").and_then(Value::as_str).unwrap_or("project");
            let scope = d.get("scope").and_then(Value::as_str).unwrap_or("project");
            let tier = d
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or(if scope == "thread" { "T2" } else { "T1" });
            let conf = d.get("confidence").and_then(Value::as_f64).unwrap_or(0.5);
            let imp = d
                .get("importance")
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .clamp(1, 10);
            let reason = d.get("reason").and_then(Value::as_str);
            let thread = if tier == "T2" { Some(thread_id) } else { None };
            let source =
                json!({ "event_id": event_id, "thread_id": thread_id, "role": source_role });
            let mut touched = targets.clone();
            match action.as_str() {
                "ADD" if !text.is_empty() => {
                    let id = self.store.memory_add(
                        tier,
                        thread,
                        kind,
                        scope,
                        &text,
                        Some(&source),
                        Some(now_ms()),
                        conf,
                        imp,
                        &[],
                    )?;
                    touched.push(id);
                }
                "UPDATE" | "MERGE" if !text.is_empty() && !targets.is_empty() => {
                    for t in &targets {
                        self.store.memory_invalidate(t, None)?;
                    }
                    let id = self.store.memory_add(
                        tier,
                        thread,
                        kind,
                        scope,
                        &text,
                        Some(&source),
                        Some(now_ms()),
                        conf,
                        imp,
                        &targets,
                    )?;
                    touched.push(id);
                }
                "INVALIDATE" if !targets.is_empty() => {
                    for t in &targets {
                        self.store.memory_invalidate(t, Some(now_ms()))?;
                    }
                    if !text.is_empty() {
                        let id = self.store.memory_add(
                            tier,
                            thread,
                            kind,
                            scope,
                            &text,
                            Some(&source),
                            Some(now_ms()),
                            conf,
                            imp,
                            &[],
                        )?;
                        touched.push(id);
                    }
                }
                _ => {}
            }
            self.store.log_decision(
                "write",
                Some(event_id),
                Some(&gate),
                Some(&action),
                &touched,
                reason,
                Some(conf),
                Some(&self.cfg.decision_model),
                Some(ms),
            )?;
        }
        Ok(())
    }

    /// Compaction trigger (design §6.6): 20 turns past `compacted_through` or > 24k tokens in summary + window.
    pub async fn maybe_compact(&self, chat_id: &str) -> Result<()> {
        let Some(chat) = self.store.chat(chat_id)? else {
            return Ok(());
        };
        let turns = self
            .store
            .t3_thread(chat_id, chat.compacted_through.as_deref(), None, 400)?;
        let turns: Vec<_> = turns
            .into_iter()
            .filter(|t| t.kind == "user" || t.kind == "assistant")
            .collect();
        let approx_tokens = (chat
            .compaction_summary
            .as_deref()
            .map(str::len)
            .unwrap_or(0)
            + turns.iter().map(|t| t.text.len()).sum::<usize>())
            / 4;
        if (turns.len() as i64) < COMPACTION_TURNS && approx_tokens < COMPACTION_TOKENS {
            return Ok(());
        }
        if turns.len() <= VERBATIM_TURNS {
            return Ok(());
        }
        let leaving = &turns[..turns.len() - VERBATIM_TURNS];
        let through = leaving.last().map(|t| t.id.clone()).unwrap_or_default();
        let mut body = String::new();
        body.push_str("## Previous summary\n");
        body.push_str(chat.compaction_summary.as_deref().unwrap_or("(none)"));
        body.push_str("\n\n## Turns leaving the window\n");
        for t in leaving {
            let text: String = if t.kind == "tool_output" {
                let s = &t.text;
                if s.len() > TOOL_OUTPUT_CAP_FOR_SUMMARISER {
                    format!(
                        "{}…{}",
                        &s[..1500.min(s.len())],
                        &s[s.len().saturating_sub(500)..]
                    )
                } else {
                    s.clone()
                }
            } else {
                t.text.clone()
            };
            body.push_str(&format!(
                "[{}] {}\n",
                t.role.as_deref().unwrap_or(&t.kind),
                text
            ));
        }
        let (v, _ms) = self
            .llm
            .complete_json(&self.cfg.talking_model, COMPACTION_SYSTEM, &body, 2500)
            .await?;
        let summary = v
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if summary.is_empty() {
            return Ok(());
        }
        self.store
            .set_chat_compaction(chat_id, &summary, &through)?;
        let _ = self.store.t3_append(
            chat_id,
            None,
            None,
            "compaction",
            Some("system"),
            &format!("chat compacted through {through}: {} chars", summary.len()),
            None,
        );
        let candidates = v
            .get("candidates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !candidates.is_empty() {
            self.write_pass(
                &format!("compaction:{through}"),
                chat_id,
                "compaction",
                &summary,
                &candidates,
            )
            .await?;
        }
        Ok(())
    }
}
