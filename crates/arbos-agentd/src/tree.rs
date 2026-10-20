//! Memory tree (D77): a read-only side index over the Project's own T3 history.
//!
//! Leaves are the existing `t3` rows (every message, tool call, tool result, subagent step);
//! nothing in `t3` is ever changed. A background builder groups leaves into episodes, episodes
//! into threads (chats, DMs, groups, subagent sessions) under the Project root, and writes one
//! index card per node (heading, summary, entities) with the project's decision-model slot.
//! Big tool results also get a page card (what the page/output is, its salient labels and
//! numbers). Before every reply and every subagent launch, the decision model (Jev through the
//! relay's `/proxy/decisions`) walks the tree top-down — one request per sibling group, all
//! siblings scored in parallel — expands what scores above a per-content-type threshold, adds
//! keyword hits as extra leaf candidates, and packs the chosen raw leaves verbatim under their
//! headings. The pack is injected into the prompt (mode `on`), only logged (mode `log`), or
//! skipped (`off`). Every walk is logged next to what the model had anyway (the pi session's
//! recent window), with Jev scores, tokens, cost and latency. A per-box daily spend cap stops
//! both the builder and the walk; the walk then falls back to today's behaviour.
//!
//! Each Project's store holds only its own history, so the tree is private to that agent by
//! construction. Group and people chats see only their own subtree.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures::future::join_all;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use arbos_core::ids::{now_ms, ulid};

use crate::config::Config;
use crate::llm::Llm;
use crate::store::{Store, T3Row};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tree_nodes (
  id TEXT PRIMARY KEY, level TEXT NOT NULL, thread_id TEXT, parents TEXT NOT NULL DEFAULT '[]',
  children TEXT NOT NULL DEFAULT '[]', heading TEXT NOT NULL DEFAULT '', summary TEXT NOT NULL DEFAULT '',
  entities TEXT NOT NULL DEFAULT '[]', t0 INTEGER NOT NULL DEFAULT 0, t1 INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'chat', status TEXT NOT NULL DEFAULT 'open',
  title TEXT NOT NULL DEFAULT '', updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tree_nodes_level ON tree_nodes(level, thread_id);
CREATE TABLE IF NOT EXISTS tree_page_cards (
  page_key TEXT PRIMARY KEY, page TEXT NOT NULL, facts TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tree_leaf_pages (leaf_id TEXT PRIMARY KEY, page_key TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tree_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tree_cost (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, usd REAL NOT NULL, tokens_in INTEGER NOT NULL, tokens_out INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tree_cost_at ON tree_cost(created_at);
CREATE TABLE IF NOT EXISTS tree_walk_log (
  id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, message_id TEXT, purpose TEXT NOT NULL, mode TEXT NOT NULL, outcome TEXT NOT NULL,
  reason TEXT, query TEXT, pack TEXT, decisions TEXT, jev_calls INTEGER NOT NULL DEFAULT 0, jev_cost REAL NOT NULL DEFAULT 0,
  jev_ms INTEGER NOT NULL DEFAULT 0, walk_ms INTEGER NOT NULL DEFAULT 0, pack_tokens INTEGER NOT NULL DEFAULT 0,
  recent_window TEXT, created_at INTEGER NOT NULL
);
"#;

const EPISODE_GAP_MS: i64 = 30 * 60 * 1000;
const SETTLE_MS: i64 = 90 * 1000;
const THREAD_CARD_DEBOUNCE_MS: i64 = 10 * 60 * 1000;
const ROOT_CARD_DEBOUNCE_MS: i64 = 30 * 60 * 1000;
/// A card call that failed (model answered no JSON, upstream error) is retried no sooner than
/// this. Without it a node whose card keeps failing was retried on every 60 s build pass — one
/// Haiku call a minute per workspace, forever, with nothing new to summarise.
const CARD_FAIL_BACKOFF_MS: i64 = 60 * 60 * 1000;
const MAX_CARDS_PER_PASS: usize = 40;
const PAGE_CARD_MIN_CHARS: usize = 1500;
const PAGE_CARD_CLIP: usize = 7000;
const TOOL_CLIP_FOR_CARDS: usize = 1500;
const LEAF_CLIP_FOR_JEV: usize = 1200;
const JEV_CHUNK: usize = 24;
const BM25_EXTRA: i64 = 20;
const WALK_DEADLINE_MS: u64 = 2500;
const JEV_TIMEOUT_MS: u64 = 1500;
// Pack budgets (estimated tokens). "Full" is the pack as shipped with D77: one budget by content
// kind, raw leaf text clipped. "Compact" (`ARBOS_MEMORY_PACK_COMPACT`) is the D87 follow-up: with
// the prompt prefix stable the per-turn block is the largest fresh cache write of a turn, and
// Haiku writes cost 12.5× reads, so chat and tool leaves each get their own budget, the whole
// block is capped, a tool output that has a page card is packed as the card + facts only (the raw
// text is one `context_open` away), exact repeats and leaves the prompt's project-memory block
// already states are dropped.
const BUDGET_CHAT_FULL: usize = 6000;
const BUDGET_TOOL_FULL: usize = 12000;
const CLIP_CHAT: usize = 2000;
const CLIP_TOOL_FULL: usize = 6000;
const BUDGET_CHAT: usize = 4000;
const BUDGET_TOOL: usize = 3000;
const BUDGET_PACK: usize = 5000;
const CLIP_TOOL_PACK: usize = 1500;
/// Leaves shorter than this are never deduplicated against the prompt's project-memory block
/// (short lines match by accident).
const DEDUPE_MIN_CHARS: usize = 40;
const ROOT_ID: &str = "root";

const CARD_SYSTEM: &str = "You write index cards for an agent's memory tree. Given raw events (or child cards), return JSON with keys: \"heading\" (ONE line, <= 110 chars: who did what, and the outcome), \"summary\" (2-3 plain sentences with the concrete facts, names, numbers, decisions, file paths and results that someone searching later would need), \"entities\" (list of <= 10 short strings: people, projects, tools, repos, topics). Never invent facts. Prefer specifics over generalities. Output JSON only.";
const PAGE_CARD_SYSTEM: &str = "You index tool results an agent saw. Given a tool call and its output (clipped), return JSON {\"page\": \"<one line: what this output/page is and what it is for>\", \"facts\": [<up to 14 short strings>]}. The facts are the concrete things someone might later ask about: labels, option names in order, column headers, button names, prices, counts, dates, status values, error text, file names, record names. Quote them exactly. JSON only.";

/// Where the pack goes: into the prompt, only into the log, or nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMode {
    On,
    Log,
    Off,
}

impl TreeMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "inject" | "true" | "1" => Some(Self::On),
            "log" | "log_only" | "log-only" | "shadow" => Some(Self::Log),
            "off" | "false" | "0" | "none" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Log => "log",
            Self::Off => "off",
        }
    }
}

/// Content kind of a thread; picks the walk thresholds and the pack clip (chat vs tool history).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Chat,
    Tool,
}

impl Kind {
    fn parse(s: &str) -> Self {
        if s == "tool" {
            Self::Tool
        } else {
            Self::Chat
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Tool => "tool",
        }
    }
    /// Recall over precision: the benchmark sweeps kept full gold-leaf recall at these values
    /// (chat history 0.3–0.5; tool history needs lower thread/episode cuts), and card wording
    /// shifts Jev's probabilities by a few tenths, so the chat cuts sit at the low end.
    fn threshold(self, level: &str) -> f64 {
        match (self, level) {
            (Self::Chat, "thread") => 0.3,
            (Self::Chat, "episode") => 0.3,
            (Self::Chat, "leaf") => 0.4,
            (Self::Tool, "thread") => 0.2,
            (Self::Tool, "episode") => 0.3,
            (Self::Tool, "leaf") => 0.4,
            _ => 0.5,
        }
    }
    fn beam(self, level: &str) -> usize {
        match (self, level) {
            (Self::Chat, "thread") => 12,
            (Self::Chat, "episode") => 16,
            (Self::Tool, "thread") => 16,
            (Self::Tool, "episode") => 20,
            _ => 80,
        }
    }
    fn episode_max_tokens(self) -> usize {
        match self {
            Self::Chat => 2500,
            Self::Tool => 8000,
        }
    }
    fn episode_max_leaves(self) -> usize {
        match self {
            Self::Chat => 24,
            Self::Tool => 12,
        }
    }
    fn clip_full(self) -> usize {
        match self {
            Self::Chat => CLIP_CHAT,
            Self::Tool => CLIP_TOOL_FULL,
        }
    }
    fn clip(self) -> usize {
        match self {
            Self::Chat => CLIP_CHAT,
            Self::Tool => CLIP_TOOL_PACK,
        }
    }
    fn budget(self) -> usize {
        match self {
            Self::Chat => BUDGET_CHAT,
            Self::Tool => BUDGET_TOOL,
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    id: String,
    level: String,
    thread_id: Option<String>,
    parents: Vec<String>,
    children: Vec<String>,
    heading: String,
    summary: String,
    entities: Vec<String>,
    t0: i64,
    t1: i64,
    tokens: i64,
    kind: Kind,
    status: String,
    title: String,
    updated_at: i64,
}

/// The pack for one turn, plus everything the log needs.
#[derive(Debug, Clone, Default)]
pub struct Pack {
    pub text: String,
    pub tokens: usize,
    pub leaf_ids: Vec<String>,
    pub node_ids: Vec<String>,
    pub decisions: Vec<Value>,
    pub jev_calls: i64,
    pub jev_cost: f64,
    pub jev_ms: i64,
    pub walk_ms: i64,
    /// `injected` | `fallback` | `empty` | `logged`.
    pub outcome: String,
    pub reason: Option<String>,
}

pub struct Tree {
    project_id: String,
    project_name: String,
    /// Chat model for the cards (the project's `decision_model` slot).
    card_model: String,
    store: Store,
    llm: Arc<Llm>,
    build_lock: tokio::sync::Mutex<()>,
    cap_warned: std::sync::Mutex<Option<i64>>,
}

pub fn est_tokens(s: &str) -> usize {
    s.chars().count() / 4 + 1
}

fn fmt_ts(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default()
}

/// Lower-cased, whitespace-collapsed text for the pack's repeat / prompt-memory checks.
fn norm_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head} …[{} more chars]", s.chars().count() - max)
    }
}

fn sha16(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    hex::encode(&d[..8])
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "what", "which", "who", "whom", "when", "where",
    "why", "how", "was", "were", "are", "is", "did", "does", "do", "you", "your", "our", "its",
    "can", "could", "would", "should", "have", "has", "had", "from", "into", "about", "tell",
    "please", "there", "they", "them", "then", "than", "also", "just", "any", "some", "all", "not",
    "but", "yet", "again", "still", "ever", "me", "my",
];

/// Content words of a question, for the keyword candidates (OR query).
fn content_words(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in q
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
    {
        let w = w.trim_matches(|c: char| c == '.' || c == '-');
        if w.len() > 2 && !STOPWORDS.contains(&w) && !out.iter().any(|x| x == w) {
            out.push(w.to_owned());
        }
    }
    out.truncate(12);
    out
}

/// Parse a card; a truncated or fenced answer still yields heading + summary through a regex.
fn parse_card(text: &str) -> Option<(String, String, Vec<String>)> {
    let take = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    if let Some(v) = crate::llm::parse_json_loose(text) {
        if let Some(h) = v.get("heading").and_then(Value::as_str) {
            let entities = v
                .get("entities")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| take(s, 40))
                        .take(10)
                        .collect()
                })
                .unwrap_or_default();
            return Some((
                take(h, 160),
                take(v.get("summary").and_then(Value::as_str).unwrap_or(""), 900),
                entities,
            ));
        }
    }
    let re_h = regex_lite::Regex::new(r#""heading"\s*:\s*"((?:[^"\\]|\\.)*)""#).ok()?;
    let re_s = regex_lite::Regex::new(r#""summary"\s*:\s*"((?:[^"\\]|\\.)*)"#).ok()?;
    let h = re_h.captures(text)?.get(1)?.as_str().replace("\\\"", "\"");
    let s = re_s
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().replace("\\\"", "\""))
        .unwrap_or_default();
    Some((take(&h, 160), take(&s, 900), Vec::new()))
}

fn day_start_ms(now: i64) -> i64 {
    now - now.rem_euclid(86_400_000)
}

impl Tree {
    pub fn new(cfg: &Config, store: Store, llm: Arc<Llm>) -> Self {
        Self::with_ids(
            &cfg.project_id,
            &cfg.project_name,
            &cfg.decision_model,
            store,
            llm,
        )
    }

    pub fn with_ids(
        project_id: &str,
        project_name: &str,
        card_model: &str,
        store: Store,
        llm: Arc<Llm>,
    ) -> Self {
        let _ = store.exec_batch(SCHEMA);
        // Every card and walk call carries `x-arbos-purpose: memory-tree`, so the relay's usage
        // table and credit ledger show the feature's spend on its own.
        let llm = Arc::new(llm.with_purpose("memory-tree"));
        Self {
            project_id: project_id.to_owned(),
            project_name: project_name.to_owned(),
            card_model: card_model.to_owned(),
            store,
            llm,
            build_lock: tokio::sync::Mutex::new(()),
            cap_warned: std::sync::Mutex::new(None),
        }
    }

    // ---- settings -----------------------------------------------------------------------------

    /// Per-box daily cap on tree spend (cards + Jev), USD. `ARBOS_MEMORY_TREE_DAILY_CAP_USD`,
    /// default 0.80 so six boxes stay under the $5/day rule.
    pub fn daily_cap_usd() -> f64 {
        std::env::var("ARBOS_MEMORY_TREE_DAILY_CAP_USD")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v >= 0.0)
            .unwrap_or(0.80)
    }

    pub fn spend_today(&self) -> f64 {
        let since = day_start_ms(now_ms());
        self.store
            .with(|c| {
                c.query_row(
                    "SELECT COALESCE(SUM(usd), 0) FROM tree_cost WHERE created_at >= ?1",
                    params![since],
                    |r| r.get::<_, f64>(0),
                )
            })
            .unwrap_or(0.0)
    }

    /// Spend today split by kind, plus a straight-line projection to 24 h.
    pub fn spend_report(&self) -> Value {
        let now = now_ms();
        let since = day_start_ms(now);
        let rows: Vec<(String, f64, i64)> = self
            .store
            .with(|c| {
                let mut st = c.prepare("SELECT kind, COALESCE(SUM(usd),0), COUNT(*) FROM tree_cost WHERE created_at >= ?1 GROUP BY kind")?;
                let it = st.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                it.collect()
            })
            .unwrap_or_default();
        let total: f64 = rows.iter().map(|r| r.1).sum();
        let elapsed_h = ((now - since) as f64 / 3_600_000.0).max(0.25);
        json!({
            "project_id": self.project_id,
            "today_usd": total,
            "by_kind": rows.iter().map(|(k, u, n)| json!({"kind": k, "usd": u, "calls": n})).collect::<Vec<_>>(),
            "projected_24h_usd": total / elapsed_h * 24.0,
            "cap_usd": Self::daily_cap_usd(),
            "capped": self.capped(),
        })
    }

    /// True once today's spend reached the cap; warns once a day.
    pub fn capped(&self) -> bool {
        let cap = Self::daily_cap_usd();
        if cap <= 0.0 {
            return false;
        }
        let spent = self.spend_today();
        if spent < cap {
            return false;
        }
        let today = day_start_ms(now_ms());
        let mut w = self.cap_warned.lock().expect("cap warned");
        if *w != Some(today) {
            *w = Some(today);
            tracing::warn!(project = %self.project_id, spent_usd = spent, cap_usd = cap, "memory tree: daily spend cap reached; builder and walk paused until tomorrow (UTC)");
        }
        true
    }

    fn add_cost(&self, kind: &str, usage: &Value) {
        let usd = usage
            .get("cost")
            .or_else(|| usage.get("cost_usd"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let tin = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let tout = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let _ = self.store.with(|c| {
            c.execute(
                "INSERT INTO tree_cost(id, kind, usd, tokens_in, tokens_out, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![ulid(), kind, usd, tin, tout, now_ms()],
            )
        });
    }

    fn state_get(&self, key: &str) -> Option<String> {
        self.store
            .with(|c| {
                c.query_row(
                    "SELECT value FROM tree_state WHERE key = ?1",
                    params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    fn state_set(&self, key: &str, value: &str) {
        let _ = self.store.with(|c| {
            c.execute(
                "INSERT INTO tree_state(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
        });
    }

    // ---- nodes --------------------------------------------------------------------------------

    fn node_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
        let parents: String = r.get(3)?;
        let children: String = r.get(4)?;
        let entities: String = r.get(7)?;
        let kind: String = r.get(11)?;
        Ok(Node {
            id: r.get(0)?,
            level: r.get(1)?,
            thread_id: r.get(2)?,
            parents: serde_json::from_str(&parents).unwrap_or_default(),
            children: serde_json::from_str(&children).unwrap_or_default(),
            heading: r.get(5)?,
            summary: r.get(6)?,
            entities: serde_json::from_str(&entities).unwrap_or_default(),
            t0: r.get(8)?,
            t1: r.get(9)?,
            tokens: r.get(10)?,
            kind: Kind::parse(&kind),
            status: r.get(12)?,
            title: r.get(13)?,
            updated_at: r.get(14)?,
        })
    }

    const NODE_COLS: &'static str = "id, level, thread_id, parents, children, heading, summary, entities, t0, t1, tokens, kind, status, title, updated_at";

    fn node(&self, id: &str) -> Option<Node> {
        self.store
            .with(|c| {
                c.query_row(
                    &format!("SELECT {} FROM tree_nodes WHERE id = ?1", Self::NODE_COLS),
                    params![id],
                    Self::node_from_row,
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    fn nodes_where(&self, where_sql: &str, p: &[&dyn rusqlite::ToSql]) -> Vec<Node> {
        self.store
            .with(|c| {
                let mut st = c.prepare(&format!(
                    "SELECT {} FROM tree_nodes WHERE {where_sql}",
                    Self::NODE_COLS
                ))?;
                let it = st.query_map(p, Self::node_from_row)?;
                it.collect()
            })
            .unwrap_or_default()
    }

    fn upsert_node(&self, n: &Node) {
        let _ = self.store.with(|c| {
            c.execute(
                "INSERT INTO tree_nodes(id, level, thread_id, parents, children, heading, summary, entities, t0, t1, tokens, kind, status, title, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                 ON CONFLICT(id) DO UPDATE SET parents=excluded.parents, children=excluded.children, heading=excluded.heading, summary=excluded.summary,
                   entities=excluded.entities, t0=excluded.t0, t1=excluded.t1, tokens=excluded.tokens, kind=excluded.kind, status=excluded.status, title=excluded.title, updated_at=excluded.updated_at",
                params![
                    n.id, n.level, n.thread_id, serde_json::to_string(&n.parents).unwrap_or_default(), serde_json::to_string(&n.children).unwrap_or_default(),
                    n.heading, n.summary, serde_json::to_string(&n.entities).unwrap_or_default(), n.t0, n.t1, n.tokens, n.kind.as_str(), n.status, n.title, n.updated_at
                ],
            )
        });
    }

    fn set_status(&self, id: &str, status: &str) {
        let _ = self.store.with(|c| {
            c.execute(
                "UPDATE tree_nodes SET status = ?2, updated_at = CASE WHEN ?2 = 'card_failed' THEN ?3 ELSE updated_at END WHERE id = ?1",
                params![id, status, now_ms()],
            )
        });
    }

    fn t3_rows(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Vec<T3Row> {
        self.store
            .with(|c| {
                let mut st = c.prepare(&format!(
                    "SELECT id, thread_id, subagent_id, session_id, kind, role, at, text, size FROM t3 {sql}"
                ))?;
                let it = st.query_map(p, |r| {
                    Ok(T3Row {
                        id: r.get(0)?,
                        thread_id: r.get(1)?,
                        subagent_id: r.get(2)?,
                        session_id: r.get(3)?,
                        kind: r.get(4)?,
                        role: r.get(5)?,
                        at: r.get(6)?,
                        text: r.get(7)?,
                        size: r.get(8)?,
                    })
                })?;
                it.collect()
            })
            .unwrap_or_default()
    }

    fn leaves_by_ids(&self, ids: &[String]) -> Vec<T3Row> {
        let mut out = Vec::new();
        for chunk in ids.chunks(200) {
            let marks = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let p: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            out.extend(self.t3_rows(&format!("WHERE id IN ({marks}) ORDER BY at, id"), &p));
        }
        out
    }

    fn page_card(&self, leaf_id: &str) -> Option<(String, Vec<String>)> {
        self.store
            .with(|c| {
                c.query_row(
                    "SELECT p.page, p.facts FROM tree_leaf_pages l JOIN tree_page_cards p ON p.page_key = l.page_key WHERE l.leaf_id = ?1",
                    params![leaf_id],
                    |r| {
                        let facts: String = r.get(1)?;
                        Ok((r.get::<_, String>(0)?, serde_json::from_str(&facts).unwrap_or_default()))
                    },
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    /// One leaf as text: timestamp, role, kind, page card when there is one, the text clipped.
    fn leaf_line(&self, leaf: &T3Row, clip: usize) -> String {
        self.leaf_line_with(leaf, clip, false)
    }

    /// `leaf_line` for the per-turn pack: a leaf with a page card is rendered as the card and its
    /// facts only, plus the `context_open` pointer — the raw output is what made the block large.
    fn leaf_line_packed(&self, leaf: &T3Row, clip: usize) -> String {
        self.leaf_line_with(leaf, clip, true)
    }

    fn leaf_line_with(&self, leaf: &T3Row, clip: usize, card_only: bool) -> String {
        let who = leaf.role.clone().unwrap_or_else(|| leaf.kind.clone());
        let mut s = format!("[{}] {who} ({}): ", fmt_ts(leaf.at), leaf.kind);
        if let Some((page, facts)) = self.page_card(&leaf.id) {
            // a parked (failed) page card is empty: show nothing rather than an empty bracket
            if !page.is_empty() {
                s.push_str(&format!("〔{page}〕 "));
            }
            if !facts.is_empty() {
                s.push_str(&format!("facts: {} — ", facts.join(" | ")));
            }
            if card_only && !page.is_empty() {
                s.push_str(&format!(
                    "[id {} — context_open for the full output]",
                    leaf.id
                ));
                return s;
            }
        }
        let body = clip_chars(&leaf.text, clip);
        s.push_str(&body);
        if leaf.text.chars().count() > clip {
            s.push_str(&format!(
                " [id {} — context_open for the full text]",
                leaf.id
            ));
        }
        s
    }

    /// Compact pack: newest first into per-kind budgets and the cap for the whole block. Before
    /// budgeting, exact repeats (same role and text — the newest stays) and leaves the prompt's
    /// project-memory block already states verbatim are dropped. `blocks` come back in time order.
    fn pack_compact(
        &self,
        kept: &mut [&T3Row],
        leaf_kind: &HashMap<String, Kind>,
        prompt_memory: &str,
        pack: &mut Pack,
        blocks: &mut Vec<(i64, String, usize, String)>,
        total: &mut usize,
    ) {
        let memory_norm = norm_text(prompt_memory);
        // memory items read `- text` (T0) or `- [kind] text` (T1): compare the text only
        let memory_lines: Vec<String> = prompt_memory
            .lines()
            .map(|l| {
                let l = l.trim().trim_start_matches("- ");
                let l = match (l.starts_with('['), l.find("] ")) {
                    (true, Some(i)) => &l[i + 2..],
                    _ => l,
                };
                norm_text(l)
            })
            .filter(|l| l.chars().count() >= DEDUPE_MIN_CHARS)
            .collect();
        kept.sort_by(|a, b| (b.at, &b.id).cmp(&(a.at, &a.id)));
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut used: HashMap<Kind, usize> = HashMap::new();
        for l in kept.iter() {
            let kind = leaf_kind.get(&l.id).copied().unwrap_or(Kind::Chat);
            let body = norm_text(&l.text);
            let key = (
                l.role.clone().unwrap_or_default(),
                body.chars().take(400).collect::<String>(),
            );
            let long_enough = body.chars().count() >= DEDUPE_MIN_CHARS;
            let dropped = if !seen.insert(key) {
                Some("repeat")
            } else if long_enough
                && (memory_norm.contains(body.as_str())
                    || memory_lines.iter().any(|m| body.contains(m.as_str())))
            {
                Some("in prompt memory")
            } else {
                None
            };
            if let Some(why) = dropped {
                pack.decisions
                    .push(json!({ "level": "pack", "node_id": l.id, "dropped": why }));
                continue;
            }
            let line = self.leaf_line_packed(l, kind.clip());
            let tok = est_tokens(&line);
            let kind_used = used.get(&kind).copied().unwrap_or(0);
            if kind_used + tok > kind.budget() || *total + tok > BUDGET_PACK {
                pack.decisions.push(json!({ "level": "pack", "node_id": l.id, "dropped": "budget", "tokens": tok, "kind": kind.as_str() }));
                continue;
            }
            used.insert(kind, kind_used + tok);
            *total += tok;
            blocks.push((l.at, line, tok, l.id.clone()));
        }
        blocks.sort_by(|a, b| (a.0, &a.3).cmp(&(b.0, &b.3)));
    }

    // ---- thread metadata ----------------------------------------------------------------------

    fn thread_meta(&self, thread_id: &str) -> (String, Kind, Vec<String>) {
        // chat?
        if let Ok(Some(c)) = self.store.chat(thread_id) {
            return (c.title, Kind::Chat, vec![ROOT_ID.to_owned()]);
        }
        if let Ok(Some(s)) = self.store.subagent(thread_id) {
            let mut parents = vec![ROOT_ID.to_owned()];
            if let Some(cid) = &s.chat_id {
                parents.push(format!("thread:{cid}"));
            }
            return (
                format!("subagent {}: {}", s.name, s.purpose),
                Kind::Tool,
                parents,
            );
        }
        (
            format!("chat {thread_id}"),
            Kind::Chat,
            vec![ROOT_ID.to_owned()],
        )
    }

    fn ensure_root(&self) {
        if self.node(ROOT_ID).is_none() {
            self.upsert_node(&Node {
                id: ROOT_ID.into(),
                level: "project".into(),
                thread_id: None,
                parents: vec![],
                children: vec![],
                heading: format!("Project {}", self.project_name),
                summary: String::new(),
                entities: vec![],
                t0: 0,
                t1: 0,
                tokens: 0,
                kind: Kind::Chat,
                status: "stale".into(),
                title: self.project_name.clone(),
                updated_at: 0,
            });
        }
    }

    fn ensure_thread(&self, thread_id: &str, at: i64) -> Node {
        let id = format!("thread:{thread_id}");
        if let Some(n) = self.node(&id) {
            return n;
        }
        let (title, kind, parents) = self.thread_meta(thread_id);
        let n = Node {
            id: id.clone(),
            level: "thread".into(),
            thread_id: Some(thread_id.to_owned()),
            parents: parents.clone(),
            children: vec![],
            heading: String::new(),
            summary: String::new(),
            entities: vec![],
            t0: at,
            t1: at,
            tokens: 0,
            kind,
            status: "stale".into(),
            title,
            updated_at: 0,
        };
        self.upsert_node(&n);
        for p in &parents {
            if let Some(mut pn) = self.node(p) {
                if !pn.children.contains(&id) {
                    pn.children.push(id.clone());
                    pn.status = "stale".into();
                    self.upsert_node(&pn);
                }
            }
        }
        n
    }

    // ---- builder ------------------------------------------------------------------------------

    /// One incremental pass: new leaves → episodes; page cards; cards for settled episodes, stale
    /// threads and the root. Bounded work per pass; safe to call often.
    pub async fn build_pass(&self) -> Result<Value> {
        let _g = self.build_lock.lock().await;
        self.ensure_root();
        let t0 = Instant::now();
        let mut stats = json!({ "leaves": 0, "episodes_new": 0, "cards": 0, "page_cards": 0, "skipped_cap": false });
        // 1. leaves → episodes (no model calls)
        let through = self.state_get("built_through").unwrap_or_default();
        let rows = self.t3_rows(
            "WHERE id > ?1 AND kind <> 'route' ORDER BY id LIMIT 2000",
            &[&through],
        );
        let mut last_id = through.clone();
        let mut new_eps = 0;
        for leaf in &rows {
            let thread = self.ensure_thread(&leaf.thread_id, leaf.at);
            let kind = thread.kind;
            let tokens = est_tokens(&leaf.text) as i64;
            let open = self
                .nodes_where(
                    "level = 'episode' AND thread_id = ?1 ORDER BY t1 DESC LIMIT 1",
                    &[&leaf.thread_id],
                )
                .into_iter()
                .next();
            let starts_new = match &open {
                None => true,
                Some(ep) => {
                    let gap = leaf.at - ep.t1;
                    let new_turn = leaf.kind == "user"
                        && ep.tokens as usize > kind.episode_max_tokens() / 2
                        && ep.children.len() >= 6;
                    gap > EPISODE_GAP_MS
                        || (ep.tokens + tokens) as usize > kind.episode_max_tokens()
                        || ep.children.len() >= kind.episode_max_leaves()
                        || new_turn
                        || ep.status == "ready"
                            && (ep.children.len() >= kind.episode_max_leaves() / 2)
                }
            };
            if starts_new {
                if let Some(ep) = &open {
                    if ep.status == "open" {
                        self.set_status(&ep.id, "closed");
                    }
                }
                let ep = Node {
                    id: format!("ep:{}", ulid()),
                    level: "episode".into(),
                    thread_id: Some(leaf.thread_id.clone()),
                    parents: vec![thread.id.clone()],
                    children: vec![leaf.id.clone()],
                    heading: String::new(),
                    summary: String::new(),
                    entities: vec![],
                    t0: leaf.at,
                    t1: leaf.at,
                    tokens,
                    kind,
                    status: "open".into(),
                    title: String::new(),
                    updated_at: now_ms(),
                };
                self.upsert_node(&ep);
                let mut th = thread.clone();
                th.children.push(ep.id.clone());
                th.t1 = leaf.at;
                th.tokens += tokens;
                th.status = "stale".into();
                self.upsert_node(&th);
                new_eps += 1;
            } else if let Some(mut ep) = open {
                ep.children.push(leaf.id.clone());
                ep.t1 = leaf.at;
                ep.tokens += tokens;
                // a card written for an episode that then grew is stale again
                ep.status = "open".into();
                self.upsert_node(&ep);
                let mut th = thread.clone();
                th.t1 = leaf.at;
                th.tokens += tokens;
                th.status = "stale".into();
                self.upsert_node(&th);
            }
            last_id = leaf.id.clone();
        }
        if last_id != through {
            self.state_set("built_through", &last_id);
        }
        stats["leaves"] = json!(rows.len());
        stats["episodes_new"] = json!(new_eps);
        if self.capped() {
            stats["skipped_cap"] = json!(true);
            stats["ms"] = json!(t0.elapsed().as_millis() as i64);
            return Ok(stats);
        }
        let model = self.card_model();
        let mut calls = 0usize;
        let mut errors = 0usize;
        // 2. page cards for big tool results (deduped by content)
        let pages_through = self.state_get("pages_through").unwrap_or_default();
        let big = self.t3_rows(
            "WHERE id > ?1 AND kind IN ('tool_output','session_entry') AND size > ?2 ORDER BY id LIMIT 60",
            &[&pages_through, &(PAGE_CARD_MIN_CHARS as i64)],
        );
        let mut pages_last = pages_through.clone();
        for leaf in &big {
            if calls >= MAX_CARDS_PER_PASS {
                break;
            }
            let head: String = leaf.text.chars().take(PAGE_CARD_CLIP).collect();
            let key = sha16(&head);
            let exists: bool = self
                .store
                .with(|c| {
                    c.query_row(
                        "SELECT 1 FROM tree_page_cards WHERE page_key = ?1",
                        params![key],
                        |_| Ok(()),
                    )
                    .optional()
                    .map(|o| o.is_some())
                })
                .unwrap_or(false);
            if !exists {
                let call = self
                    .t3_rows(
                        "WHERE thread_id = ?1 AND kind = 'tool_call' AND id < ?2 ORDER BY id DESC LIMIT 1",
                        &[&leaf.thread_id, &leaf.id],
                    )
                    .into_iter()
                    .next()
                    .map(|c| clip_chars(&c.text, 400))
                    .unwrap_or_default();
                match self
                    .llm
                    .complete_json_with_usage(
                        &model,
                        PAGE_CARD_SYSTEM,
                        &format!("Tool call: {call}\n\nOutput:\n{head}"),
                        600,
                    )
                    .await
                {
                    Ok((v, _, usage)) => {
                        self.add_cost("page_cards", &usage);
                        calls += 1;
                        let page = v
                            .get("page")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(200)
                            .collect::<String>();
                        let facts: Vec<String> = v
                            .get("facts")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str())
                                    .map(|s| s.chars().take(120).collect())
                                    .take(14)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let _ = self.store.with(|c| {
                            c.execute(
                                "INSERT OR IGNORE INTO tree_page_cards(page_key, page, facts, created_at) VALUES (?1,?2,?3,?4)",
                                params![key, page, serde_json::to_string(&facts).unwrap_or_default(), now_ms()],
                            )
                        });
                        stats["page_cards"] = json!(stats["page_cards"].as_i64().unwrap_or(0) + 1);
                    }
                    Err(e) => {
                        // Park the page (empty card) so the same leaf is not re-sent on every
                        // pass; a later identical output reuses the empty card and costs nothing.
                        tracing::warn!(error = %e, leaf = %leaf.id, "page card failed; parked");
                        let _ = self.store.with(|c| {
                            c.execute(
                                "INSERT OR IGNORE INTO tree_page_cards(page_key, page, facts, created_at) VALUES (?1, '', '[]', ?2)",
                                params![key, now_ms()],
                            )
                        });
                        errors += 1;
                        calls += 1;
                        if errors >= 3 {
                            break;
                        }
                    }
                }
            }
            let _ = self.store.with(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO tree_leaf_pages(leaf_id, page_key) VALUES (?1, ?2)",
                    params![leaf.id, key],
                )
            });
            pages_last = leaf.id.clone();
        }
        if pages_last != pages_through {
            self.state_set("pages_through", &pages_last);
        }
        // 3. episode cards: closed episodes, or open ones that settled
        let settle_before = now_ms() - SETTLE_MS;
        let retry_before = now_ms() - CARD_FAIL_BACKOFF_MS;
        let todo = self.nodes_where(
            "level = 'episode' AND (status = 'closed' OR (status = 'open' AND t1 < ?1) OR (status = 'card_failed' AND updated_at < ?2)) ORDER BY t1 DESC LIMIT 60",
            &[&settle_before, &retry_before],
        );
        for mut ep in todo {
            if calls >= MAX_CARDS_PER_PASS {
                break;
            }
            let leaves = self.leaves_by_ids(&ep.children);
            let body: Vec<String> = leaves
                .iter()
                .map(|l| {
                    self.leaf_line(
                        l,
                        if l.kind == "user" {
                            4000
                        } else {
                            TOOL_CLIP_FOR_CARDS
                        },
                    )
                })
                .collect();
            let thread = ep
                .thread_id
                .as_ref()
                .and_then(|t| self.node(&format!("thread:{t}")));
            let hint = thread
                .map(|t| format!("thread '{}' ({})", t.title, t.kind.as_str()))
                .unwrap_or_default();
            match self
                .write_card(&model, "episode", &hint, &body.join("\n"))
                .await
            {
                Ok(card) => {
                    calls += 1;
                    ep.heading = card.0;
                    ep.summary = card.1;
                    ep.entities = card.2;
                    ep.status = "ready".into();
                    ep.updated_at = now_ms();
                    self.upsert_node(&ep);
                    if let Some(t) = &ep.thread_id {
                        self.set_status(&format!("thread:{t}"), "stale");
                    }
                    stats["cards"] = json!(stats["cards"].as_i64().unwrap_or(0) + 1);
                }
                Err(e) => {
                    tracing::warn!(error = %e, node = %ep.id, "episode card failed; retry in an hour");
                    errors += 1;
                    calls += 1;
                    self.set_status(&ep.id, "card_failed");
                    if errors >= 3 {
                        break;
                    }
                }
            }
        }
        // 4. thread cards (debounced) from episode cards
        let debounce = now_ms() - THREAD_CARD_DEBOUNCE_MS;
        let threads = self.nodes_where(
            "level = 'thread' AND ((status = 'stale' AND (updated_at < ?1 OR heading = '')) OR (status = 'card_failed' AND updated_at < ?2)) ORDER BY t1 DESC LIMIT 20",
            &[&debounce, &retry_before],
        );
        for mut th in threads {
            if calls >= MAX_CARDS_PER_PASS {
                break;
            }
            let kids: Vec<Node> = th
                .children
                .iter()
                .filter_map(|c| self.node(c))
                .filter(|n| !n.heading.is_empty())
                .collect();
            if kids.is_empty() {
                continue;
            }
            let body: Vec<String> = kids
                .iter()
                .map(|k| {
                    format!(
                        "- [{} – {}] {} — {}",
                        fmt_ts(k.t0),
                        fmt_ts(k.t1),
                        k.heading,
                        k.summary
                    )
                })
                .collect();
            let hint = format!("{} '{}'", th.kind.as_str(), th.title);
            match self
                .write_card(&model, "thread", &hint, &body.join("\n"))
                .await
            {
                Ok(card) => {
                    calls += 1;
                    th.heading = card.0;
                    th.summary = card.1;
                    th.entities = card.2;
                    th.t0 = kids.iter().map(|k| k.t0).min().unwrap_or(th.t0);
                    th.status = "ready".into();
                    th.updated_at = now_ms();
                    self.upsert_node(&th);
                    self.set_status(ROOT_ID, "stale");
                    stats["cards"] = json!(stats["cards"].as_i64().unwrap_or(0) + 1);
                }
                Err(e) => {
                    tracing::warn!(error = %e, node = %th.id, "thread card failed; retry in an hour");
                    errors += 1;
                    calls += 1;
                    // `stale` is restored by the next episode card, so new content still wins.
                    self.set_status(&th.id, "card_failed");
                    if errors >= 3 {
                        break;
                    }
                }
            }
        }
        // 5. the root card (debounced harder)
        if calls < MAX_CARDS_PER_PASS {
            if let Some(mut root) = self.node(ROOT_ID) {
                let due = (root.status == "stale"
                    && root.updated_at < now_ms() - ROOT_CARD_DEBOUNCE_MS)
                    || (root.status == "card_failed" && root.updated_at < retry_before);
                if due {
                    let kids: Vec<Node> = root
                        .children
                        .iter()
                        .filter_map(|c| self.node(c))
                        .filter(|n| !n.heading.is_empty())
                        .collect();
                    if !kids.is_empty() {
                        let body: Vec<String> = kids
                            .iter()
                            .map(|k| {
                                format!(
                                    "- [{} – {}] {} — {}",
                                    fmt_ts(k.t0),
                                    fmt_ts(k.t1),
                                    k.heading,
                                    k.summary
                                )
                            })
                            .collect();
                        match self
                            .write_card(
                                &model,
                                "project",
                                &self.project_name.clone(),
                                &body.join("\n"),
                            )
                            .await
                        {
                            Ok(card) => {
                                root.heading = card.0;
                                root.summary = card.1;
                                root.entities = card.2;
                                root.t0 = kids.iter().map(|k| k.t0).min().unwrap_or(0);
                                root.t1 = kids.iter().map(|k| k.t1).max().unwrap_or(0);
                                root.status = "ready".into();
                                root.updated_at = now_ms();
                                self.upsert_node(&root);
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "project card failed; retry in an hour");
                                // A thread card sets the root back to `stale`, so new content
                                // still refreshes it before the backoff ends.
                                self.set_status(ROOT_ID, "card_failed");
                            }
                        }
                    }
                }
            }
        }
        stats["ms"] = json!(t0.elapsed().as_millis() as i64);
        Ok(stats)
    }

    fn card_model(&self) -> String {
        self.card_model.clone()
    }

    async fn write_card(
        &self,
        model: &str,
        kind: &str,
        hint: &str,
        body: &str,
    ) -> Result<(String, String, Vec<String>)> {
        let body: String = body.chars().take(24_000).collect();
        let (text, _, usage) = self
            .llm
            .complete_with_usage(
                model,
                &format!("{CARD_SYSTEM}\nAnswer with one JSON object and nothing else."),
                &format!("Node kind: {kind}. Context: {hint}\n\nContent:\n{body}"),
                1200,
            )
            .await?;
        self.add_cost("cards", &usage);
        parse_card(&text).ok_or_else(|| anyhow!("card without heading: {}", clip_chars(&text, 120)))
    }

    /// Node counts by level and status (for the admin/debug view and the roll-out check).
    pub fn stats(&self) -> Value {
        let rows: Vec<(String, String, i64)> = self
            .store
            .with(|c| {
                let mut st = c.prepare(
                    "SELECT level, status, COUNT(*) FROM tree_nodes GROUP BY level, status",
                )?;
                let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                it.collect()
            })
            .unwrap_or_default();
        let pages: i64 = self
            .store
            .with(|c| c.query_row("SELECT COUNT(*) FROM tree_page_cards", [], |r| r.get(0)))
            .unwrap_or(0);
        json!({
            "nodes": rows.iter().map(|(l, s, n)| json!({"level": l, "status": s, "n": n})).collect::<Vec<_>>(),
            "page_cards": pages,
            "built_through": self.state_get("built_through"),
            "spend": self.spend_report(),
        })
    }

    // ---- walk ---------------------------------------------------------------------------------

    /// Threads visible from `chat_id`: a people/group chat sees only itself and the subagents it
    /// spawned; a project chat sees the whole Project.
    fn allowed_threads(&self, chat_id: &str, restricted: bool) -> Option<HashSet<String>> {
        if !restricted {
            return None;
        }
        let mut set = HashSet::new();
        set.insert(chat_id.to_owned());
        if let Ok(subs) = self.store.subagents() {
            for s in subs {
                if s.chat_id.as_deref() == Some(chat_id) {
                    set.insert(s.id);
                }
            }
        }
        Some(set)
    }

    fn card_json(&self, n: &Node) -> Value {
        let mut c = json!({
            "kind": if n.level == "thread" { format!("{} thread", n.kind.as_str()) } else { n.level.clone() },
            "heading": if n.heading.is_empty() { n.title.clone() } else { n.heading.clone() },
            "summary": n.summary,
            "when": format!("{} – {}", fmt_ts(n.t0), fmt_ts(n.t1)),
        });
        if !n.entities.is_empty() {
            c["entities"] = json!(n.entities);
        }
        if n.level == "thread" && !n.title.is_empty() {
            c["title"] = json!(n.title);
        }
        c
    }

    fn leaf_card_json(&self, leaf: &T3Row) -> Value {
        json!({
            "kind": format!("raw {} event", leaf.kind),
            "when": fmt_ts(leaf.at),
            "text": self.leaf_line(leaf, LEAF_CLIP_FOR_JEV),
        })
    }

    /// Score sibling candidates with the decision model; one request per chunk, chunks in parallel.
    async fn score(
        &self,
        scorer: &str,
        query: &Value,
        cards: &[(String, Value)],
        tally: &mut (i64, f64, i64),
    ) -> HashMap<String, f64> {
        let futs = cards.chunks(JEV_CHUNK).map(|chunk| {
            let cands: serde_json::Map<String, Value> = chunk
                .iter()
                .enumerate()
                .map(|(i, (_, card))| (format!("c{i}"), card.clone()))
                .collect();
            let questions: serde_json::Map<String, Value> = (0..chunk.len())
                .map(|i| {
                    let cid = format!("c{i}");
                    (cid.clone(), json!({
                        "type": "noul",
                        "instructions": format!("Could `candidates.{cid}` contain information needed to answer `query`? Judge this candidate on its own, from its heading, summary, time and entities. When in doubt, say yes: missing a needed record is worse than reading an extra one."),
                        "criteria": {
                            "true": "The candidate covers the same subject, person, file, project, decision or event as the query, or an episode where the answer was likely said; or the query asks about a time span the candidate falls in.",
                            "false": "The candidate is about an unrelated subject and nothing in it would help answer the query."
                        }
                    }))
                })
                .collect();
            let state = json!({ "query": query, "candidates": Value::Object(cands) });
            async move {
                let r = self
                    .llm
                    .decide(
                        scorer,
                        &state,
                        &Value::Object(questions),
                        Duration::from_millis(JEV_TIMEOUT_MS),
                    )
                    .await;
                (chunk, r)
            }
        });
        let mut out = HashMap::new();
        for (chunk, r) in join_all(futs).await {
            match r {
                Ok(d) => {
                    tally.0 += 1;
                    tally.1 += d.cost_usd.unwrap_or(0.0);
                    tally.2 = tally.2.max(d.latency_ms);
                    self.add_cost(
                        "walk",
                        &json!({"cost": d.cost_usd.unwrap_or(0.0), "input_tokens": d.input_tokens, "output_tokens": d.output_tokens}),
                    );
                    for (i, (id, _)) in chunk.iter().enumerate() {
                        let p = d
                            .answers
                            .get(&format!("c{i}"))
                            .map(|a| a.probability)
                            .unwrap_or(0.0);
                        out.insert(id.clone(), p);
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "tree walk: decision call failed");
                }
            }
        }
        out
    }

    /// The walk proper. `exclude_thread` is the chat the question is asked in (its recent window
    /// is already in the model's session); `restricted` limits the walk to that chat's subtree.
    #[allow(clippy::too_many_arguments)]
    async fn walk_inner(
        &self,
        scorer: &str,
        chat_id: &str,
        restricted: bool,
        query_text: &str,
        asker: &str,
        prompt_memory: &str,
        compact: bool,
    ) -> Pack {
        let t0 = Instant::now();
        let mut pack = Pack::default();
        let allowed = self.allowed_threads(chat_id, restricted);
        let query = json!({ "text": query_text, "asked_at": fmt_ts(now_ms()), "asked_in": if restricted { "a group or people chat" } else { "a chat with the project owner" }, "asked_by": asker });
        let mut tally = (0i64, 0f64, 0i64);
        // level 1: threads
        let mut threads: Vec<Node> = self
            .nodes_where("level = 'thread' ORDER BY t1 DESC LIMIT 400", &[])
            .into_iter()
            .filter(|n| {
                n.thread_id.as_deref() != Some(chat_id)
                    && allowed
                        .as_ref()
                        .map(|a| n.thread_id.as_ref().is_some_and(|t| a.contains(t)))
                        .unwrap_or(true)
            })
            .collect();
        if restricted {
            // the group's own thread is the subtree root; keep its episodes (not the recent ones)
            if let Some(n) = self.node(&format!("thread:{chat_id}")) {
                threads.push(n);
            }
        }
        if threads.is_empty() {
            pack.outcome = "empty".into();
            pack.reason = Some("no threads".into());
            pack.walk_ms = t0.elapsed().as_millis() as i64;
            return pack;
        }
        let cards: Vec<(String, Value)> = threads
            .iter()
            .map(|n| (n.id.clone(), self.card_json(n)))
            .collect();
        let probs = self.score(scorer, &query, &cards, &mut tally).await;
        let mut keep_threads: Vec<&Node> = Vec::new();
        {
            let mut ranked: Vec<&Node> = threads.iter().collect();
            ranked.sort_by(|a, b| {
                probs
                    .get(&b.id)
                    .unwrap_or(&0.0)
                    .partial_cmp(probs.get(&a.id).unwrap_or(&0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut per_kind: HashMap<&str, usize> = HashMap::new();
            for n in ranked {
                let p = *probs.get(&n.id).unwrap_or(&0.0);
                let c = per_kind.entry(n.kind.as_str()).or_insert(0);
                let keep = (p >= n.kind.threshold("thread") && *c < n.kind.beam("thread"))
                    || (restricted && n.thread_id.as_deref() == Some(chat_id))
                    || keep_threads.len() < 2;
                pack.decisions.push(json!({ "level": "thread", "node_id": n.id, "p": (p * 1000.0).round() / 1000.0, "thr": n.kind.threshold("thread"), "expanded": keep, "heading": clip_chars(&n.heading, 120) }));
                if keep {
                    *c += 1;
                    keep_threads.push(n);
                }
            }
        }
        // level 2: episodes (ready ones scored; open/closed ones scanned at leaf level directly)
        let mut ep_cards: Vec<(String, Value)> = Vec::new();
        let mut ep_nodes: HashMap<String, Node> = HashMap::new();
        let mut direct_leaves: Vec<String> = Vec::new();
        for t in &keep_threads {
            for c in &t.children {
                if let Some(ep) = self.node(c) {
                    if ep.level != "episode" {
                        continue;
                    }
                    if ep.status == "ready" && !ep.heading.is_empty() {
                        ep_cards.push((ep.id.clone(), self.card_json(&ep)));
                        ep_nodes.insert(ep.id.clone(), ep);
                    } else {
                        direct_leaves.extend(ep.children.iter().cloned());
                    }
                }
            }
        }
        let ep_probs = if ep_cards.is_empty() {
            HashMap::new()
        } else {
            self.score(scorer, &query, &ep_cards, &mut tally).await
        };
        let mut keep_eps: Vec<&Node> = Vec::new();
        {
            let mut ranked: Vec<&Node> = ep_nodes.values().collect();
            ranked.sort_by(|a, b| {
                ep_probs
                    .get(&b.id)
                    .unwrap_or(&0.0)
                    .partial_cmp(ep_probs.get(&a.id).unwrap_or(&0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut per_kind: HashMap<&str, usize> = HashMap::new();
            for n in ranked {
                let p = *ep_probs.get(&n.id).unwrap_or(&0.0);
                let c = per_kind.entry(n.kind.as_str()).or_insert(0);
                let keep = (p >= n.kind.threshold("episode") && *c < n.kind.beam("episode"))
                    || keep_eps.len() < 2;
                pack.decisions.push(json!({ "level": "episode", "node_id": n.id, "p": (p * 1000.0).round() / 1000.0, "thr": n.kind.threshold("episode"), "expanded": keep, "heading": clip_chars(&n.heading, 120) }));
                if keep {
                    *c += 1;
                    keep_eps.push(n);
                }
            }
        }
        // level 3: leaves of kept episodes + unsummarized leaves + keyword hits
        let mut leaf_ids: Vec<String> = Vec::new();
        let mut leaf_kind: HashMap<String, Kind> = HashMap::new();
        for ep in &keep_eps {
            for l in &ep.children {
                leaf_ids.push(l.clone());
                leaf_kind.insert(l.clone(), ep.kind);
            }
        }
        for l in &direct_leaves {
            leaf_ids.push(l.clone());
        }
        let mut extra: HashSet<String> = HashSet::new();
        if let Ok(hits) = self
            .store
            .t3_search_any(&content_words(query_text), BM25_EXTRA)
        {
            for (r, _, _) in hits {
                if r.kind == "route" || r.thread_id == chat_id {
                    continue;
                }
                if let Some(a) = &allowed {
                    if !a.contains(&r.thread_id) {
                        continue;
                    }
                }
                if !leaf_ids.contains(&r.id) {
                    leaf_ids.push(r.id.clone());
                    extra.insert(r.id.clone());
                }
            }
        }
        let mut seen = HashSet::new();
        leaf_ids.retain(|l| seen.insert(l.clone()));
        let leaves = self.leaves_by_ids(&leaf_ids);
        if leaves.is_empty() {
            pack.outcome = "empty".into();
            pack.reason = Some("no candidate leaves".into());
            pack.jev_calls = tally.0;
            pack.jev_cost = tally.1;
            pack.jev_ms = tally.2;
            pack.walk_ms = t0.elapsed().as_millis() as i64;
            return pack;
        }
        let leaf_cards: Vec<(String, Value)> = leaves
            .iter()
            .map(|l| (l.id.clone(), self.leaf_card_json(l)))
            .collect();
        let lp = self.score(scorer, &query, &leaf_cards, &mut tally).await;
        let mut kept: Vec<&T3Row> = Vec::new();
        for l in &leaves {
            let kind = leaf_kind
                .get(&l.id)
                .copied()
                .unwrap_or_else(|| self.thread_meta(&l.thread_id).1);
            let p = *lp.get(&l.id).unwrap_or(&0.0);
            let keep = p >= kind.threshold("leaf");
            pack.decisions.push(json!({ "level": "leaf", "node_id": l.id, "p": (p * 1000.0).round() / 1000.0, "thr": kind.threshold("leaf"), "expanded": keep, "source": if extra.contains(&l.id) { "bm25" } else { "tree" }, "heading": clip_chars(&l.text, 100) }));
            if keep {
                kept.push(l);
            }
        }
        let mut blocks: Vec<(i64, String, usize, String)> = Vec::new(); // (at, text, tokens, leaf id)
        let mut total: usize = 0;
        if compact {
            self.pack_compact(
                &mut kept,
                &leaf_kind,
                prompt_memory,
                &mut pack,
                &mut blocks,
                &mut total,
            );
        } else {
            // full: time order, one budget by content kind, oldest dropped when over budget
            let any_tool = kept
                .iter()
                .any(|l| leaf_kind.get(&l.id).copied().unwrap_or(Kind::Chat) == Kind::Tool);
            let budget = if any_tool {
                BUDGET_TOOL_FULL
            } else {
                BUDGET_CHAT_FULL
            };
            kept.sort_by_key(|l| (l.at, l.id.clone()));
            for l in &kept {
                let kind = leaf_kind.get(&l.id).copied().unwrap_or(Kind::Chat);
                let line = self.leaf_line(l, kind.clip_full());
                let tok = est_tokens(&line);
                blocks.push((l.at, line, tok, l.id.clone()));
            }
            total = blocks.iter().map(|b| b.2).sum();
            let mut i = 0;
            while total > budget && i < blocks.len() {
                total -= blocks[i].2;
                blocks[i].1.clear();
                i += 1;
            }
        }
        let mut out = String::new();
        let mut cur_thread = String::new();
        let mut thread_headings: HashMap<String, String> = HashMap::new();
        for t in &keep_threads {
            if let Some(tid) = &t.thread_id {
                thread_headings.insert(
                    tid.clone(),
                    if t.heading.is_empty() {
                        t.title.clone()
                    } else {
                        format!("{} — {}", t.title, t.heading)
                    },
                );
            }
        }
        let leaf_thread: HashMap<&str, &str> = kept
            .iter()
            .map(|l| (l.id.as_str(), l.thread_id.as_str()))
            .collect();
        for (_, line, _, id) in &blocks {
            if line.is_empty() {
                continue;
            }
            let tid = leaf_thread.get(id.as_str()).copied().unwrap_or("");
            if tid != cur_thread {
                cur_thread = tid.to_owned();
                let (title, _, _) = self.thread_meta(tid);
                let h = thread_headings.get(tid).cloned().unwrap_or(title);
                out.push_str(&format!("\n## {h} [thread {tid}]\n"));
            }
            out.push_str(line);
            out.push('\n');
            pack.leaf_ids.push(id.clone());
        }
        pack.node_ids = keep_threads
            .iter()
            .map(|n| n.id.clone())
            .chain(keep_eps.iter().map(|n| n.id.clone()))
            .collect();
        pack.tokens = total;
        pack.text = out.trim().to_owned();
        pack.jev_calls = tally.0;
        pack.jev_cost = tally.1;
        pack.jev_ms = tally.2;
        pack.walk_ms = t0.elapsed().as_millis() as i64;
        pack.outcome = if pack.text.is_empty() {
            "empty".into()
        } else {
            "ok".into()
        };
        pack
    }

    /// Run the walk for one turn with a deadline, log it, and return the text to inject (None =
    /// current behaviour). `mode` decides whether an ok pack is injected or only logged.
    #[allow(clippy::too_many_arguments)]
    pub async fn pack_for_turn(
        &self,
        mode: TreeMode,
        scorer: &str,
        chat_id: &str,
        message_id: Option<&str>,
        purpose: &str,
        restricted: bool,
        query_text: &str,
        asker: &str,
        recent_window: &Value,
        prompt_memory: &str,
        compact: bool,
    ) -> Option<String> {
        if mode == TreeMode::Off {
            return None;
        }
        let built = self.state_get("built_through").is_some();
        let mut pack = if !built {
            Pack {
                outcome: "fallback".into(),
                reason: Some("tree not built yet".into()),
                ..Default::default()
            }
        } else if self.capped() {
            Pack {
                outcome: "fallback".into(),
                reason: Some("daily cap".into()),
                ..Default::default()
            }
        } else {
            match tokio::time::timeout(
                Duration::from_millis(WALK_DEADLINE_MS + 600),
                self.walk_inner(
                    scorer,
                    chat_id,
                    restricted,
                    query_text,
                    asker,
                    prompt_memory,
                    compact,
                ),
            )
            .await
            {
                Ok(p) => p,
                Err(_) => Pack {
                    outcome: "fallback".into(),
                    reason: Some(format!("walk timed out after {}ms", WALK_DEADLINE_MS + 600)),
                    ..Default::default()
                },
            }
        };
        let inject = mode == TreeMode::On && pack.outcome == "ok";
        if pack.outcome == "ok" && !inject {
            pack.outcome = "logged".into();
        } else if inject {
            pack.outcome = "injected".into();
        }
        let _ = self.store.with(|c| {
            c.execute(
                "INSERT INTO tree_walk_log(id, chat_id, message_id, purpose, mode, outcome, reason, query, pack, decisions, jev_calls, jev_cost, jev_ms, walk_ms, pack_tokens, recent_window, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    ulid(), chat_id, message_id, purpose, mode.as_str(), pack.outcome, pack.reason, clip_chars(query_text, 600),
                    json!({ "node_ids": pack.node_ids, "leaf_ids": pack.leaf_ids, "text": pack.text }).to_string(),
                    Value::Array(pack.decisions.clone()).to_string(), pack.jev_calls, pack.jev_cost, pack.jev_ms, pack.walk_ms, pack.tokens as i64,
                    recent_window.to_string(), now_ms()
                ],
            )
        });
        let _ = self.store.log_decision_with_usage(
            "read",
            message_id,
            Some(&json!({ "purpose": purpose, "outcome": pack.outcome, "reason": pack.reason, "nodes": pack.node_ids.len(), "leaves": pack.leaf_ids.len(), "tokens": pack.tokens })),
            Some(if inject { "INJECT" } else { "FALLBACK" }),
            &pack.leaf_ids,
            pack.reason.as_deref(),
            None,
            Some(scorer),
            Some(pack.walk_ms),
            Some(&json!({ "provider": "jev", "calls": pack.jev_calls, "cost_usd": pack.jev_cost, "max_latency_ms": pack.jev_ms })),
        );
        tracing::info!(chat = chat_id, purpose, outcome = %pack.outcome, leaves = pack.leaf_ids.len(), tokens = pack.tokens, jev_calls = pack.jev_calls, cost_usd = pack.jev_cost, walk_ms = pack.walk_ms, "memory tree walk");
        if !inject {
            return None;
        }
        let rules = if compact {
            "[Memory retrieved for this message from earlier chats, tool results and subagent sessions; raw records, oldest first. What the user told you about themselves (name, preferences, facts) is true and outranks anything you said before or any handle you see; between two user statements the newer wins; if the records show the question's premise is wrong, say so. Answer from the records and say briefly what you checked; if nothing here answers it, say you have no record rather than guessing. More via context_search / context_open / context_around.]"
        } else {
            "[Memory retrieved from your earlier chats, tool results and subagent sessions for this message. Raw records, oldest first. Rules: what the user told you about themselves (their name, preferences, facts) is true and outranks anything you said before and any handle or label you see; between two user statements the newer wins; if the records show the question's premise is wrong, say so. Answer from these records directly and say in a few words what you checked. If nothing here answers the question, say you have no record of it rather than guessing. You can read more with context_search / context_open / context_around.]"
        };
        Some(format!("{rules}\n{}\n[End of retrieved memory]", pack.text))
    }

    /// Offline check (`arbos-agentd eval-tree`): build until nothing is left to card, then walk
    /// one query from `chat_id` and print the pack.
    pub async fn eval(
        &self,
        scorer: &str,
        chat_id: &str,
        query: &str,
        compact: bool,
    ) -> Result<Value> {
        let mut passes = Vec::new();
        for _ in 0..30 {
            let st = self.build_pass().await?;
            let did = st.get("cards").and_then(Value::as_i64).unwrap_or(0)
                + st.get("page_cards").and_then(Value::as_i64).unwrap_or(0)
                + st.get("leaves").and_then(Value::as_i64).unwrap_or(0);
            passes.push(st.clone());
            if did == 0
                || st
                    .get("skipped_cap")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                break;
            }
        }
        // the same T0/T1 text the prompt's project-memory block holds (threads omitted)
        let mut prompt_memory = String::new();
        for tier in ["T0", "T1"] {
            for m in self.store.memory_live(tier, None, 80).unwrap_or_default() {
                prompt_memory.push_str(&format!("- [{}] {}\n", m.kind, m.text));
            }
        }
        let pack = self
            .walk_inner(
                scorer,
                chat_id,
                false,
                query,
                "the project owner",
                &prompt_memory,
                compact,
            )
            .await;
        Ok(json!({
            "passes": passes,
            "stats": self.stats(),
            "pack": { "outcome": pack.outcome, "reason": pack.reason, "tokens": pack.tokens, "leaves": pack.leaf_ids, "nodes": pack.node_ids, "jev_calls": pack.jev_calls, "jev_cost": pack.jev_cost, "jev_ms": pack.jev_ms, "walk_ms": pack.walk_ms, "text": pack.text },
            "decisions": pack.decisions,
        }))
    }

    // ---- pull tools (MCP) ---------------------------------------------------------------------

    pub fn tool_search(&self, chat_id: &str, restricted: bool, query: &str, limit: i64) -> Value {
        let allowed = self.allowed_threads(chat_id, restricted);
        let ok = |tid: &str| allowed.as_ref().map(|a| a.contains(tid)).unwrap_or(true);
        let hits: Vec<Value> = self
            .store
            .t3_search(query, None, None, limit.clamp(1, 25), 0)
            .unwrap_or_default()
            .into_iter()
            .filter(|(r, _, _)| r.kind != "route" && ok(&r.thread_id))
            .map(|(r, score, snip)| json!({ "id": r.id, "kind": format!("leaf/{}", r.kind), "thread_id": r.thread_id, "when": fmt_ts(r.at), "score": -score, "snippet": clip_chars(&snip, 300) }))
            .collect();
        let words: HashSet<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_owned)
            .collect();
        let mut cards: Vec<(usize, Value)> = self
            .nodes_where("level IN ('thread','episode') AND heading <> ''", &[])
            .into_iter()
            .filter(|n| n.thread_id.as_deref().map(ok).unwrap_or(true))
            .filter_map(|n| {
                let hay = format!("{} {} {} {}", n.title, n.heading, n.summary, n.entities.join(" ")).to_lowercase();
                let ov = words.iter().filter(|w| hay.contains(w.as_str())).count();
                (ov > 0).then(|| (ov, json!({ "id": n.id, "kind": n.level, "when": format!("{} – {}", fmt_ts(n.t0), fmt_ts(n.t1)), "heading": n.heading, "summary": n.summary, "children": n.children.len() })))
            })
            .collect();
        cards.sort_by_key(|c| std::cmp::Reverse(c.0));
        json!({ "events": hits, "index_cards": cards.into_iter().take(8).map(|c| c.1).collect::<Vec<_>>() })
    }

    pub fn tool_open(&self, chat_id: &str, restricted: bool, node_id: &str) -> Value {
        let allowed = self.allowed_threads(chat_id, restricted);
        let ok = |tid: &str| allowed.as_ref().map(|a| a.contains(tid)).unwrap_or(true);
        if let Some(n) = self.node(node_id) {
            if n.thread_id.as_deref().map(|t| !ok(t)).unwrap_or(false) {
                return json!({ "error": "not visible from this chat" });
            }
            if n.level == "episode" {
                let leaves = self.leaves_by_ids(&n.children);
                let mut lines = Vec::new();
                let mut used = 0usize;
                for l in &leaves {
                    let line = self.leaf_line(l, 4000);
                    used += line.chars().count();
                    if used > 40_000 {
                        lines.push(format!(
                            "…[{} more events; use context_around]",
                            leaves.len() - lines.len()
                        ));
                        break;
                    }
                    lines.push(line);
                }
                return json!({ "id": n.id, "heading": n.heading, "summary": n.summary, "events": lines });
            }
            let kids: Vec<Value> = n
                .children
                .iter()
                .filter_map(|c| self.node(c))
                .filter(|k| k.thread_id.as_deref().map(ok).unwrap_or(true))
                .map(|k| json!({ "id": k.id, "kind": k.level, "when": format!("{} – {}", fmt_ts(k.t0), fmt_ts(k.t1)), "heading": if k.heading.is_empty() { k.title.clone() } else { k.heading.clone() }, "summary": k.summary, "status": k.status, "children": k.children.len() }))
                .collect();
            return json!({ "id": n.id, "kind": n.level, "heading": n.heading, "summary": n.summary, "children": kids });
        }
        match self.store.t3_get(node_id) {
            Ok(Some(l)) if ok(&l.thread_id) => {
                json!({ "id": l.id, "thread_id": l.thread_id, "text": self.leaf_line(&l, 12_000) })
            }
            Ok(Some(_)) => json!({ "error": "not visible from this chat" }),
            _ => json!({ "error": "no such node or event" }),
        }
    }

    pub fn tool_around(
        &self,
        chat_id: &str,
        restricted: bool,
        leaf_id: &str,
        before: i64,
        after: i64,
    ) -> Value {
        let allowed = self.allowed_threads(chat_id, restricted);
        let Ok(Some(l)) = self.store.t3_get(leaf_id) else {
            return json!({ "error": "no such event" });
        };
        if let Some(a) = &allowed {
            if !a.contains(&l.thread_id) {
                return json!({ "error": "not visible from this chat" });
            }
        }
        let before = before.clamp(0, 20);
        let after = after.clamp(0, 20);
        let mut rows = self.t3_rows(
            "WHERE thread_id = ?1 AND kind <> 'route' AND id < ?2 ORDER BY id DESC LIMIT ?3",
            &[&l.thread_id, &leaf_id, &before],
        );
        rows.reverse();
        rows.push(l.clone());
        rows.extend(self.t3_rows(
            "WHERE thread_id = ?1 AND kind <> 'route' AND id > ?2 ORDER BY id ASC LIMIT ?3",
            &[&l.thread_id, &leaf_id, &after],
        ));
        let (title, _, _) = self.thread_meta(&l.thread_id);
        json!({ "thread": title, "thread_id": l.thread_id, "events": rows.iter().map(|r| self.leaf_line(r, 2000)).collect::<Vec<_>>() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses() {
        assert_eq!(TreeMode::parse("on"), Some(TreeMode::On));
        assert_eq!(TreeMode::parse("LOG"), Some(TreeMode::Log));
        assert_eq!(TreeMode::parse("off"), Some(TreeMode::Off));
        assert_eq!(TreeMode::parse("maybe"), None);
    }

    #[test]
    fn thresholds_per_kind() {
        assert_eq!(Kind::Chat.threshold("thread"), 0.3);
        assert_eq!(Kind::Chat.threshold("leaf"), 0.4);
        assert_eq!(Kind::Tool.threshold("thread"), 0.2);
        assert_eq!(Kind::Tool.threshold("leaf"), 0.4);
    }

    #[test]
    fn card_parses_truncated_json() {
        let full = parse_card(r#"{"heading":"H","summary":"S","entities":["a"]}"#).unwrap();
        assert_eq!(full.0, "H");
        let cut = parse_card(r#"{"heading": "Only head", "summary": "cut off mid"#).unwrap();
        assert_eq!(cut.0, "Only head");
        assert_eq!(cut.1, "cut off mid");
        assert!(parse_card("no card here").is_none());
    }

    #[test]
    fn clip_counts_chars() {
        assert_eq!(clip_chars("héllo", 10), "héllo");
        assert!(clip_chars("héllo wörld", 5).starts_with("héllo …["));
    }
}
