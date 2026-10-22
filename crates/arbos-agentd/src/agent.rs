//! The project agent driver: one `pi --mode rpc` child per Project (D65), one pi
//! session per chat, the `route` tool validated here, streams folded into the
//! chat doc, subagents through the `HarnessAdapter` registry. Hard limits that
//! remain (turn deadline, context window, spend, tool timeouts) are announced
//! in the chat — never a silent stop.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Semaphore};

use arbos_core::approval::ApprovalOutcome;
use arbos_core::doc::{CommandStatus, CommandView, MessageStatus};
use arbos_core::ids::{now_ms, ulid};
use arbos_core::route::{pick_harness, validate, Harness, ValidRoute};
use arbos_core::rpc::{AssistantEvent, Command as RpcCommand, Event};
use arbos_core::widget::Widget;
use arbos_core::{TOOL_OUTPUT_SUMMARY_MAX, TURN_DEADLINE_SECS};

use crate::config::Config;
use crate::harness::{
    AgentEvent, ClaudeCodeAdapter, HarnessAdapter, PiAdapter, PrimeAdapter, StartRequest,
};
use crate::llm::{extract_return_contract, Llm};
use crate::memory::Memory;
use crate::rpc_child::RpcChild;
use crate::skills::{self, SkillsIndex};
use crate::store::Store;
use crate::sync::{SharedHost, SyncHub};

/// Streams one string field (`reply`, `ack`) out of the `route` tool's partial JSON arguments.
pub struct ReplyExtractor {
    key: &'static str,
    buf: String,
    pos: usize,
    in_string: bool,
    done: bool,
    pending_escape: Option<String>,
}

impl Default for ReplyExtractor {
    fn default() -> Self {
        Self::for_key("reply")
    }
}

impl ReplyExtractor {
    pub fn for_key(key: &'static str) -> Self {
        Self {
            key,
            buf: String::new(),
            pos: 0,
            in_string: false,
            done: false,
            pending_escape: None,
        }
    }

    /// The string closed (or was null): nothing more will come.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The `action` value seen so far in the same arguments, when it has streamed already.
    pub fn action_seen(&self) -> Option<&str> {
        let k = self.buf.find("\"action\"")?;
        let after = &self.buf[k + 8..];
        let colon = after.find(':')?;
        let rest = after[colon + 1..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(&rest[..end])
    }

    /// Feed one `toolcall_delta`; returns newly decoded text of the field.
    pub fn feed(&mut self, delta: &str) -> String {
        if self.done {
            return String::new();
        }
        self.buf.push_str(delta);
        let mut out = String::new();
        if !self.in_string {
            let needle = format!("\"{}\"", self.key);
            let Some(k) = self.buf.find(&needle) else {
                return out;
            };
            let after = &self.buf[k + needle.len()..];
            let Some(colon) = after.find(':') else {
                return out;
            };
            let rest = &after[colon + 1..];
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                return out;
            }
            if !trimmed.starts_with('"') {
                self.done = true; // null reply
                return out;
            }
            let q = self.buf.len() - trimmed.len();
            self.pos = q + 1;
            self.in_string = true;
        }
        let bytes = self.buf.as_bytes();
        let mut i = self.pos;
        let mut esc = self.pending_escape.take().unwrap_or_default();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if !esc.is_empty() {
                esc.push(c);
                if esc == "\\u" || (esc.starts_with("\\u") && esc.len() < 6) {
                    i += 1;
                    continue;
                }
                match esc.as_str() {
                    "\\n" => out.push('\n'),
                    "\\t" => out.push('\t'),
                    "\\r" => out.push('\r'),
                    "\\\"" => out.push('"'),
                    "\\\\" => out.push('\\'),
                    "\\/" => out.push('/'),
                    s if s.starts_with("\\u") && s.len() == 6 => {
                        if let Some(ch) = u32::from_str_radix(&s[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                    }
                    other => out.push_str(&other[1..]),
                }
                esc.clear();
                i += 1;
                continue;
            }
            if c == '\\' {
                esc.push('\\');
                i += 1;
                continue;
            }
            if c == '"' {
                self.done = true;
                i += 1;
                break;
            }
            // Multi-byte UTF-8: copy whole chars.
            let ch_len = utf8_len(bytes[i]);
            if i + ch_len > bytes.len() {
                break;
            }
            out.push_str(&self.buf[i..i + ch_len]);
            i += ch_len;
        }
        self.pos = i;
        if !esc.is_empty() {
            self.pending_escape = Some(esc);
        }
        out
    }
}

/// Cheap post-check on main-agent prose as it streams into the doc (demo review v2): drops
/// leading boilerplate lines ("Called 3 tools", "Running the tests…", "Route accepted: …"),
/// leading thought-aloud sentences ("The user is asking…", "I should reply…"), trailing
/// tool-narration ("Checking the latest…"), glued ".Word" → ". Word" / "findingsDon't" →
/// "findings Don't", and whole
/// ```json fences. Text is held back only until a line boundary, so streaming stays live.
#[derive(Default)]
pub struct ProseFilter {
    pending: String,
    in_fence: bool,
    fence_hidden: bool,
    started: bool,
}

impl ProseFilter {
    /// First-line boilerplate: narration about the mechanics, never an answer.
    fn is_boilerplate(line: &str) -> bool {
        let l = line
            .trim()
            .trim_start_matches(['*', '_', '#', '>', '-', ' '])
            .to_ascii_lowercase();
        if l.is_empty() {
            return true;
        }
        let starts = |p: &[&str]| p.iter().any(|x| l.starts_with(x));
        let tool_count = {
            // "called 3 tools", "i ran 2 tool calls.", "(4 tool calls)"
            let t = l
                .trim_start_matches(['(', 'i', ' '])
                .trim_start_matches("i ");
            let t = t
                .trim_start_matches("called ")
                .trim_start_matches("ran ")
                .trim_start_matches("used ")
                .trim_start_matches("made ")
                .trim_start_matches("executed ");
            let mut it = t.splitn(2, ' ');
            matches!((it.next().map(|n| n.parse::<u32>().is_ok()), it.next()), (Some(true), Some(rest)) if rest.starts_with("tool"))
        };
        let mechanics = starts(&[
            "tool call",
            "tool calls",
            "tools called",
            "route accepted",
            "routing:",
            "route:",
        ]);
        // Verb-led narration is short ("Running the tests…"); a real sentence that happens to
        // start with the same word runs long and stays.
        let narration = l.chars().count() <= 90
            && starts(&[
                "running ",
                "executing ",
                "calling ",
                "invoking ",
                "let me check",
                "let me run",
                "let me look",
                "i'll check",
                "i will check",
                "i'll run",
                "i will run",
                "checking ",
                "reading the file",
                "first, i",
                "first i",
                "now i'll",
                "now i will",
            ]);
        tool_count || mechanics || narration
    }

    /// A sentence that is the model talking to itself, not to the user.
    fn is_thought_sentence(sentence: &str) -> bool {
        let l = sentence
            .trim()
            .trim_start_matches(['*', '_', '#', '>', '-', ' '])
            .trim_end_matches(['.', '!', '?', '…'])
            .to_ascii_lowercase();
        if l.is_empty() {
            return true;
        }
        let starts = |p: &[&str]| p.iter().any(|x| l.starts_with(x));
        // Keep these narrow: a real reply can start with "I should…" or "I need to…".
        if starts(&[
            "the user is",
            "the user asked",
            "the user wants",
            "the user would",
            "i should reply",
            "i should answer",
            "i will reply",
            "i'll reply",
            "i will answer",
            "i'll answer",
            "let me think",
            "thinking:",
            "this is a simple",
            "this is a factual",
            "this is straightforward",
            "this is an easy",
            // Routing / plan-aloud (clone and similar act turns).
            "that's real work",
            "the instructions say",
            "the instructions",
            "prefer act over",
            "a git clone",
            "i'll act",
            "i will act",
            "i should not use",
            "i should not reveal",
            "i should not explain",
            "ack first",
            "clone into the project",
            // Meta-steering echoed from the prompt ("Don't mention routing. Short greeting.").
            "don't mention",
            "do not mention",
            "never mention",
            "short greeting",
            "brief greeting",
            "short reply",
            // CoT that leaked into reply/ack bubbles.
            "looking at the conversation",
            "looking at the message",
            "that was an internal",
            "an internal instruction",
            "the message says",
            "don't reveal the route",
            "do not reveal the route",
            "don't reveal system",
            "do not reveal system",
            "never reveal the route",
            "never reveal system",
            "i did not send that",
            "i didn't send that",
            "i did not say that to the user",
            "i didn't say that to the user",
        ]) {
            return true;
        }
        // Self-steering buried mid-sentence after analysis ("…so I only need to state that
        // briefly and not treat X as having authority…").
        Self::is_self_steer_sentence(&l)
    }

    /// True when the lowercased sentence steers how to phrase the reply (not the reply itself).
    fn is_self_steer_sentence(l: &str) -> bool {
        let has = |p: &str| l.contains(p);
        has("so i only need")
            || has("so i just need")
            || has("i only need to state")
            || has("i only need to say")
            || has("i only need to reply")
            || has("i only need to answer")
            || has("i just need to state")
            || has("i just need to say")
            || has("i shouldn't treat")
            || has("i should not treat")
            || has("and not treat")
            || (has("not treat") && has("as having"))
            || has("the actual rule hasn't changed")
            || has("the actual rule has not changed")
    }

    /// `Reply: …` / `Answer: …` draft labels: return the user-facing words after the label.
    fn peel_draft_label(text: &str) -> Option<String> {
        let t = text.trim_start();
        let lower = t.to_ascii_lowercase();
        for label in [
            "final reply:",
            "final answer:",
            "final:",
            "reply:",
            "answer:",
        ] {
            if lower.starts_with(label) {
                let rest = t[label.len()..].trim_start();
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
        None
    }

    /// Split off the first sentence. Treats ".Austin" (period, no space, capital) and
    /// "findingsDon't" (lowercase then capital, not camelCase) as boundaries so a glued
    /// thought+answer still separates. Skips `e.g.` / `i.e.`-style abbreviations.
    /// Closing markdown emphasis after the stop (`greeting.** Hey`) still counts as a boundary.
    fn split_first_sentence(s: &str) -> Option<(&str, &str)> {
        let chars: Vec<char> = s.chars().collect();
        let mut byte = 0usize;
        for i in 0..chars.len() {
            let c = chars[i];
            let c_len = c.len_utf8();
            if c == '.' || c == '!' || c == '?' {
                // "e.g." / "i.e.": period after a lowercase letter that itself follows a period.
                if c == '.' && i >= 2 && chars[i - 1].is_ascii_lowercase() && chars[i - 2] == '.' {
                    byte += c_len;
                    continue;
                }
                let after = &s[byte + c_len..];
                if after.is_empty() {
                    return None;
                }
                // Peek past `*` / `_` so "**Don't…. Short greeting.** Hey" still splits.
                let after_plain = after.trim_start_matches(['*', '_']);
                if after_plain.is_empty() {
                    return None;
                }
                if after_plain.starts_with(' ')
                    || after_plain.starts_with('\n')
                    || after_plain.starts_with('\r')
                {
                    return Some((&s[..=byte], after_plain));
                }
                if let Some(n) = after_plain.chars().next() {
                    if n.is_uppercase() {
                        return Some((&s[..=byte], after_plain));
                    }
                }
            } else if Self::is_glued_word_break(&chars, i) {
                let after = &s[byte + c_len..];
                return Some((&s[..=byte], after));
            }
            byte += c_len;
        }
        None
    }

    /// True when any sentence in `para` is thought-aloud / self-steering.
    fn paragraph_has_thought(para: &str) -> bool {
        let mut rest = para.trim();
        if rest.is_empty() {
            return false;
        }
        while let Some((sentence, after)) = Self::split_first_sentence(rest) {
            if Self::is_thought_sentence(sentence) {
                return true;
            }
            rest = after.trim_start();
        }
        Self::is_thought_sentence(rest)
    }

    /// Drop leading blank-line-separated paragraphs that contain thought sentences.
    /// CoT often lands as its own paragraph before the real answer.
    fn strip_leading_thought_paragraphs(text: &str) -> String {
        let mut rest = text.trim_start();
        while let Some(i) = rest.find("\n\n") {
            let para = &rest[..i];
            let after = rest[i..].trim_start();
            if !Self::paragraph_has_thought(para) {
                break;
            }
            rest = after;
            if rest.is_empty() {
                return String::new();
            }
        }
        rest.to_string()
    }

    /// When analysis precedes a self-steer sentence in the same paragraph, drop from the
    /// start through that self-steer (inclusive). Keeps the user-facing answer after.
    fn strip_through_self_steer(text: &str) -> String {
        let first_para = match text.find("\n\n") {
            Some(i) => &text[..i],
            None => text,
        };
        let after_para = text.get(first_para.len()..).unwrap_or("");
        let mut rest = first_para.trim_start();
        let mut kept = String::new();
        let mut saw_self_steer = false;
        while let Some((sentence, after)) = Self::split_first_sentence(rest) {
            let lower = sentence
                .trim()
                .trim_start_matches(['*', '_', '#', '>', '-', ' '])
                .trim_end_matches(['.', '!', '?', '…'])
                .to_ascii_lowercase();
            if Self::is_self_steer_sentence(&lower) {
                saw_self_steer = true;
                kept.clear();
                rest = after.trim_start();
                continue;
            }
            if saw_self_steer {
                // After a self-steer, keep whatever remains in this paragraph.
                if !kept.is_empty() && !kept.ends_with([' ', '\n', '\r']) {
                    kept.push(' ');
                }
                kept.push_str(sentence.trim());
            } else {
                if !kept.is_empty() && !kept.ends_with([' ', '\n', '\r']) {
                    kept.push(' ');
                }
                kept.push_str(sentence.trim());
            }
            rest = after.trim_start();
        }
        if !rest.is_empty() {
            let lower = rest
                .trim()
                .trim_start_matches(['*', '_', '#', '>', '-', ' '])
                .trim_end_matches(['.', '!', '?', '…'])
                .to_ascii_lowercase();
            if Self::is_self_steer_sentence(&lower) {
                saw_self_steer = true;
                kept.clear();
            } else if saw_self_steer || !kept.is_empty() {
                if !kept.is_empty() && !kept.ends_with([' ', '\n', '\r']) {
                    kept.push(' ');
                }
                kept.push_str(rest.trim());
            } else {
                kept.push_str(rest.trim());
            }
        }
        if !saw_self_steer {
            return text.to_string();
        }
        let mut out = kept;
        if !after_para.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(after_para.trim_start());
        }
        out
    }

    /// Drop leading thought-aloud sentences; keep the user-facing answer.
    /// A `Reply:` / `Answer:` label after thoughts marks the real answer.
    /// Leading paragraphs that contain self-steering / CoT are dropped first.
    fn strip_thought_prefix(text: &str) -> String {
        let (body, nl) = match text.strip_suffix('\n') {
            Some(b) => match b.strip_suffix('\r') {
                Some(b2) => (b2, "\r\n"),
                None => (b, "\n"),
            },
            None => (text, ""),
        };
        let mut rest = body.trim_start().to_string();
        rest = Self::strip_leading_thought_paragraphs(&rest);
        if rest.is_empty() {
            return String::new();
        }
        rest = Self::strip_through_self_steer(&rest);
        if rest.is_empty() {
            return String::new();
        }
        loop {
            if let Some(peeled) = Self::peel_draft_label(&rest) {
                rest = peeled;
                break;
            }
            let Some((sentence, after)) = Self::split_first_sentence(&rest) else {
                break;
            };
            if !Self::is_thought_sentence(sentence) {
                break;
            }
            rest = after.trim_start().to_string();
        }
        if rest.is_empty() {
            return String::new();
        }
        if Self::is_thought_sentence(&rest) && Self::split_first_sentence(&rest).is_none() {
            // Whole remainder is one thought sentence (no answer after).
            return String::new();
        }
        format!("{rest}{nl}")
    }

    /// Drop trailing tool-narration / boilerplate sentences ("Checking the latest…").
    fn strip_trailing_narration(text: &str) -> String {
        let (body, nl) = match text.strip_suffix('\n') {
            Some(b) => match b.strip_suffix('\r') {
                Some(b2) => (b2, "\r\n"),
                None => (b, "\n"),
            },
            None => (text, ""),
        };
        if body.trim().is_empty() {
            return String::new();
        }
        let mut sentences: Vec<String> = Vec::new();
        let mut rest = body.trim_start();
        while let Some((sentence, after)) = Self::split_first_sentence(rest) {
            sentences.push(sentence.trim().to_string());
            rest = after.trim_start();
        }
        if !rest.is_empty() {
            sentences.push(rest.trim().to_string());
        }
        while sentences
            .last()
            .is_some_and(|s| Self::is_boilerplate(s) || Self::is_thought_sentence(s))
        {
            sentences.pop();
        }
        if sentences.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for s in sentences {
            if !out.is_empty() && !out.ends_with([' ', '\n', '\r']) {
                out.push(' ');
            }
            out.push_str(&s);
        }
        format!("{out}{nl}")
    }

    /// True when `s` is (or is still typing toward) a short tool-narration sentence.
    fn could_be_narration_prefix(s: &str) -> bool {
        let l = s
            .trim()
            .trim_start_matches(['*', '_', '#', '>', '-', ' '])
            .to_ascii_lowercase();
        if l.is_empty() {
            return false;
        }
        if Self::is_boilerplate(s) {
            return true;
        }
        const STARTERS: &[&str] = &[
            "running ",
            "executing ",
            "calling ",
            "invoking ",
            "let me check",
            "let me run",
            "let me look",
            "i'll check",
            "i will check",
            "i'll run",
            "i will run",
            "checking ",
            "reading the file",
            "first, i",
            "first i",
            "now i'll",
            "now i will",
        ];
        STARTERS
            .iter()
            .any(|st| st.starts_with(&l) || l.starts_with(st))
    }

    /// Split after the last sentence-ending punctuation (including glued ".Word").
    /// `None` when no sentence has finished yet. Peek past closing `*` / `_` the same way
    /// `split_first_sentence` does.
    fn split_at_last_sentence_end(s: &str) -> Option<(&str, &str)> {
        let bytes = s.as_bytes();
        let mut last: Option<usize> = None;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'.' || b == b'!' || b == b'?' {
                if b == b'.' && i >= 2 && bytes[i - 1].is_ascii_lowercase() && bytes[i - 2] == b'.'
                {
                    i += 1;
                    continue;
                }
                let after = &s[i + 1..];
                let after_plain = after.trim_start_matches(['*', '_']);
                if after.is_empty()
                    || after_plain.is_empty()
                    || after_plain.starts_with(' ')
                    || after_plain.starts_with('\n')
                    || after_plain.starts_with('\r')
                    || after_plain.chars().next().is_some_and(|c| c.is_uppercase())
                {
                    last = Some(i + 1);
                }
            }
            i += utf8_len(bytes[i]);
        }
        let end = last?;
        Some((&s[..end], &s[end..]))
    }

    /// True when `chars[i]` (lowercase) is glued to an uppercase letter that starts a new
    /// sentence/word ("findingsDon't"), not mid-camelCase ("GitHub", "iPhone").
    fn is_glued_word_break(chars: &[char], i: usize) -> bool {
        let Some(next) = chars.get(i + 1) else {
            return false;
        };
        if !chars[i].is_lowercase() || !next.is_uppercase() {
            return false;
        }
        // Next must continue as a word ("Don't", "Auditing"), not an acronym ("USA").
        let continues = match chars.get(i + 2) {
            None => true,
            Some(n) => n.is_lowercase() || *n == '\'',
        };
        if !continues {
            return false;
        }
        // Lowercase run ending at i; skip when preceded by a letter (camelCase).
        let mut run_start = i;
        while run_start > 0 && chars[run_start - 1].is_lowercase() {
            run_start -= 1;
        }
        if run_start > 0 && chars[run_start - 1].is_ascii_alphabetic() {
            return false;
        }
        // "iPhone": single leading lowercase after a boundary — leave it.
        (i + 1 - run_start) >= 2
    }

    /// Insert a space after `.` / `!` / `?` when the next character is an uppercase letter
    /// ("have.Checking" → "have. Checking"), and between a lowercase run and an uppercase
    /// sentence start ("findingsDon't" → "findings Don't"). Skips `e.g.` / `i.e.` and
    /// camelCase like `GitHub`.
    fn fix_glued_spaces(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len() + 8);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            out.push(c);
            if matches!(c, '.' | '!' | '?') {
                let abbrev =
                    c == '.' && i >= 2 && chars[i - 1].is_ascii_lowercase() && chars[i - 2] == '.';
                if !abbrev {
                    if let Some(next) = chars.get(i + 1) {
                        if next.is_uppercase() {
                            out.push(' ');
                        }
                    }
                }
            } else if Self::is_glued_word_break(&chars, i) {
                out.push(' ');
            }
            i += 1;
        }
        out
    }

    /// Strip thought-aloud and trailing tool-narration; fix glued sentence spacing.
    pub fn clean_prose(text: &str) -> String {
        let stripped = Self::strip_thought_prefix(text);
        if stripped.trim().is_empty() {
            return String::new();
        }
        let without_tail = Self::strip_trailing_narration(&stripped);
        if without_tail.trim().is_empty() {
            return String::new();
        }
        Self::fix_glued_spaces(&without_tail)
    }

    fn take_line(&mut self, line: &str) -> Option<String> {
        let t = line.trim_start();
        if self.in_fence {
            if t.starts_with("```") {
                self.in_fence = false;
                let hidden = self.fence_hidden;
                self.fence_hidden = false;
                return if hidden { None } else { Some(line.to_owned()) };
            }
            return if self.fence_hidden {
                None
            } else {
                Some(line.to_owned())
            };
        }
        if t.starts_with("```") {
            let lang = t.trim_start_matches('`').trim().to_ascii_lowercase();
            self.in_fence = true;
            self.fence_hidden = lang.starts_with("json");
            return if self.fence_hidden {
                None
            } else {
                Some(line.to_owned())
            };
        }
        if Self::is_boilerplate(line) {
            return None;
        }
        let cleaned = Self::clean_prose(line);
        if cleaned.trim().is_empty() {
            return None;
        }
        self.started = true;
        Some(cleaned)
    }

    /// Feed a delta; returns the text that may enter the doc now.
    pub fn feed(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut out = String::new();
        while let Some(i) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=i).collect();
            if let Some(l) = self.take_line(&line) {
                out.push_str(&l);
            }
        }
        // A partial line streams at once when it cannot be boilerplate or a fence any more.
        let p = self.pending.trim_start();
        let could_be_fence = p.is_empty() || "```".starts_with(p) || p.starts_with("```");
        if self.started && !self.in_fence && !could_be_fence {
            // Emit through the last finished sentence (cleaned); hold a trailing fragment
            // that still looks like tool-narration so "Checking…" never lands mid-stream.
            let raw = std::mem::take(&mut self.pending);
            if let Some((complete, rest)) = Self::split_at_last_sentence_end(&raw) {
                out.push_str(&Self::clean_prose(complete));
                if !rest.is_empty() && Self::could_be_narration_prefix(rest) {
                    self.pending = rest.to_string();
                } else if !rest.is_empty() {
                    let rest = Self::fix_glued_spaces(rest.trim_start());
                    if !rest.is_empty() {
                        if !out.is_empty()
                            && !out.ends_with([' ', '\n', '\r'])
                            && rest.chars().next().is_some_and(|c| c.is_uppercase())
                            && (out.ends_with(['.', '!', '?'])
                                || out.chars().last().is_some_and(|c| c.is_lowercase()))
                        {
                            out.push(' ');
                        }
                        out.push_str(&rest);
                    }
                }
            } else if Self::could_be_narration_prefix(&raw) {
                self.pending = raw;
            } else {
                out.push_str(&Self::fix_glued_spaces(&raw));
            }
        } else if !self.started && !self.in_fence && self.pending.len() > 160 {
            // Long first line: real prose. Judge it once and let it through.
            if Self::is_boilerplate(&self.pending) {
                self.pending.clear();
            } else {
                let cleaned = Self::clean_prose(&self.pending);
                self.pending.clear();
                if cleaned.trim().is_empty() {
                    // Entire buffer was thought-aloud / narration; keep waiting for real prose.
                } else {
                    self.started = true;
                    out.push_str(&cleaned);
                }
            }
        }
        out
    }

    /// End of a text run (tool call or turn end): judge what is still held back.
    pub fn flush(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        if rest.is_empty() {
            return rest;
        }
        if self.in_fence {
            // An unterminated fence at the end: drop it when hidden, else let it through.
            self.in_fence = false;
            let hidden = self.fence_hidden;
            self.fence_hidden = false;
            return if hidden { String::new() } else { rest };
        }
        self.take_line(&rest).unwrap_or_default()
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Subagents running at once per box; further spawns wait in FIFO order (demo review item 2).
pub const SUBAGENT_MAX_CONCURRENT: usize = 4;

/// Pack section for every subagent: an unknown service, API or tool is researched, not guessed
/// at and not handed back to the user. Mirrors the main agent's rule in `prompts/router.md`.
const RESEARCH_SECTION: &str = "## Unknown services, APIs and tools\n\nWhen the task involves a service, API, CLI or library you do not know, or a call fails (not found, unknown endpoint, auth error, unexpected shape), find the answer yourself; guessing more than one or two URLs is not research. Until one path works: `web_search` for the service's API reference or developer docs and `web_fetch` them (the docs site, its OpenAPI/Swagger spec at `/openapi.json`, `/swagger.json`, `/docs`, `/redoc`, `/llms.txt`, the README); find the official SDK or CLI, install it (`uv tool install`, `pip install`, `npm i -g`, a release binary) and read its `--help` and, when needed, its source for the real base URL, routes and auth header; read the service's GitHub repositories and examples; read the error body. Read what you fetched before the next call. Authenticated calls go through `egress` with the secret's placeholder in a header or the body.\n\nReport `blocked` or `needs_input` only for a real blocker: a credential or account that does not exist, a choice only the user can make between real options you found, a spend decision, or a service that is down after you checked it against its docs. Never ask for docs, an API reference, an endpoint, or for the user to install something. Money: anything that charges the user beyond what the brief states (renting, buying, subscribing, paying) needs their go-ahead first: show the chosen option with its price (and a cheaper alternative when one exists) and ask with `ask_user`; listing options and quoting prices is free.\n\nSecret notes: each secret the owner stored comes with their note (`use_notes` in `secrets_list`, comments in your shell's secrets.env); the note is the policy for that key — for `egress` and for any CLI, SDK or script that reads the real `$NAME`. Spending guidance in it binds you: a limit (\"may spend up to $X without asking\") lets you proceed within it and no further; \"confirm before charging\" or \"read-only\" means ask first or never charge, whatever the brief says. A note without spending guidance leaves the money rule above in force. Never pass a confirmation-skipping flag (`--yes`, `-y`, `--force`, `--no-confirm` and the like) to a command that charges unless the note allows that spend.\n\n";

/// Code mode (D86), appended to the byte-stable router prompt when `Config::code_mode()` is on: the
/// main agent batches and filters tool work in one `codemode` program per step instead of a chain of
/// single calls. Exact script names are given so the model does not spend a round trip guessing them
/// (observed: `tools.arbos_submit_feedback` TypeErrors). Off: the prompt is unchanged byte for byte.
pub const CODE_MODE_SECTION: &str = "## Code mode\n\n`codemode` runs a JavaScript program that calls your tools as async functions; only what the program prints or returns reaches you. Use one program instead of a chain of separate calls whenever the next step does not depend on your reading the previous result: read several files at once (`Promise.all`), run a search and the reads it points to, run the tests and keep only the failures, call several `arbos` tools together. Filter in code and return only what you need (counts, matching lines, the first rows, a short summary), never the raw output. Direct `read`/`bash`/`edit`/`write` calls remain right for a single step, and for an edit you must see first.\n\nNames inside a program: `tools.read({path})`, `tools.bash({command})` (resolves to `{output, exit_code}`), `tools.edit({path, edits: [{oldText, newText}]})`, `tools.write({path, content})`; every `arbos` project tool is `tools.mcp__arbos__<name>(args)`, for example `tools.mcp__arbos__web_search({query})`, `tools.mcp__arbos__web_fetch({url})`, `tools.mcp__arbos__context_search({query})`, `tools.mcp__arbos__history_search({query})`, `tools.mcp__arbos__secrets_list({})`, `tools.mcp__arbos__egress({url, method, headers, body})`, `tools.mcp__arbos__deliver_file({path, title})`, `tools.mcp__arbos__routines_create({...})`, `tools.mcp__arbos__remember({name, value})`, `tools.mcp__arbos__recall({name})`, `tools.mcp__arbos__submit_feedback({description})`. `await describeTool(\"mcp__arbos__<name>\")` shows a tool's arguments; `await searchTools(\"...\")` finds the rest. Never call `route`, `ask_user` or `send_message` from a program: those are direct calls only, and a program that tries is refused. A program has exactly your permissions: the deny-list, approval cards, secret placeholders and spending rules apply to every call it makes, and a refused call rejects with the reason. A program stops after 15 minutes; when one does, say so in one sentence and continue in smaller steps.\n";

/// Context size of one model call as pi counts it: `totalTokens`, else input + output + cache
/// read + cache write.
fn context_tokens(usage: &Value) -> i64 {
    let total = usage
        .get("totalTokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if total > 0 {
        return total;
    }
    ["input", "output", "cacheRead", "cacheWrite"]
        .iter()
        .map(|k| usage.get(*k).and_then(Value::as_i64).unwrap_or(0))
        .sum()
}

/// What the rolling summary must keep when agentd compacts a chat (D91). Anything it drops is
/// still one memory walk away, but facts the user stated and the thread of the current task
/// should not need a lookup.
const SOFT_COMPACTION_INSTRUCTIONS: &str = "Keep, exactly as written, every fact the user stated about themselves or their preferences, every decision and rule agreed, every number, date, name, file path, URL, identifier and command that was used or produced, and every open question or task with its current state. State the current goal and what the next step was going to be. Older detail you leave out is still retrievable with context_search / context_open, so prefer precision over coverage, and keep the whole summary under about 1,500 words.";

/// A memory walk running beside the turn's setup. Dropped without `block` (stop, preempt, an
/// error before the prompt) it is aborted, so no Jev request is spent on a turn that never ran.
pub struct PendingMemory(Option<tokio::task::JoinHandle<Option<String>>>);

impl PendingMemory {
    pub fn none() -> Self {
        Self(None)
    }

    /// Wait for the walk; `None` when there was none, it failed, or it had nothing to inject.
    pub async fn block(mut self) -> Option<String> {
        match self.0.take() {
            Some(h) => h.await.ok().flatten(),
            None => None,
        }
    }
}

impl Drop for PendingMemory {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Why the main pi session is being prompted.
#[derive(Debug, Clone)]
pub enum TurnKind {
    /// A user message (`send` / `answer`).
    User,
    /// A finished subagent's result, relayed to the user in prose by the main agent.
    SubagentResult { subagent_id: String, name: String },
    /// An approval card was decided after its turn ended (feature `approve`): the main agent
    /// dispatches or acknowledges, in prose.
    ApprovalOutcome { approval_id: String, title: String },
}

/// What one main-agent turn produced, for the caller's follow-ups (memory pass, hand-offs).
pub struct TurnOutcome {
    pub assistant_msg_id: String,
    pub route_name: &'static str,
    pub turn: TurnState,
    pub result: Result<()>,
}

#[derive(Debug, Clone, Default)]
pub struct TurnMetrics {
    pub ttft_ms: Option<i64>,
    pub route_ms: Option<i64>,
    /// Tool calls the model issued itself (a `codemode` script counts once).
    pub tool_calls: i64,
    /// Calls made from inside `codemode` scripts (D86); logged like direct calls, counted apart.
    pub nested_calls: i64,
    /// Assistant messages = model round trips of this turn.
    pub model_calls: i64,
    /// Usage of the last model call (what the clients show today).
    pub usage: Option<Value>,
    /// Summed usage of every model call of the turn (`input`, `output`, `cacheRead`, `cacheWrite`, `cost`).
    pub usage_total: Option<Value>,
    pub route_missing: bool,
    pub route_corrected: bool,
}

impl TurnMetrics {
    /// Fold one finished assistant message's `usage` into the per-turn totals.
    pub fn add_model_call(&mut self, usage: Option<&Value>) {
        self.model_calls += 1;
        let Some(u) = usage else {
            return;
        };
        let mut tot = self.usage_total.take().unwrap_or_else(
            || json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "cost": 0.0 }),
        );
        for k in ["input", "output", "cacheRead", "cacheWrite"] {
            let add = u.get(k).and_then(Value::as_i64).unwrap_or(0);
            let cur = tot.get(k).and_then(Value::as_i64).unwrap_or(0);
            tot[k] = json!(cur + add);
        }
        let add_cost = u
            .get("cost")
            .and_then(|c| {
                c.get("total")
                    .and_then(Value::as_f64)
                    .or_else(|| c.as_f64())
            })
            .unwrap_or(0.0);
        let cur_cost = tot.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        tot["cost"] = json!(cur_cost + add_cost);
        self.usage_total = Some(tot);
    }

    /// The `usage` field of the result row and of `turn_metrics`: the last model call's usage as
    /// before (clients read it today), plus the per-turn totals and call counts.
    pub fn usage_for_doc(&self) -> Value {
        let mut v = match &self.usage {
            Some(u) => u.clone(),
            None => match &self.usage_total {
                Some(_) => json!({}),
                None => return Value::Null,
            },
        };
        if let Some(t) = &self.usage_total {
            v["total"] = t.clone();
        }
        v["modelCalls"] = json!(self.model_calls);
        v["toolCalls"] = json!(self.tool_calls);
        v["nestedCalls"] = json!(self.nested_calls);
        v
    }
}

pub struct TurnState {
    pub chat_id: String,
    pub user_msg_id: String,
    pub assistant_msg_id: String,
    pub text_part_id: String,
    pub started: Instant,
    pub route: Option<ValidRoute>,
    pub route_attempts: u32,
    pub route_failed: bool,
    pub escalate: bool,
    pub asked: Option<String>,
    pub metrics: TurnMetrics,
    pub streamed_text: String,
    pub act_transcript: Vec<String>,
    /// call id → args from `tool_execution_start`, so the tool part records the real input.
    pub tool_inputs: HashMap<String, Value>,
    pub reply_extractor: ReplyExtractor,
    /// Streams `route.ack` into the first row of the turn (agent-flow contract §1).
    pub ack_extractor: ReplyExtractor,
    /// The acknowledgement sentence written so far.
    pub ack_text: String,
    /// The ack row was closed; `assistant_msg_id` now names the result row (or none, see `row_closed`).
    pub ack_split: bool,
    /// No live row right now (a `new`/`wake` turn ended on its ack): the next text opens one.
    pub row_closed: bool,
    /// Progress bubbles sent with `send_message` this turn.
    pub progress_texts: Vec<String>,
    /// The `activity` currently on the live row, so unchanged labels are not rewritten.
    pub activity: Option<String>,
    /// Boilerplate filter on everything the main agent says in this turn.
    pub prose: ProseFilter,
    /// Act turns: prose is held here and only the text after the last tool call reaches the doc;
    /// anything the model said between tool calls is narration and goes to T3 only.
    pub act_buffer: String,
    pub route_call_id: Option<String>,
    pub text_part_dirty: bool,
    /// Set by an out-of-band `stop`: the turn ends as it is and no hand-off follows.
    pub stopped: bool,
    /// This turn was started by a voice-call `ask_agent` / `look` (user row `voice: true`).
    /// A later typed `send` preempts it so text chat is not stuck behind a hung call turn.
    pub from_voice: bool,
    /// The text pi was prompted with; the skills selector matches against it (F5).
    pub prompt_text: String,
    /// Skills injected into this turn's working set (recorded once, in the Details drawer).
    pub skills_used: Vec<String>,
    /// The model call ended in an error (`message_end` with `stopReason: error`, or a streamed
    /// `error` event). pi 1.2 reports provider errors — a refusal, a context overflow, a 5xx —
    /// only on the final message, so this is the only place they surface.
    pub model_error: Option<ModelError>,
    /// Fresh-session retry after a refusal already happened this turn (at most one).
    pub session_rotated: bool,
}

/// How the model call failed, as pi reported it.
#[derive(Clone, Debug)]
pub struct ModelError {
    /// Provider text (Anthropic's stop explanation, an HTTP body, …). Never shown as is.
    pub detail: String,
    /// `stop_reason: refusal` — the provider declined this context, not a transient failure.
    pub refusal: bool,
}

impl ModelError {
    /// Build from a pi assistant message that ended with `stopReason: error`. An `aborted` stop
    /// is agentd's own doing (user stop, turn deadline) and is announced where it happens.
    pub fn from_message(message: &Value) -> Option<Self> {
        if message.get("stopReason").and_then(Value::as_str)? != "error" {
            return None;
        }
        let raw = message.get("rawStopReason").and_then(Value::as_str);
        let detail = message
            .get("errorMessage")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("provider error")
            .to_owned();
        let lower = detail.to_ascii_lowercase();
        Some(Self {
            refusal: raw == Some("refusal")
                || lower.contains("refus")
                || lower.contains("was blocked")
                || lower.contains("terms of service"),
            detail,
        })
    }

    /// One short line for the chat, first person like the limit announcements (#1037).
    pub fn chat_line(&self) -> String {
        if self.refusal {
            return "I couldn't answer that: the model provider declined the request. Ask me again in a moment.".to_owned();
        }
        if let Some((notice, _)) = classify_hard_limit(&self.detail) {
            return notice.to_owned();
        }
        "I couldn't answer that: the model call failed. Ask me again in a moment.".to_owned()
    }

    /// Error-part code the clients key on.
    pub fn code(&self) -> &'static str {
        if self.refusal {
            "model_refused"
        } else {
            "model_error"
        }
    }
}

impl TurnState {
    /// Placeholder installed as soon as `handle_send` takes `turn_lock`, before attachments /
    /// `tree_pack` / `drive_turn`. `voice_cancel` and typed-send preempt need a real `TurnState`
    /// in that window — without it they no-op and the lock is held forever
    /// (feedback app-01M5CT92B0NPK1X09YW5RHNRN8, follow-up to app-01M5B5ZZNJP3W8770YVMGXPBKY).
    fn claim(chat_id: &str, user_msg_id: &str, from_voice: bool) -> Self {
        Self {
            chat_id: chat_id.to_owned(),
            user_msg_id: user_msg_id.to_owned(),
            assistant_msg_id: String::new(),
            text_part_id: String::new(),
            started: Instant::now(),
            route: None,
            route_attempts: 0,
            route_failed: false,
            escalate: false,
            asked: None,
            metrics: TurnMetrics::default(),
            streamed_text: String::new(),
            act_transcript: Vec::new(),
            tool_inputs: HashMap::new(),
            reply_extractor: ReplyExtractor::default(),
            ack_extractor: ReplyExtractor::for_key("ack"),
            ack_text: String::new(),
            ack_split: false,
            row_closed: false,
            progress_texts: Vec::new(),
            activity: None,
            prose: ProseFilter::default(),
            act_buffer: String::new(),
            route_call_id: None,
            text_part_dirty: false,
            stopped: false,
            from_voice,
            prompt_text: String::new(),
            skills_used: Vec::new(),
            model_error: None,
            session_rotated: false,
        }
    }
}

pub struct Agent {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub sync: SyncHub,
    pub llm: Arc<Llm>,
    pub memory: Arc<Memory>,
    /// Memory tree (D77): read-only side index over T3, walked by the decision model per turn.
    pub tree: Arc<crate::tree::Tree>,
    pi: Mutex<Option<Arc<RpcChild>>>,
    current_chat: Mutex<Option<String>>,
    pub turn: Arc<Mutex<Option<TurnState>>>,
    adapters: HashMap<Harness, Arc<dyn HarnessAdapter>>,
    pub turn_running: AtomicBool,
    /// Subagent turns in flight (keep-alive must cover them too, design §4.6).
    pub subagent_turns: std::sync::atomic::AtomicU32,
    pub pi_ready: AtomicBool,
    pub ext_loaded: AtomicBool,
    /// Set by `shutdown`: the pi supervisor must not respawn a child we are stopping (an orphaned
    /// pi dies with `write EPIPE` at its next reply once agentd is gone).
    pub shutting_down: AtomicBool,
    turn_lock: Mutex<()>,
    /// FIFO gate on concurrent subagent runs in this workspace (`SUBAGENT_MAX_CONCURRENT`).
    subagent_slots: Arc<Semaphore>,
    /// Box-wide gate shared by every workspace (`workspace::BOX_MAX_SUBAGENTS`).
    box_slots: Arc<Semaphore>,
    http: reqwest::Client,
    pub settings: crate::settings::Settings,
    /// Cached skills index (F5): shared + this Project's own, bodies included.
    pub skills: SkillsIndex,
    /// Pending approval cards and short-lived grants (feature `approve`).
    pub approvals: crate::approval::Approvals,
    /// Running subagents → (chat id, progress message id), so a file a subagent delivers lands
    /// on its own card (F10 `deliver_file`).
    pub subagent_msgs: Mutex<HashMap<String, (String, String)>>,
    /// Group-chat bookkeeping (chat kinds from the registry, this box's "Projects ⇄" tab).
    pub groups: crate::groups::GroupState,
    pub people: crate::people::PeopleState,
    /// Image inputs for the next pi prompt (set by `handle_send` from the message's attachments).
    pub turn_images: std::sync::Mutex<Vec<Value>>,
    /// Cached active-goals blurb for the working set (Goals, feedback #1091).
    pub goals_cache: std::sync::Mutex<String>,
    /// D87: the project-memory text last written into the system prompt and when (debounce).
    pub prompt_memory: std::sync::Mutex<Option<(String, Instant)>>,
    /// D91 soft-compaction hysteresis per chat: `None` right after a compaction (the floor is
    /// not known yet), then the window observed on the first turn after it.
    compaction_floor: std::sync::Mutex<HashMap<String, Option<i64>>>,
}

impl Agent {
    pub async fn pi_child(&self) -> Result<Arc<RpcChild>> {
        self.pi().await
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Relay blob mirror slot for a chat (`PUT/GET /blob/{slot}/{key}`).
    /// People chats use `people:{chat_id}`; project chats use this project's id.
    pub fn blob_slot(&self, chat_id: &str) -> String {
        if self.people.is_people_chat(chat_id) {
            format!("people:{chat_id}")
        } else {
            self.cfg.project_id.clone()
        }
    }

    pub fn new(
        cfg: Arc<Config>,
        store: Store,
        sync: SyncHub,
        box_slots: Arc<Semaphore>,
    ) -> Arc<Self> {
        let cfg_for_settings = cfg.clone();
        let llm = Arc::new(Llm::new(&cfg));
        let memory = Arc::new(Memory::new(cfg.clone(), store.clone(), llm.clone()));
        let tree = Arc::new(crate::tree::Tree::new(&cfg, store.clone(), llm.clone()));
        let mut adapters: HashMap<Harness, Arc<dyn HarnessAdapter>> = HashMap::new();
        adapters.insert(
            Harness::Prime,
            Arc::new(PrimeAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::Pi,
            Arc::new(PiAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::ClaudeCode,
            Arc::new(ClaudeCodeAdapter::new(cfg.clone(), store.clone())),
        );
        Arc::new(Self {
            cfg,
            store,
            sync,
            llm,
            memory,
            tree,
            pi: Mutex::new(None),
            current_chat: Mutex::new(None),
            turn: Arc::new(Mutex::new(None)),
            adapters,
            turn_running: AtomicBool::new(false),
            subagent_turns: std::sync::atomic::AtomicU32::new(0),
            pi_ready: AtomicBool::new(false),
            ext_loaded: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            turn_lock: Mutex::new(()),
            subagent_slots: Arc::new(Semaphore::new(SUBAGENT_MAX_CONCURRENT)),
            box_slots,
            http: reqwest::Client::new(),
            settings: crate::settings::Settings::new(&cfg_for_settings),
            skills: SkillsIndex::new(),
            approvals: crate::approval::Approvals::default(),
            subagent_msgs: Mutex::new(HashMap::new()),
            groups: crate::groups::GroupState::default(),
            people: crate::people::PeopleState::default(),
            turn_images: std::sync::Mutex::new(Vec::new()),
            goals_cache: std::sync::Mutex::new(String::new()),
            prompt_memory: std::sync::Mutex::new(None),
            compaction_floor: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Refresh the active-goals blurb used by the working set.
    pub async fn refresh_goals_cache(self: &Arc<Self>) {
        let text = self.active_goals_text().await;
        if let Ok(mut g) = self.goals_cache.lock() {
            *g = text;
        }
    }

    /// Poll active goals so every chat's working set stays current.
    pub fn spawn_goals_poller(self: &Arc<Self>) {
        let agent = self.clone();
        tokio::spawn(async move {
            loop {
                agent.refresh_goals_cache().await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }

    /// Refetch the skills index from the relay (60 s timer, `skills_rev` change, after a save).
    pub async fn refresh_skills(&self) {
        match self
            .skills
            .refresh(
                &self.http,
                &self.cfg.relay_url,
                &self.cfg.box_token,
                &self.cfg.project_id,
            )
            .await
        {
            Ok(n) => {
                tracing::debug!(skills = n, rev = %self.skills.rev(), "skills index refreshed")
            }
            Err(e) => tracing::debug!(error = %e, "skills index refresh"),
        }
    }

    /// Live model slots (registry/doc writes land here; `cfg` only holds the boot values).
    pub(crate) async fn live_models(&self) -> (String, String) {
        let m = self.settings.models.read().await;
        (m.talking.id.clone(), m.subagent.id.clone())
    }

    pub fn adapter(&self, h: Harness) -> Arc<dyn HarnessAdapter> {
        self.adapters.get(&h).cloned().expect("registered adapter")
    }

    // ---- pi child -----------------------------------------------------------------------------

    fn system_prompt_path(&self) -> std::path::PathBuf {
        self.cfg.state_dir.join("pi").join("system-prompt.md")
    }

    /// Router prompt (byte-stable, first) + project instructions + T0/T1 (design §6.9 layout).
    pub fn write_system_prompt(&self) -> Result<()> {
        let mut s = String::from(include_str!("../prompts/router.md"));
        if self.cfg.code_mode() {
            s.push_str("\n\n");
            s.push_str(CODE_MODE_SECTION);
        }
        let talking = self
            .settings
            .models
            .try_read()
            .map(|m| m.talking.id.clone())
            .unwrap_or_else(|_| self.cfg.talking_model.clone());
        // One box, many workspaces (D70): the project's files live in its own folder, not /work.
        s.push_str(&format!(
            "\n\n## This project\n\nProject \"{}\". Its folder is `{}`: every file you create or read for this project goes there (relative paths resolve there; do not use `/work` itself, other folders under `/work` belong to other projects). You run on model `{}`.\n",
            self.cfg.project_name,
            self.cfg.work_dir.display(),
            talking
        ));
        s.push('\n');
        s.push_str(&crate::media::prompt_section(&self.cfg));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str("\n\n## Project instructions\n\n");
            s.push_str(self.cfg.instructions.trim());
            s.push('\n');
        }
        // D87 stable prefix: project memory changes after most memory passes; inside the system
        // prompt every change rewrites the whole cached prefix on the next new chat. Measured on the
        // eval set: moving it into every turn cost more than the rewrites (Haiku writes cost 12.5×
        // reads), so with the flag on the block stays here but is refreshed at most every
        // `ARBOS_MEMORY_PROMPT_REFRESH_SECS` (default 30 min): chats opened in between share the prefix.
        // Off: rewritten at every memory change, as before.
        let core = self.memory_for_prompt();
        if !core.trim().is_empty() {
            s.push_str("\n\n## Project memory\n\n");
            s.push_str(&core);
        }
        std::fs::create_dir_all(self.system_prompt_path().parent().unwrap())?;
        std::fs::write(self.system_prompt_path(), s)?;
        Ok(())
    }

    /// The project-memory block of the system prompt. Stable prefix on: the live text replaces the
    /// remembered one only when it changed and the remembered one is older than the refresh window.
    fn memory_for_prompt(&self) -> String {
        let live = self.memory.core_memory_text();
        if !self.cfg.stable_prefix() {
            return live;
        }
        let refresh = Duration::from_secs(
            std::env::var("ARBOS_MEMORY_PROMPT_REFRESH_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1800),
        );
        let mut held = self.prompt_memory.lock().expect("prompt memory");
        match held.as_ref() {
            Some((text, at)) if *text == live || at.elapsed() < refresh => text.clone(),
            _ => {
                *held = Some((live.clone(), Instant::now()));
                live
            }
        }
    }

    /// Deterministic pi session file for a chat: `<sessions>/<ts>_<chatId>.jsonl` (one session per chat).
    fn session_file_for(&self, chat_id: &str) -> Result<std::path::PathBuf> {
        if let Some(p) = self.store.chat(chat_id)?.and_then(|c| c.pi_session_path) {
            if std::path::Path::new(&p).exists() {
                return Ok(p.into());
            }
        }
        let dir = self.cfg.pi_sessions_dir();
        std::fs::create_dir_all(&dir)?;
        if let Some(existing) = crate::harness::find_session_file(&dir, chat_id) {
            self.store
                .set_chat_session_path(chat_id, &existing.display().to_string())?;
            return Ok(existing);
        }
        self.new_session_file(chat_id)
    }

    /// A brand-new pi session file for `chat_id`, recorded as the chat's session. The previous
    /// file (if any) stays on disk: T3 and the memory tree already hold its history.
    fn new_session_file(&self, chat_id: &str) -> Result<std::path::PathBuf> {
        let dir = self.cfg.pi_sessions_dir();
        std::fs::create_dir_all(&dir)?;
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
        let path = dir.join(format!("{ts}_{chat_id}.jsonl"));
        let header = json!({ "type": "session", "version": 3, "id": chat_id, "timestamp": chrono::Utc::now().to_rfc3339(), "cwd": self.cfg.work_dir });
        std::fs::write(&path, format!("{header}\n"))?;
        // pi appends to this file as the workspace user; agentd (root) created it.
        if let Some(ra) = &self.cfg.run_as {
            let _ = std::os::unix::fs::chown(&path, Some(ra.uid), Some(ra.gid));
        }
        self.store
            .set_chat_session_path(chat_id, &path.display().to_string())?;
        Ok(path)
    }

    pub async fn start_pi(self: &Arc<Self>, initial_chat: &str) -> Result<()> {
        self.write_system_prompt()?;
        let session = self.session_file_for(initial_chat)?;
        let (talking_model, _) = self.live_models().await;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        std::fs::create_dir_all(log.parent().unwrap())?;
        std::fs::create_dir_all(&self.cfg.work_dir)?;
        let t0 = Instant::now();
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, initial_chat, "main"),
            &self.cfg.work_dir,
            &log,
            self.cfg.run_as.as_ref(),
        )
        .await?;
        let state = child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        if !state.success {
            return Err(anyhow!("pi get_state failed: {:?}", state.error));
        }
        tracing::info!(ms = t0.elapsed().as_millis() as i64, "pi main agent ready");
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(initial_chat.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        self.store.upsert_session(
            initial_chat,
            None,
            Some(&session.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &talking_model,
            "pi",
            "main",
        )?;
        let _ = self
            .post_timing("pi_boot_ms", t0.elapsed().as_millis() as i64, None)
            .await;
        // The extension writes an `extension_loaded` audit line on load.
        if let Ok(s) = std::fs::read_to_string(self.cfg.audit_path()) {
            if s.contains("\"extension_loaded\"") {
                self.ext_loaded.store(true, Ordering::SeqCst);
            }
        }
        // Supervise: respawn on exit (sandbox-image.md §3.3).
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let alive = me
                    .pi
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.is_alive())
                    .unwrap_or(false);
                if me.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                if !alive && !me.turn_running.load(Ordering::SeqCst) {
                    me.pi_ready.store(false, Ordering::SeqCst);
                    let chat = me.current_chat.lock().await.clone().unwrap_or_default();
                    tracing::warn!("pi main child is gone; respawning");
                    if let Err(e) = me.respawn_pi(&chat).await {
                        tracing::error!(error = %e, "pi respawn failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
        Ok(())
    }

    /// Respawn the main pi child on the chat it was last on (empty → a fresh
    /// session file). Used when an in-session `set_model` is refused so the
    /// next turn boots with `live_models()`.
    pub(crate) async fn respawn_pi_current(self: &Arc<Self>) -> Result<()> {
        let chat = self.current_chat.lock().await.clone().unwrap_or_default();
        self.respawn_pi(&chat).await
    }

    async fn respawn_pi(self: &Arc<Self>, chat_id: &str) -> Result<()> {
        if let Some(old) = self.pi.lock().await.take() {
            old.shutdown().await;
        }
        // The code-mode flag is read again here (D86): toggling it needs a pi respawn, not a box restart.
        self.write_system_prompt()?;
        let session = self.session_file_for(chat_id)?;
        let (talking_model, _) = self.live_models().await;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, chat_id, "main"),
            &self.cfg.work_dir,
            &log,
            self.cfg.run_as.as_ref(),
        )
        .await?;
        child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Wait briefly for the supervisor to respawn pi (it polls every 2 s). A send that
    /// lands between death and respawn must not permanently Fail with no assistant bubble.
    async fn pi(&self) -> Result<Arc<RpcChild>> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(c) = self.pi.lock().await.clone() {
                return Ok(c);
            }
            if self.shutting_down.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return Err(anyhow!("pi main agent not running"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn ensure_chat_session(&self, chat_id: &str) -> Result<()> {
        if self.current_chat.lock().await.as_deref() == Some(chat_id) {
            return Ok(());
        }
        let path = self.session_file_for(chat_id)?;
        self.switch_session_to(chat_id, &path).await
    }

    /// Replace the chat's model window: a fresh pi session file, switched to at once. Used when
    /// the current window is stuck — a provider refusal bound to its content, or a turn that
    /// settled with no route and no prose (the same session keeps failing the same way).
    async fn rotate_chat_session(self: &Arc<Self>, chat_id: &str, reason: &str) -> Result<()> {
        let previous = self
            .store
            .chat(chat_id)?
            .and_then(|c| c.pi_session_path)
            .unwrap_or_default();
        let path = self.new_session_file(chat_id)?;
        *self.current_chat.lock().await = None;
        self.switch_session_to(chat_id, &path).await?;
        // A fresh window holds none of the chat: every earlier turn is now the tree's to offer (D91).
        let _ = self.tree.note_session_cut(chat_id, 0);
        tracing::warn!(chat_id, reason, previous = %previous, fresh = %path.display(), "fresh pi session after empty turn");
        self.append_audit(
            "session_rotated",
            json!({ "chat_id": chat_id, "reason": reason, "previous": previous, "fresh": path.display().to_string() }),
        );
        let _ = self.store.t3_append(
            chat_id,
            None,
            Some(chat_id),
            "compaction",
            Some("system"),
            &format!("pi session replaced after {reason} (history stays in the memory tree)"),
            None,
        );
        Ok(())
    }

    async fn switch_session_to(&self, chat_id: &str, path: &std::path::Path) -> Result<()> {
        let pi = self.pi().await?;
        let t0 = Instant::now();
        pi.call_ok(RpcCommand::SwitchSession {
            id: pi.next_id("sw"),
            session_path: path.display().to_string(),
        })
        .await?;
        tracing::info!(
            chat_id,
            ms = t0.elapsed().as_millis() as i64,
            "switched pi session"
        );
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        let (talking_model, _) = self.live_models().await;
        self.store.upsert_session(
            chat_id,
            None,
            Some(&path.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &talking_model,
            "pi",
            "main",
        )?;
        Ok(())
    }

    // ---- commands -----------------------------------------------------------------------------

    /// Mark-before-execute, then `done`/`failed` in the doc. The result is also returned for
    /// callers that report it elsewhere (routine runs, channel replies); the command loop ignores it.
    pub async fn handle_command(self: &Arc<Self>, chat_id: &str, cmd: CommandView) -> Result<()> {
        let Some(host) = self.sync.host(chat_id).await else {
            return Err(anyhow!("no room for chat {chat_id}"));
        };
        // People chats route by mention (or, in a group, the decision model): a message not
        // for this agent is left pending for whoever it is for; humans' own sends are theirs.
        // Shared addresses ("hey agents", several @agents) stay `taken` until each addressed
        // agent has replied — see `people_shared_send_open` / sync `new_pending`.
        let people_msg_id = if cmd.kind == "send" && self.people.is_people_chat(chat_id) {
            let msg_id = cmd
                .payload
                .get("messageId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if !self.people_wants(chat_id, &host, &msg_id).await {
                return Ok(());
            }
            if self.people_already_replied(&host, &msg_id).await {
                return Ok(());
            }
            Some(msg_id)
        } else {
            None
        };
        {
            host.lock()
                .await
                .doc
                .set_command_status(&cmd.id, CommandStatus::Taken, None);
        }
        let res = match cmd.kind.as_str() {
            "send" => self.handle_send(chat_id, &host, &cmd).await,
            "stop" => self.handle_stop(chat_id).await,
            // Guide: inject into the running turn (no turn_lock). Same as stop — mid-task.
            "steer" | "guide" => self.handle_steer(chat_id, &host, &cmd).await,
            "answer" => self.handle_send(chat_id, &host, &cmd).await,
            "retitle" => {
                let title = cmd
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                self.retitle(chat_id, &title).await
            }
            "retry" => self.handle_retry(chat_id, &host, &cmd).await,
            "approve" | "reject" => {
                let approval_id = cmd
                    .payload
                    .get("approvalId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let outcome = if cmd.kind == "approve" {
                    ApprovalOutcome::Approved
                } else {
                    ApprovalOutcome::Rejected {
                        reason: cmd
                            .payload
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|r| !r.is_empty())
                            .map(str::to_owned),
                    }
                };
                self.decide_approval(&approval_id, outcome, &cmd.issued_by)
                    .await
            }
            other => Err(anyhow!("unknown command kind {other}")),
        };
        let leave_taken = match (&res, people_msg_id.as_deref()) {
            (Ok(()), Some(msg_id)) => self.people_shared_send_open(chat_id, &host, msg_id).await,
            _ => false,
        };
        let mut h = host.lock().await;
        match &res {
            Ok(()) => {
                if !leave_taken {
                    h.doc.set_command_status(&cmd.id, CommandStatus::Done, None);
                }
            }
            Err(e) => {
                tracing::warn!(chat_id, cmd = %cmd.kind, error = %e, "command failed");
                h.doc.set_command_status(
                    &cmd.id,
                    CommandStatus::Failed,
                    Some(json!({ "error": e.to_string() })),
                );
                // iOS never renders command status. A send that failed before any assistant
                // row would leave the user message alone forever — surface an error with Retry.
                if matches!(cmd.kind.as_str(), "send" | "answer" | "steer" | "guide") {
                    let user_msg = cmd
                        .payload
                        .get("messageId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !user_msg.is_empty() {
                        let has_reply = h.doc.messages().iter().any(|m| {
                            m.role == "assistant"
                                && m.rest.get("inReplyTo").and_then(Value::as_str) == Some(user_msg)
                                && m.status != "cancelled"
                        });
                        // A failed steer still has the live turn's assistant row; only surface a
                        // new error bubble when nothing is answering this user message.
                        if !has_reply {
                            let id = ulid();
                            h.doc
                                .start_assistant_message(&id, user_msg, &self.cfg.device_id);
                            h.doc.add_status_part(&id, "failed", "Failed");
                            h.doc.add_error_part(&id, "send_failed", &e.to_string());
                            h.doc.finish_message(&id, MessageStatus::Error);
                        }
                    }
                }
            }
        }
        res
    }

    /// Abort the turn running in `chat_id` (people chat removal, voice-call cancel).
    pub(crate) async fn stop_turn(&self, chat_id: &str) -> Result<()> {
        self.handle_stop(chat_id).await
    }

    /// Take `turn_lock`. For typed (non-voice) sends: while another turn holds the lock, abort
    /// an in-flight voice-call turn for this chat so the owner is not stuck after hanging up.
    async fn acquire_turn_lock(
        &self,
        chat_id: &str,
        this_is_voice: bool,
    ) -> tokio::sync::MutexGuard<'_, ()> {
        if this_is_voice {
            return self.turn_lock.lock().await;
        }
        loop {
            let should_stop = self
                .turn
                .lock()
                .await
                .as_ref()
                .is_some_and(|t| t.chat_id == chat_id && t.from_voice && !t.stopped);
            if should_stop {
                tracing::info!(chat_id, "preempting voice turn so a typed send can run");
                let _ = self.handle_stop(chat_id).await;
            }
            if let Ok(g) = self.turn_lock.try_lock() {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Re-run the turn for a failed assistant message (`retry { messageId }`). The payload is the
    /// assistant row the UI shows Retry on; we find its user message (`inReplyTo`) and `send` again.
    async fn handle_retry(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Result<()> {
        let failed_id = cmd
            .payload
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if failed_id.is_empty() {
            return Err(anyhow!("retry: missing messageId"));
        }
        let user_msg_id = {
            let mut h = host.lock().await;
            let msgs = h.doc.messages();
            let failed = msgs.iter().find(|m| m.id == failed_id);
            let user_id = failed
                .and_then(|m| m.rest.get("inReplyTo").and_then(Value::as_str))
                .map(str::to_owned)
                .or_else(|| {
                    let ix = msgs.iter().position(|m| m.id == failed_id)?;
                    msgs[..ix]
                        .iter()
                        .rev()
                        .find(|m| m.role == "user")
                        .map(|m| m.id.clone())
                })
                .ok_or_else(|| anyhow!("retry: no user message for {failed_id}"))?;
            if failed.is_some_and(|m| m.role == "assistant") {
                h.doc.finish_message(&failed_id, MessageStatus::Cancelled);
            }
            user_id
        };
        let send = CommandView {
            id: cmd.id.clone(),
            kind: "send".into(),
            payload: json!({ "messageId": user_msg_id }),
            issued_by: cmd.issued_by.clone(),
            issued_at: cmd.issued_at,
            status: cmd.status.clone(),
        };
        self.handle_send(chat_id, host, &send).await
    }

    /// `stop` runs out of band (its own task, no turn lock): abort the pi turn now, mark the turn
    /// stopped so no hand-off follows, and tell the chat.
    async fn handle_stop(&self, chat_id: &str) -> Result<()> {
        let target = {
            let mut tl = self.turn.lock().await;
            match tl.as_mut() {
                Some(t) if t.chat_id == chat_id => {
                    t.stopped = true;
                    t.escalate = false;
                    Some(t.assistant_msg_id.clone())
                }
                _ => None,
            }
        };
        let Some(msg_id) = target else {
            tracing::info!(chat_id, "stop: no turn running for this chat");
            return Ok(());
        };
        if let Ok(pi) = self.pi().await {
            let _ = pi
                .call_timeout(
                    RpcCommand::Abort {
                        id: pi.next_id("ab"),
                    },
                    Duration::from_secs(5),
                )
                .await;
        }
        if let Some(host) = self.sync.host(chat_id).await {
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&msg_id, "stopped", "Stopped by the user");
        }
        tracing::info!(chat_id, "stop: pi turn aborted");
        Ok(())
    }

    /// `steer` / `guide` runs out of band (no turn lock): inject the user's words into the
    /// currently running pi turn via `RpcCommand::Steer`. When nothing is running, fall back to
    /// a normal `send` so a late Guide still lands.
    /// This agent as the reply-to note names it (own rows vs. another agent's).
    fn reply_me(&self) -> crate::reply::Me<'_> {
        crate::reply::Me {
            name: &self.cfg.project_name,
            project_id: &self.cfg.project_id,
        }
    }

    async fn handle_steer(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Result<()> {
        let user_msg_id = cmd
            .payload
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let (user_text, reply_note) = {
            let h = host.lock().await;
            let m = h.doc.message(&user_msg_id);
            let reply_note = m
                .as_ref()
                .and_then(|m| crate::reply::quoted_message_note(&h.doc, m, &self.reply_me()));
            (m.map(|m| m.text()).unwrap_or_default(), reply_note)
        };
        if user_text.trim().is_empty() {
            return Err(anyhow!("steer: message {user_msg_id} not found or empty"));
        }
        let running_here = {
            let tl = self.turn.lock().await;
            matches!(tl.as_ref(), Some(t) if t.chat_id == chat_id)
        };
        if !running_here {
            tracing::info!(chat_id, "steer: no turn running; falling back to send");
            return self.handle_send(chat_id, host, cmd).await;
        }
        let _ = self.store.t3_append(
            chat_id,
            None,
            Some(chat_id),
            "user",
            Some("user"),
            &user_text,
            Some(&json!({ "message_id": user_msg_id, "steer": true })),
        );
        let pi = self.pi().await?;
        let message = match reply_note {
            Some(note) => format!("{note}{user_text}"),
            None => user_text,
        };
        let r = pi
            .call(RpcCommand::Steer {
                id: pi.next_id("s"),
                message,
            })
            .await?;
        if !r.success {
            return Err(anyhow!("steer rejected: {}", r.error.unwrap_or_default()));
        }
        tracing::info!(chat_id, "steer: injected into running turn");
        Ok(())
    }

    /// For an `answer` command with `{ questionId, value }`: `(question, rendered text)`. The
    /// widget comes from the ask row (falling back to the doc part); the value is rendered once,
    /// here, so the model, T3 and the bubble all read the same words. Also flips the part to
    /// `answered` and the ask row to answered.
    async fn resolve_structured_answer(
        &self,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Option<(String, String)> {
        if cmd.kind != "answer" {
            return None;
        }
        let question_id = cmd.payload.get("questionId").and_then(Value::as_str)?;
        let value = cmd.payload.get("value").cloned().unwrap_or(Value::Null);
        let (question, widget_json) = match self.store.ask(question_id) {
            Ok(Some((q, w, _))) => (q, w),
            Ok(None) | Err(_) => {
                let h = host.lock().await;
                let (_, q, w) = h
                    .doc
                    .open_question()
                    .filter(|(id, _, _)| id == question_id)?;
                (q, w)
            }
        };
        let rendered = match Widget::parse(widget_json.as_ref()) {
            Ok(Some(w)) => w.answer_text(&value),
            Ok(None) | Err(_) => {
                let text = cmd
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or(value.to_string())
                    });
                format!("Answer: {text}")
            }
        };
        let _ = self.store.answer_ask(question_id);
        {
            let mut h = host.lock().await;
            h.doc.answer_question_part(question_id, &value);
        }
        Some((question, rendered))
    }

    async fn handle_send(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Result<()> {
        let user_msg_id = cmd
            .payload
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        // Peek before taking the turn lock: a typed send must not wait forever behind a
        // voice-call turn that is still running after the call ended (approval wait, long
        // tool, deferred ask_agent). Preempt that turn so the owner's text is answered.
        let this_is_voice = {
            let h = host.lock().await;
            h.doc
                .message(&user_msg_id)
                .as_ref()
                .is_some_and(message_from_voice)
        };
        let _guard = self.acquire_turn_lock(chat_id, this_is_voice).await;
        // Claim before attachments / tree_pack so hang-up `voice_cancel` and a later typed send
        // can preempt this turn while setup still holds the lock.
        *self.turn.lock().await = Some(TurnState::claim(chat_id, &user_msg_id, this_is_voice));
        // A structured answer (`answer { questionId, value }`): render the value to text with the
        // widget the question was asked with, and mark the part answered before the turn runs.
        let structured = self.resolve_structured_answer(host, cmd).await;
        let (mut user_text, channel_note, message_view, reply_note) = {
            let h = host.lock().await;
            let m = h.doc.message(&user_msg_id);
            let text = m.as_ref().map(|m| m.text()).unwrap_or_default();
            let reply_note = m
                .as_ref()
                .and_then(|m| crate::reply::quoted_message_note(&h.doc, m, &self.reply_me()));
            (
                text,
                m.as_ref().and_then(crate::channels::prompt_note),
                m,
                reply_note,
            )
        };
        if user_text.trim().is_empty() {
            if let Some((_, rendered)) = &structured {
                user_text = rendered.clone();
            }
        }
        // Memory tree (D77): the walk starts as soon as the words are known and runs beside the
        // rest of the setup; `drive_turn` joins it before the prompt goes to pi.
        let mut memory = if user_text.trim().is_empty() {
            PendingMemory::none()
        } else {
            self.spawn_tree_pack(chat_id, &user_msg_id, "reply", &user_text)
        };
        // Attachments (images, voice notes, files): fetched into the workspace; images ride
        // along as vision input, a voice note's transcript becomes the text when none was typed.
        let attachments = match &message_view {
            Some(m) => self.turn_attachments(chat_id, m).await,
            None => crate::attachments::TurnAttachments::default(),
        };
        if user_text.trim().is_empty() {
            if let Some(t) = &attachments.transcript {
                user_text = t.clone();
            }
        }
        if user_text.trim().is_empty() && !attachments.notes.is_empty() {
            user_text = "(attachment)".to_owned();
        }
        if user_text.trim().is_empty() {
            let _ = self.turn.lock().await.take();
            return Err(anyhow!("send: message {user_msg_id} not found or empty"));
        }
        if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
            let _ = self.turn.lock().await.take();
            return Ok(());
        }
        if memory.0.is_none() {
            // the words came from a voice note or an attachment: start the walk now
            memory = self.spawn_tree_pack(chat_id, &user_msg_id, "reply", &user_text);
        }
        let _ = self.store.touch_chat(chat_id);
        let _ = self.store.t3_append(
            chat_id,
            None,
            Some(chat_id),
            "user",
            Some("user"),
            &user_text,
            Some(&json!({ "message_id": user_msg_id })),
        );
        // Title from the first message (design §3.4) when the chat still has the default title.
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            if c.title.is_empty() || c.title == "New chat" {
                let title: String = user_text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(48)
                    .collect();
                let _ = self
                    .retitle(chat_id, title.trim_end_matches(['.', ',', ':', ';']).trim())
                    .await;
            }
        }
        // Answering an open question (S8): mark it answered; pi receives the text as the next prompt.
        let mut prompt_text = user_text.clone();
        if let Some((question, rendered)) = &structured {
            prompt_text = format!("Answer to your question \"{question}\":\n{rendered}");
        } else if let Ok(Some((ask_id, _, question))) = self.store.open_ask(chat_id) {
            let _ = self.store.answer_ask(&ask_id);
            prompt_text = format!("Answer to your question \"{question}\": {user_text}");
        }
        // Reply-to: the quoted message rides with the user's words (user turn, never the
        // system prompt — D87 keeps the prefix byte-stable).
        if let Some(note) = reply_note {
            prompt_text = format!("{note}{prompt_text}");
        }
        // A message that arrived through a channel (F7): the model learns where it came from and
        // that the reply is read as plain chat text.
        if let Some(note) = channel_note {
            prompt_text = format!("{note}\n\n{prompt_text}");
        }
        // Group chats (F6): the decision model picks which agents reply (none / one / some /
        // all); fan-out goes only to the chosen others, and the host skips its turn when not
        // selected.
        let mut skip_host_group_turn = false;
        if let Some(gt) = self.group_turn_context(chat_id, host, &user_msg_id).await {
            if let Some(text) = &gt.fan_out_user_text {
                let responders = self.decide_group_responders(&gt, &user_msg_id, text).await;
                let host_id = self.cfg.project_id.as_str();
                let others: Vec<String> = responders
                    .iter()
                    .filter(|id| id.as_str() != host_id)
                    .cloned()
                    .collect();
                let all_others: Vec<String> = gt
                    .participant_ids
                    .iter()
                    .filter(|id| id.as_str() != host_id)
                    .cloned()
                    .collect();
                let targets = if others.is_empty() {
                    Some(Vec::new())
                } else if others.len() == all_others.len() {
                    None
                } else {
                    Some(others)
                };
                self.spawn_group_fan_out(&gt, text, targets);
                if !responders.iter().any(|id| id == host_id) {
                    skip_host_group_turn = true;
                }
            }
            if !skip_host_group_turn {
                prompt_text = format!("{}{prompt_text}", gt.prelude);
            }
        }
        if skip_host_group_turn {
            let _ = self.turn.lock().await.take();
            self.people.set_current(None);
            return Ok(());
        }
        // People chats (D72): who is asking, in what role; the reply is attributed to them.
        // Otherwise: guests get a label; the owner gets an address-by-display-name note.
        if let Some(prelude) = self.people_turn_context(chat_id, host, &user_msg_id).await {
            prompt_text = format!("{prelude}{prompt_text}");
        } else if let Some(note) = self
            .shared_project_guest_note(chat_id, host, &user_msg_id)
            .await
        {
            prompt_text = format!("{note}{prompt_text}");
        } else if let Some(note) = self.owner_address_note(chat_id, host, &user_msg_id).await {
            prompt_text = format!("{note}{prompt_text}");
        }
        if !attachments.notes.is_empty() {
            let mut extra = attachments.notes.join("\n");
            if let Some(t) = &attachments.transcript {
                if user_text.trim() != t.trim() {
                    extra = format!("{extra}\n[Voice note transcript]: {t}");
                }
            }
            prompt_text = format!("{extra}\n\n{prompt_text}");
        }
        *self.turn_images.lock().expect("turn images") = attachments.images;

        // Hang-up / typed preempt during setup: drop the claim and leave without opening a row.
        if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
            let _ = self.turn.lock().await.take();
            self.people.set_current(None);
            return Ok(());
        }

        let outcome = self
            .drive_turn(
                chat_id,
                host,
                &user_msg_id,
                &prompt_text,
                TurnKind::User,
                memory,
            )
            .await;
        self.people.set_current(None);
        let outcome = outcome?;
        let TurnOutcome {
            assistant_msg_id,
            route_name: _,
            turn,
            result,
        } = outcome;

        // The model acknowledges a hand-off in one sentence (demo review item 2); only when it
        // wrote nothing at all does agentd fill the gap so the chat never shows a bare route.
        if matches!(
            turn.route,
            Some(ValidRoute::New { .. }) | Some(ValidRoute::Wake { .. })
        ) && turn.streamed_text.trim().is_empty()
            && turn.ack_text.trim().is_empty()
        {
            let mut h = host.lock().await;
            if let Some(p) = h.doc.add_text_part(&assistant_msg_id, "text") {
                h.doc.append_text(
                    &assistant_msg_id,
                    &p,
                    "Handing this to a subagent; I will post its result here.",
                );
            }
            tracing::info!(chat_id, "ack_fallback: model wrote no sentence after route");
        }
        // An act turn that hit a hard limit and escalated says so instead of leaving an empty
        // bubble; the subagent that continues it reports back in a later message.
        if matches!(turn.route, Some(ValidRoute::Act { .. }))
            && turn.escalate
            && !turn.stopped
            && turn.streamed_text.trim().is_empty()
        {
            let mut h = host.lock().await;
            if let Some(p) = h.doc.add_text_part(&assistant_msg_id, "text") {
                h.doc.append_text(
                    &assistant_msg_id,
                    &p,
                    "I hit a turn limit; I'm handing the rest to a subagent and will post the result here.",
                );
            }
            tracing::info!(chat_id, "escalation note: act turn ended with no text");
        }

        self.spawn_memory_pass(chat_id, &assistant_msg_id, &user_text, &turn.streamed_text);

        // Hand-offs after the main turn: new / wake / escalation. Never on the chat's critical path.
        if turn.stopped {
            return result;
        }
        let followup = match (&turn.route, turn.escalate) {
            (
                Some(ValidRoute::New {
                    name,
                    purpose,
                    brief,
                    harness,
                    isolate: _,
                    data_heavy,
                }),
                _,
            ) => Some((
                None,
                name.clone(),
                purpose.clone(),
                brief.clone(),
                *harness,
                *data_heavy,
                false,
            )),
            (Some(ValidRoute::Wake { subagent_id, brief }), _) => Some((
                Some(subagent_id.clone()),
                String::new(),
                String::new(),
                brief.clone(),
                None,
                false,
                false,
            )),
            (Some(ValidRoute::Act { plan }), true) => {
                let brief = format!(
                    "Continue this task that the project agent started and could not finish within its limits.\n\nUser request: {user_text}\n\nPlan: {plan}\n\nAct so far:\n{}\n\nFinish the task.",
                    turn.act_transcript.join("\n").chars().take(2400).collect::<String>()
                );
                Some((
                    None,
                    "continue-act".into(),
                    "Finish a task the project agent started".into(),
                    brief,
                    None,
                    false,
                    true,
                ))
            }
            _ => None,
        };
        if let Some((existing, name, purpose, brief, harness, data_heavy, escalated)) = followup {
            self.spawn_subagent_task(
                chat_id,
                host,
                &user_msg_id,
                existing,
                name,
                purpose,
                brief,
                harness,
                data_heavy,
                escalated,
            );
        }
        result
    }

    /// Run the subagent in the background and report a start failure into the chat.
    #[allow(clippy::too_many_arguments)]
    fn spawn_subagent_task(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        existing: Option<String>,
        name: String,
        purpose: String,
        brief: String,
        harness: Option<Harness>,
        data_heavy: bool,
        escalated: bool,
    ) {
        let me = self.clone();
        let chat = chat_id.to_owned();
        let host2 = host.clone();
        let user_msg = user_msg_id.to_owned();
        tokio::spawn(async move {
            if let Err(e) = me
                .run_subagent(
                    &chat,
                    &host2,
                    &user_msg,
                    existing.clone(),
                    &name,
                    &purpose,
                    &brief,
                    harness,
                    data_heavy,
                    escalated,
                )
                .await
            {
                tracing::warn!(error = %e, "subagent run failed");
                let label = if name.is_empty() {
                    existing.clone().unwrap_or_else(|| "subagent".into())
                } else {
                    name.clone()
                };
                // Clear leftover `running`/`queued` and any streaming progress row
                // so a failed start cannot pin the working indicator or working set.
                let sub_id = existing
                    .clone()
                    .or_else(|| {
                        me.store
                            .subagent_by_name(&name)
                            .ok()
                            .flatten()
                            .map(|r| r.id)
                    })
                    .unwrap_or_default();
                if !sub_id.is_empty() {
                    me.subagent_msgs.lock().await.remove(&sub_id);
                    let _ = me.store.set_subagent_status(&sub_id, "idle");
                    let _ = me.store.set_subagent_last_status(&sub_id, "failed");
                    {
                        let mut h = host2.lock().await;
                        let stale: Vec<String> = h
                            .doc
                            .messages()
                            .into_iter()
                            .filter(|m| {
                                m.role == "assistant"
                                    && m.status == "streaming"
                                    && m.rest.get("subagentId").and_then(|v| v.as_str())
                                        == Some(sub_id.as_str())
                            })
                            .map(|m| m.id)
                            .collect();
                        for id in stale {
                            h.doc.add_status_part(&id, "failed", "Failed");
                            h.doc.add_error_part(&id, "subagent_failed", &e.to_string());
                            h.doc.finish_message(&id, MessageStatus::Error);
                            h.doc
                                .set_message_fields(&id, &[("handoffMessages", json!(2))]);
                        }
                    }
                    me.write_threads_index().await;
                }
                {
                    let mut h = host2.lock().await;
                    let id = ulid();
                    h.doc
                        .start_assistant_message(&id, &user_msg, &me.cfg.device_id);
                    h.doc.add_status_part(&id, "failed", "Failed");
                    h.doc.add_error_part(&id, "subagent_failed", &e.to_string());
                    h.doc.finish_message(&id, MessageStatus::Error);
                }
                // The main agent must learn about the failure too, or its next turn still believes
                // the hand-off is running; it also tells the user in prose.
                if let Err(e2) = me
                    .run_result_turn(
                        &chat,
                        &host2,
                        &user_msg,
                        existing.as_deref().unwrap_or(""),
                        &label,
                        "failed",
                        0,
                        "(the subagent did not start)",
                        Some(&e.to_string()),
                    )
                    .await
                {
                    tracing::warn!(error = %e2, "result turn after a failed start");
                }
            }
        });
    }

    /// Memory write pass + compaction, off the reply path (design §6.9).
    fn spawn_memory_pass(self: &Arc<Self>, chat_id: &str, msg_id: &str, ask: &str, reply: &str) {
        let me = self.clone();
        let chat = chat_id.to_owned();
        let text = format!("User: {ask}\nAgent: {reply}");
        let msg = msg_id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if let Err(e) = me
                .memory
                .write_pass(&msg, &chat, "assistant", &text, &[])
                .await
            {
                tracing::debug!(error = %e, "memory write pass");
            }
            if let Err(e) = me.memory.maybe_compact(&chat).await {
                tracing::debug!(error = %e, "compaction");
            }
            let _ = me.write_system_prompt();
            if me.tree_mode() != crate::tree::TreeMode::Off {
                if let Err(e) = me.tree.build_pass().await {
                    tracing::debug!(error = %e, "memory tree build pass");
                }
            }
        });
    }

    /// The tree mode for this Project (`decision_model.memory_tree`, else the box env).
    pub fn tree_mode(&self) -> crate::tree::TreeMode {
        self.settings
            .models
            .try_read()
            .map(|m| m.decision_cfg.memory_tree)
            .unwrap_or(self.cfg.decision_cfg.memory_tree)
    }

    /// Whether `chat_id` sees only its own subtree (people and group chats, D72/D69).
    pub fn tree_restricted(&self, chat_id: &str) -> bool {
        self.people.is_people_chat(chat_id)
            || self
                .groups
                .kind_of(chat_id)
                .map(|k| k != "chat")
                .unwrap_or(false)
    }

    /// Walk the memory tree for one turn or subagent launch; `None` = inject nothing.
    pub async fn tree_pack(
        &self,
        chat_id: &str,
        message_id: Option<&str>,
        purpose: &str,
        query_text: &str,
    ) -> Option<String> {
        let mode = self.tree_mode();
        if mode == crate::tree::TreeMode::Off {
            return None;
        }
        let (scorer, asker) = {
            let m = self.settings.models.read().await;
            (
                m.decision_cfg.scorer.clone(),
                self.people
                    .asker(chat_id)
                    .map(|a| format!("{} ({})", a.spoken(), a.role))
                    .unwrap_or_else(|| "the project owner".to_owned()),
            )
        };
        let recent = self
            .store
            .t3_thread_before(chat_id, None, 12)
            .map(|rows| json!({ "n": rows.len(), "ids": rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>() }))
            .unwrap_or(Value::Null);
        // The pack skips leaves the prompt's project-memory block already states; pass the text
        // the prompt actually holds (with the stable prefix on, up to 30 min old), not the live one.
        let prompt_memory = self.memory_for_prompt();
        self.tree
            .pack_for_turn(
                mode,
                &scorer,
                chat_id,
                message_id,
                purpose,
                self.tree_restricted(chat_id),
                query_text,
                &asker,
                &recent,
                &prompt_memory,
                self.cfg.walk_options(),
            )
            .await
    }

    /// Run `maybe_soft_compact` behind the current turn: the task waits for the turn lock, so it
    /// lands after this turn is finished and before the next prompt.
    fn spawn_soft_compaction(self: &Arc<Self>, chat_id: &str, usage: Option<&Value>) {
        if crate::workspace::compact_at_tokens() == 0 {
            return;
        }
        let me = self.clone();
        let chat = chat_id.to_owned();
        let usage = usage.cloned();
        tokio::spawn(async move {
            let _guard = me.turn_lock.lock().await;
            me.maybe_soft_compact(&chat, usage.as_ref()).await;
        });
    }

    /// Soft compaction (D91). `usage` is the last model call's usage, so its context size is the
    /// window the next turn would start from. Above `compact_at_tokens()` (64k by default) pi is
    /// asked to compact: `keepRecentTokens` of recent turns stay verbatim, the rest becomes one
    /// summary, and the walk gets the compacted-away leaves of this chat as candidates. pi's own
    /// compaction at the 128k hard ceiling is unchanged and covers a single turn that balloons.
    async fn maybe_soft_compact(&self, chat_id: &str, usage: Option<&Value>) {
        let threshold = crate::workspace::compact_at_tokens();
        if threshold == 0 {
            return;
        }
        let tokens = usage.map(context_tokens).unwrap_or(0);
        if tokens <= 0 {
            return;
        }
        // Hysteresis: the first turn after a compaction shows where the window landed (the
        // floor); the next compaction waits until the window has grown a good step past it, so a
        // turn large enough to land back over the threshold on its own cannot rewrite the cache
        // every turn. The 128k ceiling still catches a chat that keeps growing meanwhile.
        {
            let mut floors = self.compaction_floor.lock().expect("compaction floors");
            match floors.get(chat_id).copied() {
                Some(None) => {
                    floors.insert(chat_id.to_owned(), Some(tokens));
                    tracing::debug!(chat_id, tokens, "soft compaction: floor observed");
                    return;
                }
                Some(Some(floor))
                    if tokens < floor + crate::workspace::COMPACTION_HYSTERESIS_TOKENS as i64 =>
                {
                    if tokens >= threshold as i64 {
                        tracing::debug!(chat_id, tokens, floor, "soft compaction deferred: window has not grown enough since the last one");
                    }
                    return;
                }
                _ => {}
            }
        }
        if tokens < threshold as i64 {
            return;
        }
        if self.current_chat.lock().await.as_deref() != Some(chat_id) {
            return;
        }
        let Ok(pi) = self.pi().await else {
            return;
        };
        let t0 = Instant::now();
        let r = pi
            .call_timeout(
                RpcCommand::Compact {
                    id: pi.next_id("c"),
                    custom_instructions: Some(SOFT_COMPACTION_INSTRUCTIONS.to_owned()),
                },
                Duration::from_secs(240),
            )
            .await;
        match r {
            Ok(resp) if resp.success => {
                let n = self.store.bump_session_compactions(chat_id).unwrap_or(0);
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "compaction",
                    Some("system"),
                    &format!(
                        "pi session compacted at {tokens} tokens (soft threshold {threshold})"
                    ),
                    None,
                );
                let cut = self
                    .tree
                    .note_session_cut(chat_id, crate::workspace::KEEP_RECENT_TOKENS as usize);
                self.compaction_floor
                    .lock()
                    .expect("compaction floors")
                    .insert(chat_id.to_owned(), None);
                tracing::info!(chat_id, tokens, threshold, compactions = n, cut = ?cut, ms = t0.elapsed().as_millis() as i64, "soft compaction");
            }
            Ok(resp) => {
                tracing::warn!(chat_id, tokens, error = %resp.error.unwrap_or_default(), "soft compaction refused");
            }
            Err(e) => {
                tracing::warn!(chat_id, tokens, error = %e, "soft compaction failed");
            }
        }
    }

    /// Start the reply's memory walk beside the rest of the turn's setup (attachments, preludes,
    /// the assistant row, the pi session switch); `drive_turn` joins it right before the prompt
    /// goes to pi. The block still rides in front of the user message — in the user turn, never
    /// the system prompt, so the D87 prefix stays byte-stable.
    pub fn spawn_tree_pack(
        self: &Arc<Self>,
        chat_id: &str,
        message_id: &str,
        purpose: &'static str,
        query_text: &str,
    ) -> PendingMemory {
        let me = self.clone();
        let chat_id = chat_id.to_owned();
        let message_id = message_id.to_owned();
        let query_text = query_text.to_owned();
        PendingMemory(Some(tokio::spawn(async move {
            me.tree_pack(&chat_id, Some(&message_id), purpose, &query_text)
                .await
        })))
    }

    /// Periodic builder: new leaves become episodes and cards even when no turn runs.
    pub fn spawn_tree_builder(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            loop {
                if me.tree_mode() != crate::tree::TreeMode::Off {
                    match me.tree.build_pass().await {
                        Ok(stats) => {
                            if stats.get("cards").and_then(Value::as_i64).unwrap_or(0) > 0
                                || stats.get("leaves").and_then(Value::as_i64).unwrap_or(0) > 0
                            {
                                tracing::info!(stats = %stats, "memory tree build pass");
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "memory tree build pass"),
                    }
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        })
    }

    /// One prompt to the main pi session, folded into a new assistant message. The caller holds
    /// `turn_lock`. Finalizes the message (decision, usage, status), T3, and turn metrics.
    ///
    /// The assistant / "Working…" row is created *before* `ensure_chat_session`: a session or
    /// pi failure must still leave a visible error (with Retry), not a silent Failed command.
    async fn drive_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        prompt_text: &str,
        kind: TurnKind,
        memory: PendingMemory,
    ) -> Result<TurnOutcome> {
        // Early claim may already be stopped (hang-up during attachments / tree_pack).
        if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
            let _ = self.turn.lock().await.take();
            return Err(anyhow!("turn stopped before prompt"));
        }
        let assistant_msg_id = ulid();
        let text_part_id = {
            let mut h = host.lock().await;
            let pid =
                h.doc
                    .start_assistant_message(&assistant_msg_id, user_msg_id, &self.cfg.device_id);
            match &kind {
                TurnKind::User => {
                    h.doc
                        .add_status_part(&assistant_msg_id, "working", "Working…");
                    if let Some(attr) = self.people_reply_fields(chat_id) {
                        h.doc.set_message_fields(&assistant_msg_id, &attr.fields);
                    }
                }
                TurnKind::SubagentResult { subagent_id, name } => {
                    h.doc
                        .add_status_part(&assistant_msg_id, "working", "Working…");
                    h.doc.set_message_fields(
                        &assistant_msg_id,
                        &[
                            ("subagentId", json!(subagent_id)),
                            ("subagentName", json!(name)),
                        ],
                    );
                }
                TurnKind::ApprovalOutcome { approval_id, title } => {
                    h.doc.add_status_part(
                        &assistant_msg_id,
                        "working",
                        &format!("Following up on “{title}”…"),
                    );
                    h.doc.set_message_fields(
                        &assistant_msg_id,
                        &[("approvalId", json!(approval_id))],
                    );
                }
            }
            pid
        };

        if let Err(e) = self.ensure_chat_session(chat_id).await {
            {
                let mut h = host.lock().await;
                h.doc.add_status_part(&assistant_msg_id, "failed", "Failed");
                h.doc
                    .add_error_part(&assistant_msg_id, "session", &e.to_string());
                h.doc
                    .finish_message(&assistant_msg_id, MessageStatus::Error);
            }
            let _ = self.turn.lock().await.take();
            return Err(e);
        }

        // Stop raced with session switch: settle the Working row and leave.
        if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
            {
                let mut h = host.lock().await;
                h.doc
                    .add_status_part(&assistant_msg_id, "stopped", "Stopped by the user");
                h.doc
                    .finish_message(&assistant_msg_id, MessageStatus::Cancelled);
            }
            let _ = self.turn.lock().await.take();
            return Err(anyhow!("turn stopped before prompt"));
        }
        let from_voice = {
            let h = host.lock().await;
            h.doc
                .message(user_msg_id)
                .as_ref()
                .is_some_and(message_from_voice)
        };
        // Memory tree (D77): the walk ran beside the setup above; the chosen raw records ride in
        // front of the message. Nothing is added when the tree is not built, the walk timed out
        // or found nothing, or the daily cap is reached.
        let prompt_text = match memory.block().await {
            Some(block) => format!("{block}\n\n{prompt_text}"),
            None => prompt_text.to_owned(),
        };
        let prompt_text = prompt_text.as_str();
        *self.turn.lock().await = Some(TurnState {
            chat_id: chat_id.to_owned(),
            user_msg_id: user_msg_id.to_owned(),
            assistant_msg_id: assistant_msg_id.clone(),
            text_part_id: text_part_id.clone(),
            started: Instant::now(),
            route: None,
            route_attempts: 0,
            route_failed: false,
            escalate: false,
            asked: None,
            metrics: TurnMetrics::default(),
            streamed_text: String::new(),
            act_transcript: Vec::new(),
            tool_inputs: HashMap::new(),
            reply_extractor: ReplyExtractor::default(),
            ack_extractor: ReplyExtractor::for_key("ack"),
            ack_text: String::new(),
            ack_split: false,
            row_closed: false,
            progress_texts: Vec::new(),
            activity: None,
            prose: ProseFilter::default(),
            act_buffer: String::new(),
            route_call_id: None,
            text_part_dirty: false,
            stopped: false,
            from_voice,
            prompt_text: match &kind {
                TurnKind::User => prompt_text.to_owned(),
                TurnKind::SubagentResult { .. } | TurnKind::ApprovalOutcome { .. } => String::new(),
            },
            skills_used: Vec::new(),
            model_error: None,
            session_rotated: false,
        });
        self.turn_running.store(true, Ordering::SeqCst);
        let keepalive = self.spawn_keepalive();
        let mut result = self.run_main_turn(chat_id, host, prompt_text).await;
        // An empty turn is usually the model window, not the question: a provider refusal on a
        // bloated session (Lium, 2026-10-21: six turns, 208–242k tokens, zero output) or a settle
        // with no `route` and no prose (mr.agi group + DM, feedback app-01M5GCK…: consecutive
        // "didn't answer" until the session recovered). History lives in the memory tree, so
        // rebuild the window and ask once more before failing visibly.
        if result.is_ok() {
            if let Some(reason) = self.empty_turn_retry_reason().await {
                match self.rotate_chat_session(chat_id, reason).await {
                    Ok(()) => {
                        if let Some(t) = self.turn.lock().await.as_mut() {
                            t.model_error = None;
                            t.session_rotated = true;
                            // Drop a synthetic reply from the miss so the retry can route for real.
                            if t.metrics.route_missing {
                                t.route = None;
                            }
                        }
                        result = self.run_main_turn(chat_id, host, prompt_text).await;
                    }
                    Err(e) => {
                        tracing::warn!(chat_id, reason, error = %e, "fresh session after empty turn")
                    }
                }
            }
        }
        self.turn_running.store(false, Ordering::SeqCst);
        keepalive.abort();

        // Whatever the filter still holds back at the end of the turn is judged and written
        // as one complete bubble (no token stream).
        {
            let mut tl = self.turn.lock().await;
            if let Some(t) = tl.as_mut() {
                self.flush_staged_text(host, t).await;
            }
        }
        let turn = self.turn.lock().await.take();
        let Some(turn) = turn else {
            return Err(anyhow!("turn state vanished mid-turn"));
        };
        // After an ack or progress bubble the live row is a later one than the row this turn
        // opened with; everything below finalizes the row the turn ended on.
        let assistant_msg_id = turn.assistant_msg_id.clone();
        let total_ms = turn.started.elapsed().as_millis() as i64;
        let route_name = turn
            .route
            .as_ref()
            .map(route_name)
            .unwrap_or(if turn.route_failed { "reply" } else { "none" });
        let turn_kind = match &kind {
            TurnKind::User => route_name,
            TurnKind::SubagentResult { .. } => "subagent_result",
            TurnKind::ApprovalOutcome { .. } => "approval_outcome",
        };
        let (mut final_status, err_text) = match &result {
            Ok(()) => (MessageStatus::Done, None),
            Err(e) => (MessageStatus::Error, Some(e.to_string())),
        };
        // The model call failed and the user got no answer: say so in the bubble, never a blank
        // row (feedback lium-empty-reply-2026-10-21). A partial answer before the failure stays.
        let model_failed = turn
            .model_error
            .as_ref()
            .filter(|_| err_text.is_none() && !turn.stopped && turn.streamed_text.trim().is_empty())
            .cloned();
        // Still empty after any fresh-session retry: route missing (possibly recorded as a
        // synthetic reply), no prose, no ack (mr.agi group + DM drops). Mark error so the
        // client shows a line instead of a bare header.
        let silent_no_route = model_failed.is_none()
            && err_text.is_none()
            && !turn.stopped
            && turn.metrics.route_missing
            && turn.streamed_text.trim().is_empty()
            && turn.ack_text.trim().is_empty();
        if model_failed.is_some() || silent_no_route {
            final_status = MessageStatus::Error;
        }
        let (talking_model, _) = self.live_models().await;
        let room_name;
        {
            let mut h = host.lock().await;
            if let Some(e) = &err_text {
                h.doc.add_error_part(&assistant_msg_id, "turn_failed", e);
            }
            if let Some(m) = &model_failed {
                h.doc
                    .add_error_part(&assistant_msg_id, m.code(), &m.chat_line());
                h.doc.add_hidden_part(
                    &assistant_msg_id,
                    "model_error_detail",
                    &m.detail,
                    &json!({ "refusal": m.refusal, "sessionRotated": turn.session_rotated }),
                );
            }
            if silent_no_route {
                h.doc.add_error_part(
                    &assistant_msg_id,
                    "route_missing",
                    "I couldn't answer that. Ask me again in a moment.",
                );
                h.doc.add_hidden_part(
                    &assistant_msg_id,
                    "route_missing_detail",
                    "turn ended without a route call",
                    &json!({ "sessionRotated": turn.session_rotated, "routeMissing": turn.metrics.route_missing }),
                );
            }
            if turn.activity.is_some() {
                h.doc.set_activity(&assistant_msg_id, None);
            }
            h.doc.set_message_fields(
                &assistant_msg_id,
                &[
                    ("decision", json!(route_name)),
                    ("turn", json!({ "kind": turn_kind })),
                    ("model", json!(talking_model)),
                    ("usage", turn.metrics.usage_for_doc()),
                ],
            );
            if turn.ack_split && !turn.row_closed {
                h.doc
                    .set_message_fields(&assistant_msg_id, &[("turnRole", json!("result"))]);
            }
            h.doc.finish_message(&assistant_msg_id, final_status);
            room_name = h.room.clone();
        }
        if turn.activity.is_some() {
            self.post_activity(chat_id, None);
        }
        if let Some(m) = &model_failed {
            tracing::warn!(chat_id, refusal = m.refusal, session_rotated = turn.session_rotated, detail = %m.detail, "model call failed; error line shown in chat");
            self.append_audit(
                "model_call_failed",
                json!({
                    "chat_id": chat_id,
                    "message_id": assistant_msg_id,
                    "refusal": m.refusal,
                    "session_rotated": turn.session_rotated,
                    "detail": m.detail.chars().take(400).collect::<String>(),
                }),
            );
        }
        if silent_no_route {
            tracing::warn!(
                chat_id,
                session_rotated = turn.session_rotated,
                route_missing = turn.metrics.route_missing,
                "turn ended without route; error line shown in chat"
            );
            self.append_audit(
                "turn_without_route",
                json!({
                    "chat_id": chat_id,
                    "message_id": assistant_msg_id,
                    "session_rotated": turn.session_rotated,
                    "route_missing": turn.metrics.route_missing,
                }),
            );
        }
        // One notification per turn: the result, or the ack when the turn ended on it.
        let announce = if turn.streamed_text.trim().is_empty() {
            turn.ack_text.as_str()
        } else {
            turn.streamed_text.as_str()
        };
        if matches!(final_status, MessageStatus::Done) && !announce.trim().is_empty() {
            self.announce_reply(&room_name, &assistant_msg_id, announce);
        }
        if !turn.ack_text.trim().is_empty() {
            let _ = self.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                "assistant",
                Some("assistant"),
                turn.ack_text.trim(),
                Some(&json!({ "ack": true, "route": route_name, "turn": turn_kind })),
            );
        }
        if !turn.streamed_text.trim().is_empty() {
            let _ = self.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                "assistant",
                Some("assistant"),
                &turn.streamed_text,
                Some(&json!({ "message_id": assistant_msg_id, "route": route_name, "turn": turn_kind })),
            );
        }
        let mut usage_rec = turn.metrics.usage_for_doc();
        if usage_rec.is_null() {
            usage_rec = json!({});
        }
        if turn.metrics.route_missing {
            usage_rec["route_missing"] = json!(true);
        }
        if turn.metrics.route_corrected {
            usage_rec["route_corrected"] = json!(true);
        }
        let _ = self.store.record_turn(
            chat_id,
            user_msg_id,
            Some(turn_kind),
            turn.metrics.ttft_ms,
            turn.metrics.route_ms,
            total_ms,
            turn.metrics.tool_calls,
            turn.escalate,
            Some(&usage_rec),
        );
        if let Some(ttft) = turn.metrics.ttft_ms {
            let _ = self
                .post_timing("main_turn_ttft_ms", ttft, Some(turn_kind))
                .await;
        }
        let _ = self
            .post_timing("main_turn_total_ms", total_ms, Some(turn_kind))
            .await;
        tracing::info!(chat_id, route = route_name, turn = turn_kind, ttft_ms = ?turn.metrics.ttft_ms, route_ms = ?turn.metrics.route_ms, total_ms, tool_calls = turn.metrics.tool_calls, nested_calls = turn.metrics.nested_calls, model_calls = turn.metrics.model_calls, "turn done");
        // D91: the reply is out; if the window grew past the soft threshold, compact it behind
        // this turn (the task takes the turn lock, so the next prompt meets the small window and
        // this turn's own completion is not held up).
        self.spawn_soft_compaction(chat_id, turn.metrics.usage.as_ref());
        Ok(TurnOutcome {
            assistant_msg_id,
            route_name,
            turn,
            result,
        })
    }

    /// A finished subagent never speaks to the user directly: its raw output stays hidden and the
    /// main agent writes the update in prose, as a follow-up turn in the same chat (demo review
    /// item 1). Waits for `turn_lock`, so it lands after whatever the user is doing right now.
    #[allow(clippy::too_many_arguments)]
    async fn run_result_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
        subagent_id: &str,
        name: &str,
        status: &str,
        total_ms: i64,
        raw: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let ask = {
            let h = host.lock().await;
            h.doc
                .message(user_msg_id)
                .map(|m| m.text())
                .unwrap_or_default()
        };
        let recent = self
            .store
            .t3_thread(chat_id, None, None, 12)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.kind == "user" || r.kind == "assistant")
            .map(|r| {
                format!(
                    "- {}: {}",
                    r.role.unwrap_or_default(),
                    r.text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(240)
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw_trim: String = raw.chars().take(7000).collect();
        let mut prompt = format!(
            "[Arbos] Subagent \"{name}\" finished · status {status} · {:.0} s.\n\nThe user originally asked: {}\n",
            total_ms as f64 / 1000.0,
            if ask.trim().is_empty() { "(see the brief)" } else { ask.trim() },
        );
        if !recent.is_empty() {
            prompt.push_str(&format!("\nRecent turns in this chat:\n{recent}\n"));
        }
        if let Some(e) = error {
            prompt.push_str(&format!("\nThe run ended with an error: {e}\n"));
        }
        prompt.push_str(&format!(
            "\nSubagent output (for you only; the user has not seen it):\n<<<\n{raw_trim}\n>>>\n\nRoute `reply` and write the update for the user like a colleague in chat: what got done and the outcome, what changed that they care about, and what is open or needs their answer. Speak as if you did the work; do not mention the subagent, its name, the harness, the model, tool calls, commands, scratch paths, or token counts. No JSON, no quoted output, no list of steps. Two to five sentences."
        ));
        let kind = TurnKind::SubagentResult {
            subagent_id: subagent_id.to_owned(),
            name: name.to_owned(),
        };
        let outcome = self
            .drive_turn(
                chat_id,
                host,
                user_msg_id,
                &prompt,
                kind,
                PendingMemory::none(),
            )
            .await?;
        // A result turn relays; it does not start further work. Any other route is logged only.
        if !matches!(outcome.turn.route, Some(ValidRoute::Reply { .. }) | None) {
            tracing::warn!(
                route = outcome.route_name,
                "result turn routed away from reply; ignored"
            );
        }
        self.spawn_memory_pass(
            chat_id,
            &outcome.assistant_msg_id,
            &format!("[subagent {name} finished: {status}]"),
            &outcome.turn.streamed_text,
        );
        outcome.result
    }

    /// A `[Arbos]` notice to the main agent as its own turn in `chat_id` (approval outcomes).
    /// Waits for `turn_lock`, so it lands after whatever the user is doing right now.
    pub async fn run_notice_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        in_reply_to: &str,
        prompt: &str,
        kind: TurnKind,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let outcome = self
            .drive_turn(
                chat_id,
                host,
                in_reply_to,
                prompt,
                kind,
                PendingMemory::none(),
            )
            .await?;
        self.spawn_memory_pass(
            chat_id,
            &outcome.assistant_msg_id,
            "[approval outcome]",
            &outcome.turn.streamed_text,
        );
        outcome.result
    }

    fn spawn_keepalive(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let _ = me.keepalive(true).await;
            let mut t = tokio::time::interval(Duration::from_secs(5 * 60));
            t.tick().await;
            loop {
                t.tick().await;
                let _ = me.keepalive(true).await;
            }
        })
    }

    /// Rename a chat: registry (`PATCH /projects/{id}/chats/{chat}`), local index, and the project
    /// doc `chats` list on the next poll.
    pub async fn retitle(&self, chat_id: &str, title: &str) -> Result<()> {
        if title.is_empty() {
            return Err(anyhow!("retitle: empty title"));
        }
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            let _ = self
                .store
                .upsert_chat(chat_id, title, c.is_pinned, c.created_at);
        }
        let r = self
            .http
            .patch(format!(
                "{}/projects/{}/chats/{}",
                self.cfg.relay_url, self.cfg.project_id, chat_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "title": title }))
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(anyhow!("retitle: relay HTTP {}", r.status()));
        }
        Ok(())
    }

    pub async fn keepalive(&self, turn_running: bool) -> Result<()> {
        self.http
            .post(format!("{}/box/keepalive", self.cfg.relay_url))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "turn_running": turn_running }))
            .send()
            .await?;
        Ok(())
    }

    /// Tell the relay a whole reply landed, so people who are not looking get a notification
    /// (`POST /chats/{id}/notify` for a people chat, `POST /projects/{p}/chats/{c}/notify`
    /// otherwise). Fire-and-forget: the message is in the doc either way.
    pub fn announce_reply(self: &Arc<Self>, room: &str, message_id: &str, text: &str) {
        let Some((project, chat_id)) = room.split_once('/') else {
            return;
        };
        let url = if project == crate::people::PEOPLE_PROJECT {
            format!("{}/chats/{chat_id}/notify", self.cfg.relay_url)
        } else {
            format!(
                "{}/projects/{}/chats/{chat_id}/notify",
                self.cfg.relay_url, self.cfg.project_id
            )
        };
        let body = json!({
            "message_id": message_id,
            "text": text.chars().take(600).collect::<String>(),
            "project_id": self.cfg.project_id,
        });
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me
                .http
                .post(url)
                .bearer_auth(&me.cfg.box_token)
                .json(&body)
                .send()
                .await
            {
                tracing::debug!(error = %e, "notify relay of reply failed");
            }
        });
    }

    pub async fn post_timing(&self, kind: &str, ms: i64, detail: Option<&str>) -> Result<()> {
        self.http
            .post(format!(
                "{}/projects/{}/timings",
                self.cfg.relay_url, self.cfg.project_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "kind": kind, "ms": ms, "detail": detail }))
            .send()
            .await?;
        Ok(())
    }

    /// Upload a blob to the relay mirror, fire-and-forget. Returns the key.
    pub fn upload_blob(self: &Arc<Self>, bytes: Vec<u8>, mime: &str) -> String {
        use sha2::Digest;
        let key = hex::encode(sha2::Sha256::digest(&bytes));
        let me = self.clone();
        let mime = mime.to_owned();
        let k = key.clone();
        tokio::spawn(async move {
            let _ = me
                .http
                .put(format!(
                    "{}/blob/{}/{}",
                    me.cfg.relay_url, me.cfg.project_id, k
                ))
                .bearer_auth(&me.cfg.box_token)
                .header("content-type", mime)
                .body(bytes)
                .send()
                .await;
        });
        key
    }

    // ---- the main turn ------------------------------------------------------------------------

    async fn run_main_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        prompt_text: &str,
    ) -> Result<()> {
        let pi = self.pi().await?;
        let mut events = pi.subscribe();
        let images = std::mem::take(&mut *self.turn_images.lock().expect("turn images"));
        let r = pi
            .call(RpcCommand::Prompt {
                id: pi.next_id("p"),
                message: prompt_text.to_owned(),
                streaming_behavior: None,
                images,
            })
            .await?;
        if !r.success {
            return Err(anyhow!("prompt rejected: {}", r.error.unwrap_or_default()));
        }
        let started = Instant::now();
        let mut deadline_announced = false;
        let deadline = Duration::from_secs(turn_deadline_secs());
        // Time the turn spends blocked on an approval card is the user's, not the agent's: it
        // does not count toward the turn deadline.
        let mut approval_paused = Duration::ZERO;
        let mut approval_pause_started: Option<Instant> = None;
        loop {
            let ev = tokio::select! {
                e = events.recv() => e,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // `stop` / a typed send preempting a voice turn: leave the approval wait
                    // so the turn lock is released and later sends can run.
                    if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
                        break;
                    }
                    let waiting = self.approval_pending_for(chat_id).await;
                    match (waiting, approval_pause_started) {
                        (true, None) => approval_pause_started = Some(Instant::now()),
                        (false, Some(t0)) => {
                            approval_paused += t0.elapsed();
                            approval_pause_started = None;
                        }
                        (true, Some(_)) | (false, None) => {}
                    }
                    if waiting {
                        continue;
                    }
                    let elapsed = started.elapsed().saturating_sub(approval_paused);
                    // Turn deadline: announce in chat, hand remaining work to a subagent, never silent.
                    if elapsed > deadline && !deadline_announced {
                        deadline_announced = true;
                        if let Some(t) = self.turn.lock().await.as_mut() {
                            t.escalate = true;
                        }
                        self.post_limit_notice(
                            host,
                            "I hit the turn time limit; I'm handing the rest to a subagent.",
                        )
                        .await;
                        let _ = pi
                            .call(RpcCommand::Abort {
                                id: pi.next_id("ab"),
                            })
                            .await;
                    }
                    continue;
                }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "lagged behind pi events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(anyhow!("pi event stream closed"))
                }
            };
            // Do not wait for the 1s timer: under a busy event stream `stop` / preempt must
            // release the turn lock without relying on the select timeout arm.
            if self.turn.lock().await.as_ref().is_some_and(|t| t.stopped) {
                break;
            }
            if self.fold_main_event(chat_id, host, &ev, started).await? {
                break;
            }
            if matches!(ev, Event::Other) && !pi.is_alive() {
                return Err(anyhow!("pi main child exited mid-turn"));
            }
        }
        Ok(())
    }

    /// Fold one pi event into the chat doc and the turn state. Returns true when the turn settled.
    async fn fold_main_event(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        ev: &Event,
        started: Instant,
    ) -> Result<bool> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Ok(true);
        };
        let elapsed = started.elapsed().as_millis() as i64;
        match ev {
            Event::MessageUpdate {
                assistant_message_event,
                usage,
            } => {
                if let Some(u) = usage {
                    if u.total_tokens > 0 {
                        t.metrics.usage = Some(
                            json!({ "input": u.input, "output": u.output, "cacheRead": u.cache_read, "cacheWrite": u.cache_write, "cost": u.cost }),
                        );
                    }
                }
                match assistant_message_event {
                    AssistantEvent::TextDelta { delta, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        self.append_text(host, t, delta).await;
                    }
                    AssistantEvent::ThinkingDelta { .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                    }
                    AssistantEvent::ToolcallStart { id, tool_name, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        if tool_name == "route" {
                            t.route_call_id = Some(id.clone());
                            t.reply_extractor = ReplyExtractor::default();
                        }
                    }
                    AssistantEvent::ToolcallDelta {
                        delta,
                        content_index: _,
                    } => {
                        if t.route_call_id.is_some() && t.route.is_none() {
                            // The ack buffers first (schema order) and becomes its own bubble;
                            // a short `reply` lands as one complete block when the string closes.
                            let ack = t.ack_extractor.feed(delta);
                            if !ack.is_empty() {
                                if t.metrics.ttft_ms.is_none() {
                                    t.metrics.ttft_ms = Some(elapsed);
                                }
                                self.write_ack(host, t, &ack).await;
                            }
                            self.maybe_split_ack(host, t, false).await;
                            let text = t.reply_extractor.feed(delta);
                            if !text.is_empty() {
                                self.append_text(host, t, &text).await;
                            }
                            if t.reply_extractor.is_done() {
                                self.flush_staged_text(host, t).await;
                            }
                        }
                    }
                    AssistantEvent::ToolcallEnd { .. }
                    | AssistantEvent::Start
                    | AssistantEvent::TextStart { .. }
                    | AssistantEvent::TextEnd { .. }
                    | AssistantEvent::ThinkingStart { .. }
                    | AssistantEvent::ThinkingEnd { .. }
                    | AssistantEvent::Done { .. }
                    | AssistantEvent::Unknown => {}
                    AssistantEvent::Error { error, .. } => {
                        // `error` is the failed assistant message itself (pi-ai shape; an
                        // `aborted` stop is ours and stays quiet); older builds sent a string.
                        let m = match error {
                            Some(v) if v.get("stopReason").is_some() => ModelError::from_message(v),
                            Some(v) => Some(ModelError {
                                detail: v.to_string(),
                                refusal: false,
                            }),
                            None => Some(ModelError {
                                detail: "provider error".into(),
                                refusal: false,
                            }),
                        };
                        if let Some(m) = m {
                            self.note_model_error(host, t, m).await;
                        }
                    }
                }
            }
            Event::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => {
                // Talking to the user is not a tool step: no tool part, no count, no activity.
                if tool_name == "route" || tool_name == "send_message" {
                    return Ok(false);
                }
                // Agent-flow §1: never start real work on a bare "Working…" row. If route
                // omitted `ack`, or tools ran before route posted one, put "On it." up now.
                let needs_ack = !t.ack_split
                    && t.ack_text.trim().is_empty()
                    && !matches!(t.route, Some(ValidRoute::Reply { .. }));
                if needs_ack {
                    self.write_ack(host, t, "On it.").await;
                }
                self.maybe_split_ack(host, t, true).await;
                self.set_activity(host, t, crate::activity::label(tool_name, args))
                    .await;
                t.tool_inputs.insert(tool_call_id.clone(), args.clone());
                // A call a `codemode` script made (D86) is logged exactly like a direct call —
                // T3, the hidden tool part, the audit line the extension writes — but counted apart.
                let nested = parent_tool_call_id.is_some();
                if nested {
                    t.metrics.nested_calls += 1;
                } else {
                    t.metrics.tool_calls += 1;
                }
                let one_line = args.to_string().chars().take(120).collect::<String>();
                let arrow = if nested { "  ↳" } else { "→" };
                t.act_transcript
                    .push(format!("{arrow} {tool_name} {one_line}"));
                // No `step` event in the doc (one working indicator only); the hidden tool part
                // and T3 keep the detail for the drawer.
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "tool_call",
                    Some("assistant"),
                    &step_label(tool_name, args),
                    Some(&json!({ "call_id": tool_call_id, "input": args, "parent_call_id": parent_tool_call_id })),
                );
                let held = t.prose.flush();
                if !held.is_empty() {
                    self.stage_text(host, t, &held).await;
                }
                let interim = std::mem::take(&mut t.act_buffer);
                if !interim.trim().is_empty() {
                    let _ = self.store.t3_append(
                        chat_id,
                        None,
                        Some(chat_id),
                        "interim",
                        Some("assistant"),
                        interim.trim(),
                        Some(&json!({ "before_call": tool_call_id })),
                    );
                }
                t.text_part_dirty = true;
            }
            Event::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                parent_tool_call_id,
                ..
            } => {
                if tool_name == "route" {
                    if t.metrics.route_ms.is_none() {
                        t.metrics.route_ms = Some(elapsed);
                    }
                    // The route is validated (hook) by now: an ack that streamed becomes its
                    // own bubble, and an act turn gets its result row.
                    self.maybe_split_ack(host, t, true).await;
                    return Ok(false);
                }
                if tool_name == "send_message" {
                    return Ok(false);
                }
                let output = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                // D19 at write time: the doc copy gets the same redaction as T3.
                let output = crate::store::redact(&output);
                t.act_transcript.push(format!(
                    "← {tool_name}: {}",
                    output.chars().take(300).collect::<String>()
                ));
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "tool_output",
                    Some("tool"),
                    &format!("{tool_name}: {output}"),
                    Some(
                        &json!({ "call_id": tool_call_id, "parent_call_id": parent_tool_call_id }),
                    ),
                );
                let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                    Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                } else {
                    None
                };
                let input = t.tool_inputs.remove(tool_call_id).unwrap_or(json!({}));
                {
                    let mut h = host.lock().await;
                    h.doc.add_tool_part(
                        &t.assistant_msg_id,
                        tool_call_id,
                        tool_name,
                        &input,
                        &summary,
                        output_ref.as_deref(),
                        Some(output.len() as i64),
                        *is_error,
                    );
                }
                t.text_part_dirty = true;
                if *is_error {
                    // A script that hit a sandbox limit is announced in the chat (D86: limits are
                    // never silent); an ordinary script error stays quiet, the model handles it.
                    let limit = if tool_name == "codemode" {
                        classify_script_limit(&output)
                    } else {
                        classify_hard_limit(&output)
                    };
                    if let Some((notice, escalate)) = limit {
                        if escalate {
                            t.escalate = true;
                        }
                        self.write_text(host, t, notice).await;
                    }
                }
            }
            Event::MessageEnd { message } => {
                if message.get("role").and_then(Value::as_str) == Some("assistant") {
                    t.metrics.add_model_call(message.get("usage"));
                    // pi 1.2 never streams an `error` event: a refusal, a context overflow or a
                    // provider 5xx arrive only here, as `stopReason: error` on the final message.
                    if let Some(m) = ModelError::from_message(message) {
                        self.note_model_error(host, t, m).await;
                    }
                }
            }
            Event::AgentSettled { .. } => return Ok(true),
            Event::CompactionEnd { .. } => {
                let _ = self.store.bump_session_compactions(chat_id);
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "compaction",
                    Some("system"),
                    "pi session compacted (pi threshold or overflow)",
                    None,
                );
                let _ = self
                    .tree
                    .note_session_cut(chat_id, crate::workspace::KEEP_RECENT_TOKENS as usize);
                self.compaction_floor
                    .lock()
                    .expect("compaction floors")
                    .insert(chat_id.to_owned(), None);
            }
            Event::Other
            | Event::AgentStart
            | Event::AgentEnd { .. }
            | Event::TurnStart
            | Event::TurnEnd { .. }
            | Event::MessageStart { .. }
            | Event::ToolExecutionUpdate { .. }
            | Event::QueueUpdate { .. }
            | Event::EntryAppended { .. }
            | Event::CompactionStart { .. }
            | Event::ExtensionUiRequest { .. } => {}
        }
        Ok(false)
    }

    /// Main-agent prose enters the doc through the boilerplate filter.
    async fn append_text(&self, host: &SharedHost, t: &mut TurnState, delta: &str) {
        let out = t.prose.feed(delta);
        if !out.is_empty() {
            // The model is talking now, not running a tool.
            self.set_activity(host, t, None).await;
            self.stage_text(host, t, &out).await;
        }
    }

    /// Buffer `route.ack` until it is complete, then strip thought-aloud and write one clean
    /// sentence (agent-flow contract §1). Streaming raw deltas would flash planning text.
    async fn write_ack(&self, _host: &SharedHost, t: &mut TurnState, delta: &str) {
        t.ack_text.push_str(delta);
    }

    /// Once the ack is complete and the action is known: `act` → the ack row closes and a
    /// result row opens; `new`/`wake` → the ack row closes and the turn has nothing more to
    /// show; `reply` → one row, as before. `force` treats a still-open ack string as complete
    /// (the arguments have finished streaming by the time a tool runs).
    async fn maybe_split_ack(&self, host: &SharedHost, t: &mut TurnState, force: bool) {
        if t.ack_split || t.ack_text.trim().is_empty() {
            return;
        }
        if !t.ack_extractor.is_done() && !force {
            return;
        }
        let action: Option<String> = match &t.route {
            Some(r) => Some(route_name(r).to_owned()),
            None => t.ack_extractor.action_seen().map(str::to_owned),
        };
        let action = match (action, force) {
            (Some(a), _) => a,
            (None, true) => "act".to_owned(),
            (None, false) => return,
        };
        // Drop planning / routing thoughts and trailing tool-narration; keep only the
        // user-facing sentence(s), with glued ".Word" spacing fixed.
        let cleaned = ProseFilter::clean_prose(t.ack_text.trim());
        let mut cleaned = cleaned.trim().to_owned();
        // act/new/wake must still show one bubble: a thought-aloud-only ack used to leave
        // the chat on bare "Working…" until the result (feedback AMus6 / ALYEPIee).
        if cleaned.is_empty() && action != "reply" {
            cleaned = "On it.".to_owned();
            tracing::info!(chat_id = %t.chat_id, action, "ack bubble fallback after thought-aloud strip");
        }
        if cleaned.is_empty() {
            t.ack_text.clear();
            t.ack_split = true;
            tracing::info!(chat_id = %t.chat_id, action, "ack bubble dropped (thought-aloud only)");
            return;
        }
        t.ack_text = cleaned;
        if t.row_closed {
            self.open_row(host, t, "ack").await;
        }
        {
            let mut h = host.lock().await;
            h.doc
                .append_text(&t.assistant_msg_id, &t.text_part_id, &t.ack_text);
        }
        t.ack_split = true;
        match action.as_str() {
            "reply" => {}
            "act" => {
                self.close_row(host, t, "ack").await;
                self.open_row(host, t, "result").await;
            }
            _ => self.close_row(host, t, "ack").await,
        }
        tracing::info!(chat_id = %t.chat_id, action, ack_chars = t.ack_text.chars().count(), "ack bubble");
    }

    /// Open a fresh streaming assistant row for this turn (after an ack or a progress bubble).
    async fn open_row(&self, host: &SharedHost, t: &mut TurnState, role: &str) {
        let id = ulid();
        let mut h = host.lock().await;
        let pid = h
            .doc
            .start_assistant_message(&id, &t.user_msg_id, &self.cfg.device_id);
        h.doc.add_status_part(&id, "working", "Working…");
        h.doc.set_message_fields(&id, &[("turnRole", json!(role))]);
        if let Some(attr) = self.people_reply_fields(&t.chat_id) {
            h.doc.set_message_fields(&id, &attr.fields);
        }
        if let Some(a) = &t.activity {
            h.doc.set_activity(&id, Some(a));
        }
        t.assistant_msg_id = id;
        t.text_part_id = pid;
        t.text_part_dirty = false;
        t.row_closed = false;
    }

    /// Close the live row as a finished prose bubble (ack or progress).
    async fn close_row(&self, host: &SharedHost, t: &mut TurnState, role: &str) {
        let mut h = host.lock().await;
        if t.activity.is_some() {
            h.doc.set_activity(&t.assistant_msg_id, None);
        }
        h.doc
            .set_message_fields(&t.assistant_msg_id, &[("turnRole", json!(role))]);
        h.doc
            .finish_message(&t.assistant_msg_id, MessageStatus::Done);
        t.row_closed = true;
    }

    /// The working line's text (agent-flow contract §2): on the live row and mirrored to the relay.
    async fn set_activity(&self, host: &SharedHost, t: &mut TurnState, activity: Option<String>) {
        if t.activity == activity {
            return;
        }
        t.activity = activity.clone();
        if !t.row_closed {
            let mut h = host.lock().await;
            h.doc.set_activity(&t.assistant_msg_id, activity.as_deref());
        }
        self.post_activity(&t.chat_id, activity);
    }

    /// `POST /projects/{id}/activity` so lists and sidebars can show it without the chat doc.
    /// Fire-and-forget: the doc field is the source of truth.
    pub fn post_activity(&self, chat_id: &str, activity: Option<String>) {
        let url = format!(
            "{}/projects/{}/activity",
            self.cfg.relay_url, self.cfg.project_id
        );
        let body = json!({ "chat_id": chat_id, "text": activity });
        let http = self.http.clone();
        let token = self.cfg.box_token.clone();
        tokio::spawn(async move {
            if let Err(e) = http
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                tracing::debug!(error = %e, "post activity to relay failed");
            }
        });
    }

    /// `send_message(text, activity?)` from the main agent: a progress bubble now, as its own
    /// row, while the turn keeps running (agent-flow contract §1).
    pub async fn hook_send_message(
        &self,
        text: &str,
        activity: Option<&str>,
    ) -> Result<(), String> {
        let text = ProseFilter::clean_prose(text.trim());
        let text = text.trim();
        if text.is_empty() && activity.is_none() {
            return Err("nothing to send".into());
        }
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Err("no turn is running".into());
        };
        let Some(host) = self.sync.host(&t.chat_id).await else {
            return Err("no room for this chat".into());
        };
        if !text.is_empty() {
            self.maybe_split_ack(&host, t, true).await;
            // Narration the act turn held back is not the message: it stays in T3 only.
            let mut interim = std::mem::take(&mut t.act_buffer);
            interim.push_str(&t.prose.flush());
            if !interim.trim().is_empty() {
                let _ = self.store.t3_append(
                    &t.chat_id,
                    None,
                    Some(&t.chat_id),
                    "interim",
                    Some("assistant"),
                    interim.trim(),
                    Some(&json!({ "before": "send_message" })),
                );
            }
            if t.row_closed {
                self.open_row(&host, t, "progress").await;
            }
            {
                let mut h = host.lock().await;
                if t.text_part_dirty {
                    if let Some(p) = h.doc.add_text_part(&t.assistant_msg_id, "text") {
                        t.text_part_id = p;
                    }
                    t.text_part_dirty = false;
                }
                h.doc
                    .append_text(&t.assistant_msg_id, &t.text_part_id, text);
            }
            t.progress_texts.push(text.to_owned());
            self.close_row(&host, t, "progress").await;
            self.open_row(&host, t, "result").await;
            let _ = self.store.t3_append(
                &t.chat_id,
                None,
                Some(&t.chat_id),
                "assistant",
                Some("assistant"),
                text,
                Some(&json!({ "message_id": t.assistant_msg_id, "progress": true })),
            );
            tracing::info!(chat_id = %t.chat_id, chars = text.chars().count(), "progress bubble");
        }
        if let Some(a) = activity {
            self.set_activity(&host, t, Some(a.to_owned())).await;
        }
        Ok(())
    }

    /// Hold prose until the bubble is complete so the chat never streams token-by-token
    /// (feedback AFyd_3zYZ2uaRwPk9_7_rMs). Ack and `send_message` already land as whole
    /// bubbles; act turns already buffered mid-tools — reply turns do the same now, and
    /// only what follows the last tool call is the answer on an act turn.
    async fn stage_text(&self, _host: &SharedHost, t: &mut TurnState, text: &str) {
        t.act_buffer.push_str(text);
    }

    /// Judge held prose and write the staged buffer as one complete bubble.
    async fn flush_staged_text(&self, host: &SharedHost, t: &mut TurnState) {
        let held = t.prose.flush();
        if !held.is_empty() {
            self.stage_text(host, t, &held).await;
        }
        let final_text = std::mem::take(&mut t.act_buffer);
        let cleaned = ProseFilter::clean_prose(final_text.trim());
        if !cleaned.is_empty() {
            self.write_text(host, t, &cleaned).await;
        }
    }

    async fn write_text(&self, host: &SharedHost, t: &mut TurnState, delta: &str) {
        if t.row_closed {
            // The turn said it was over (ack only) but the model kept talking: give it a row.
            self.open_row(host, t, "result").await;
        }
        let mut h = host.lock().await;
        if t.text_part_dirty {
            // A tool part landed since the last text: open a new text part so order is kept.
            if let Some(p) = h.doc.add_text_part(&t.assistant_msg_id, "text") {
                t.text_part_id = p;
            }
            t.text_part_dirty = false;
        }
        h.doc
            .append_text(&t.assistant_msg_id, &t.text_part_id, delta);
        t.streamed_text.push_str(delta);
    }

    // ---- hooks called by arbos-pi-ext ---------------------------------------------------------

    /// `/hook/route`: validate; one retry; a second failure is treated as `reply` (design §6.2).
    ///
    /// For `act` / `new` / `wake`, the ack bubble must land here — not only from streamed
    /// tool-arg deltas. Many providers deliver the full `route` arguments only when the
    /// tool runs, so `ToolcallDelta` never feeds `ack_extractor`; without this, the chat
    /// shows a bare "Working…" line until the result (agent-flow contract §1).
    pub async fn hook_route(&self, args: &Value) -> Result<(), String> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Err("no turn is running".into());
        };
        t.route_attempts += 1;
        match validate(args) {
            Ok(r) => {
                let action = route_name(&r);
                tracing::info!(route = action, attempt = t.route_attempts, "route accepted");
                let _ = self.store.t3_append(
                    &t.chat_id,
                    None,
                    Some(&t.chat_id),
                    "route",
                    Some("system"),
                    &format!("route {action}: {args}"),
                    None,
                );
                t.route = Some(r);
                if action != "reply" {
                    if let Some(host) = self.sync.host(&t.chat_id).await {
                        if t.ack_text.trim().is_empty() {
                            let ack = args
                                .get("ack")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .unwrap_or("On it.");
                            self.write_ack(&host, t, ack).await;
                        }
                        self.maybe_split_ack(&host, t, true).await;
                    }
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, attempt = t.route_attempts, "route invalid");
                if t.route_attempts >= 2 {
                    t.route_failed = true;
                    t.route = Some(ValidRoute::Reply { reply: None });
                }
                Err(e)
            }
        }
    }

    /// `ask_user(question, widget?)` from any harness seam. Writes the `question` part (with the
    /// typed widget when one was given) plus the legacy `waiting_user` status part, and records
    /// the ask so the `answer` command can render the value for the model. Returns the question id.
    ///
    /// When the turn is in a side thread but the prompt came from another agent (group /
    /// Projects ⇄), the card is posted on the pinned main chat so the owner can find it.
    pub async fn hook_ask_user(
        &self,
        question: &str,
        widget: Option<&Value>,
        session_id: &str,
    ) -> Result<String> {
        let widget = match Widget::parse(widget) {
            Ok(w) => w,
            Err(e) => {
                // An invalid widget degrades to a plain question rather than losing the turn.
                tracing::warn!(error = %e, "ask_user widget rejected; asking as plain text");
                None
            }
        };
        let widget_json = widget.as_ref().and_then(|w| serde_json::to_value(w).ok());
        let mut tl = self.turn.lock().await;
        let turn_chat = tl
            .as_ref()
            .map(|t| t.chat_id.clone())
            .unwrap_or_else(|| session_id.to_owned());
        let pinned = self
            .store
            .chats()
            .ok()
            .and_then(|cs| cs.into_iter().find(|c| c.is_pinned).map(|c| c.id));
        // Other-agent / system-tab turns: surface the ask on Main, not a buried side thread.
        let from_other_agent = if let Some(t) = tl.as_ref() {
            let in_projects = self
                .groups
                .kind_of(&t.chat_id)
                .map(|k| k != "chat")
                .unwrap_or(false);
            let cross = if let Some(host) = self.sync.host(&t.chat_id).await {
                let h = host.lock().await;
                h.doc
                    .message(&t.user_msg_id)
                    .map(|m| {
                        m.rest.get("groupChatId").is_some()
                            || m.rest.get("hostProjectId").is_some()
                            || m.rest.get("groupTitle").is_some()
                    })
                    .unwrap_or(false)
            } else {
                false
            };
            in_projects || cross
        } else {
            false
        };
        let chat_id = if from_other_agent {
            pinned.unwrap_or_else(|| turn_chat.clone())
        } else {
            turn_chat.clone()
        };
        let id = ulid();
        self.store.insert_ask(
            &id,
            Some(&chat_id),
            Some(session_id),
            question,
            widget_json.as_ref(),
        )?;
        if let Some(t) = tl.as_mut() {
            t.asked = Some(question.to_owned());
            if chat_id == t.chat_id {
                if let Some(host) = self.sync.host(&t.chat_id).await {
                    let mut h = host.lock().await;
                    h.doc.add_question_part(
                        &t.assistant_msg_id,
                        &id,
                        question,
                        widget_json.as_ref(),
                    );
                    h.doc
                        .add_status_part(&t.assistant_msg_id, "waiting_user", question);
                    h.doc
                        .set_message_fields(&t.assistant_msg_id, &[("waitingUser", json!(true))]);
                }
            } else if let Some(host) = self.sync.host(&chat_id).await {
                // Standalone ask on the main chat; the side-thread turn just stops.
                let mut h = host.lock().await;
                let last_user = h
                    .doc
                    .messages()
                    .iter()
                    .rev()
                    .find(|m| m.role == "user")
                    .map(|m| m.id.clone())
                    .unwrap_or_default();
                let msg_id = ulid();
                h.doc
                    .start_assistant_message(&msg_id, &last_user, &self.cfg.device_id);
                h.doc
                    .set_message_fields(&msg_id, &[("turn", json!({ "kind": "ask_user" }))]);
                h.doc
                    .add_question_part(&msg_id, &id, question, widget_json.as_ref());
                h.doc.add_status_part(&msg_id, "waiting_user", question);
                h.doc
                    .set_message_fields(&msg_id, &[("waitingUser", json!(true))]);
                h.doc.finish_message(&msg_id, MessageStatus::Done);
            }
        }
        let _ = self.store.t3_append(
            &chat_id,
            None,
            Some(session_id),
            "ask_user",
            Some("assistant"),
            question,
            Some(&json!({
                "question_id": id,
                "widget": widget_json,
                "posted_chat": chat_id,
            })),
        );
        Ok(id)
    }

    /// Record a failed model call on the turn. Hard limits (#1037) are announced at once in the
    /// chat; everything else is judged when the turn ends — a refusal may still be retried in a
    /// fresh session, and a partial answer before the failure needs no extra line.
    async fn note_model_error(&self, host: &SharedHost, t: &mut TurnState, m: ModelError) {
        if t.model_error.is_some() {
            return;
        }
        tracing::warn!(chat_id = %t.chat_id, refusal = m.refusal, detail = %m.detail, "model call ended in error");
        if !m.refusal {
            if let Some((notice, escalate)) = classify_hard_limit(&m.detail) {
                if escalate {
                    t.escalate = true;
                }
                // Visible chat line: never stop silently on a hard limit.
                self.write_text(host, t, notice).await;
            }
        }
        t.model_error = Some(m);
    }

    /// When the turn showed the user nothing, return why a fresh-session retry is warranted
    /// (`model_refused` or `route_missing`). `None` when a retry would not help or already ran.
    async fn empty_turn_retry_reason(&self) -> Option<&'static str> {
        let tl = self.turn.lock().await;
        let t = tl.as_ref()?;
        if t.stopped || t.session_rotated {
            return None;
        }
        if !t.streamed_text.trim().is_empty() || !t.ack_text.trim().is_empty() {
            return None;
        }
        if t.model_error.as_ref().is_some_and(|m| m.refusal) {
            return Some("model_refused");
        }
        // Settled without `route` and without prose: pi cannot correct when `canContinue` is
        // false (ended on plain text / empty), so agentd rebuilds the window once.
        if t.metrics.route_missing {
            return Some("route_missing");
        }
        None
    }

    /// `/hook/turn-without-route`: the main run settled without calling `route`. When pi could
    /// not continue (`corrected=false`), record the turn as `reply` so any prose already
    /// streamed is a normal answer (arbos-pi-ext route guard). An empty miss is retried once
    /// via `empty_turn_retry_reason` after the run returns.
    pub async fn hook_turn_without_route(&self, session_id: &str, corrected: bool) {
        tracing::warn!(session = %session_id, corrected, "turn ended without a route call");
        let _ = self.store.t3_append(
            session_id,
            None,
            Some(session_id),
            "route",
            Some("system"),
            &format!("turn without route; corrective continuation={corrected}"),
            None,
        );
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return;
        };
        t.metrics.route_missing = true;
        // No further corrective prompt: treat the turn as a reply (harness contract).
        if !corrected && t.route.is_none() {
            t.route = Some(ValidRoute::Reply { reply: None });
            tracing::info!(session = %session_id, "route missing without correction; recorded as reply");
        }
    }

    /// Post a one- or two-sentence hard-limit notice into the live chat row (never silent).
    async fn post_limit_notice(&self, host: &SharedHost, text: &str) {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return;
        };
        self.maybe_split_ack(host, t, true).await;
        let interim = std::mem::take(&mut t.act_buffer);
        if !interim.trim().is_empty() {
            let _ = self.store.t3_append(
                &t.chat_id,
                None,
                Some(&t.chat_id),
                "interim",
                Some("assistant"),
                interim.trim(),
                Some(&json!({ "before": "limit_notice" })),
            );
        }
        self.write_text(host, t, text).await;
    }

    /// Mark subagents whose work ended without cleanup as finished: store status
    /// `running`/`queued` with no live task, and close any leftover streaming
    /// progress rows so clients stop showing "is working".
    ///
    /// `on_boot`: every leftover row is an orphan (no tasks survived the restart).
    /// Otherwise only rows older than the freshness window are reclaimed, so a
    /// subagent waiting on a slot (queued, not yet in `subagent_msgs`) is safe.
    pub async fn reclaim_orphan_subagents(&self, on_boot: bool) {
        /// Same 15-minute window as the Mac/iOS working indicator: a status left
        /// `running` after the task died must not linger forever.
        const ORPHAN_FRESH_MS: i64 = 15 * 60 * 1000;
        let live: HashSet<String> = self.subagent_msgs.lock().await.keys().cloned().collect();
        let now = now_ms();
        let mut healed: Vec<(String, Option<String>)> = Vec::new();
        for s in self.store.subagents().unwrap_or_default() {
            if s.status != "running" && s.status != "queued" {
                continue;
            }
            if live.contains(&s.id) {
                continue;
            }
            if !on_boot {
                let last = s.last_seen_at.or(s.last_turn_at).unwrap_or(s.created_at);
                // Queued waiters that just enrolled are not in `subagent_msgs` yet;
                // only reclaim after the freshness window so a brief race is safe.
                if now.saturating_sub(last) < ORPHAN_FRESH_MS {
                    continue;
                }
            }
            let _ = self.store.set_subagent_status(&s.id, "idle");
            let _ = self.store.set_subagent_last_status(&s.id, "interrupted");
            healed.push((s.id, s.chat_id));
        }
        for (sub_id, chat_id) in &healed {
            let Some(chat_id) = chat_id.as_deref() else {
                continue;
            };
            let Some(host) = self.sync.host(chat_id).await else {
                continue;
            };
            let mut h = host.lock().await;
            let stale: Vec<String> = h
                .doc
                .messages()
                .into_iter()
                .filter(|m| {
                    m.role == "assistant"
                        && m.status == "streaming"
                        && m.rest.get("subagentId").and_then(|v| v.as_str())
                            == Some(sub_id.as_str())
                })
                .map(|m| m.id)
                .collect();
            for id in stale {
                h.doc.add_status_part(&id, "stopped", "Stopped");
                h.doc.finish_message(&id, MessageStatus::Cancelled);
                h.doc
                    .set_message_fields(&id, &[("handoffMessages", json!(2))]);
            }
        }
        if !healed.is_empty() {
            tracing::info!(
                count = healed.len(),
                "reclaimed orphan subagents left running after their work ended"
            );
            self.write_threads_index().await;
        }
    }

    /// Working-set section (design §6.6 item 2b): running subagents + compaction summary +
    /// handles + notes, ≤ 40 lines.
    pub fn working_set_text(&self, chat_id: &str) -> String {
        let mut lines = Vec::new();
        // Prefer the live task map when available so a zombie `running` row
        // (task gone, status never cleared) does not keep the agent thinking
        // work is still in flight. Queued waiters are not in the map yet.
        let live = self
            .subagent_msgs
            .try_lock()
            .ok()
            .map(|m| m.keys().cloned().collect::<HashSet<_>>());
        let running: Vec<_> = self
            .store
            .subagents()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                if s.status == "queued" {
                    return true;
                }
                if s.status != "running" {
                    return false;
                }
                match &live {
                    Some(ids) => ids.contains(&s.id),
                    None => true,
                }
            })
            .collect();
        if !running.is_empty() {
            lines.push(format!(
                "Subagents running now ({}): their results arrive as later [Arbos] messages; do not start the same work twice.",
                running.len()
            ));
            for s in running.iter().take(SUBAGENT_MAX_CONCURRENT + 2) {
                lines.push(format!(
                    "- {} ({}, {}): {}",
                    s.name,
                    s.id,
                    s.status,
                    if s.last_status.is_empty() {
                        "starting"
                    } else {
                        s.last_status.as_str()
                    }
                ));
            }
        }
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            if let Some(s) = c.compaction_summary {
                lines.push(format!(
                    "Summary of earlier turns in this chat: {}",
                    s.chars().take(6000).collect::<String>()
                ));
            }
        }
        for h in self.store.handles_for_chat(chat_id, 20).unwrap_or_default() {
            lines.push(format!(
                "- {} ({}, {} chars, {} lines): {}",
                h.id, h.kind, h.chars, h.lines, h.note
            ));
        }
        for (k, v) in self.store.scratch_all(chat_id).unwrap_or_default() {
            lines.push(format!(
                "- remember:{k} = {}",
                v.chars().take(120).collect::<String>()
            ));
        }
        // Goals are pinned in memory (cached; refreshed by the goals poller / MCP tools).
        let goals = self
            .goals_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !goals.is_empty() {
            lines.push(goals);
        }
        lines.into_iter().take(40).collect::<Vec<_>>().join("\n")
    }

    /// "Relevant skills" section for the running turn (F5): selected against the prompt text
    /// within `SECTION_BUDGET_TOKENS`; empty when nothing matches or no turn is running. The
    /// first call per turn records the chosen slugs on the assistant message (Details drawer).
    pub async fn skills_section_text(&self) -> String {
        let (chat_id, msg_id, prompt, first) = {
            let mut tl = self.turn.lock().await;
            let Some(t) = tl.as_mut() else {
                return String::new();
            };
            if t.prompt_text.trim().is_empty() {
                return String::new();
            }
            let first = t.skills_used.is_empty();
            (
                t.chat_id.clone(),
                t.assistant_msg_id.clone(),
                t.prompt_text.clone(),
                first,
            )
        };
        let index = self.skills.snapshot();
        let (selected, text) = skills::section_for(&prompt, &index, skills::SECTION_BUDGET_TOKENS);
        if selected.is_empty() {
            return String::new();
        }
        let slugs: Vec<String> = selected.iter().map(|s| s.slug.clone()).collect();
        if first {
            tracing::info!(
                chat_id,
                skills = ?slugs,
                tokens = arbos_core::skill::estimate_tokens(&text),
                "skills injected into the working set"
            );
            if let Some(t) = self.turn.lock().await.as_mut() {
                t.skills_used = slugs.clone();
            }
            if let Some(host) = self.sync.host(&chat_id).await {
                let mut h = host.lock().await;
                h.doc.add_hidden_part(
                    &msg_id,
                    "skills",
                    &text,
                    &json!({ "slugs": slugs, "scores": selected.iter().map(|s| s.score).collect::<Vec<_>>() }),
                );
            }
            let _ = self.store.t3_append(
                &chat_id,
                None,
                Some(&chat_id),
                "skills",
                Some("system"),
                &format!("skills injected: {}", slugs.join(", ")),
                None,
            );
        }
        text
    }

    // ---- subagents ----------------------------------------------------------------------------

    /// Context pack v0 (design §6.7 sections 1–4, 9, 10).
    fn build_pack(&self, name: &str, purpose: &str, brief: &str, data_heavy: bool) -> String {
        let mut s = String::new();
        s.push_str(&format!("# Task: {name}\n\n{purpose}\n\n{brief}\n\n"));
        s.push_str(&format!(
            "## Project folder\n\n`{}` (your working directory). Keep every file of this project there; other folders under `/work` belong to other projects.\n\n",
            self.cfg.work_dir.display()
        ));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str(&format!(
                "## Project instructions\n\n{}\n\n",
                self.cfg.instructions.trim()
            ));
        }
        let core = self.memory.core_memory_text();
        if !core.trim().is_empty() {
            s.push_str("## Project memory (T0 + T1)\n\n");
            s.push_str(&core);
            s.push('\n');
        }
        if data_heavy {
            s.push_str("## Data handling\n\nInputs are large. Keep data in variables or files and print slices; never paste whole files into the conversation.\n\n");
        }
        // Section 11 (F5): skills matching the brief, same budget as the main agent's section.
        let (selected, skills_text) = skills::section_for(
            &format!("{name} {purpose} {brief}"),
            &self.skills.snapshot(),
            skills::SECTION_BUDGET_TOKENS,
        );
        if !skills_text.is_empty() {
            tracing::info!(subagent = name, skills = ?selected.iter().map(|x| x.slug.as_str()).collect::<Vec<_>>(), "skills injected into the pack");
            s.push_str(&skills_text);
            s.push_str("\n\n");
        }
        s.push_str("## Tools\n\nMCP server `arbos`: context_search, context_open, context_around (this Project's memory tree: raw events plus index cards of episodes and threads), history_search, history_read, read_thread, memory_search, memory_read (you may search and read the entire history of this Project), peek, sub_llm, remember/recall, projects_list, ask_user (optional widget: choice, multi_select, text, date, form), skills_list/skills_read/skills_save (reusable how-tos), draft_message (an outbound email or chat message shown to the user as an approval card; nothing is sent without their click), connectors_list/connector_call (the user's Gmail, Calendar, Slack, GitHub, Harness Comms through the relay; write/send actions need the user's approval), local_devices/local_run/local_read/local_list (the user's own computer through the arbos-local helper; every command and, unless the user allowed reads, every file read needs the user's approval), deliver_file (hand a file you made to the user as a card), generate_image (an image from a prompt, delivered as a card), secrets_list/secrets_request/egress/browser_fill (the owner's secrets: you only ever get a placeholder such as `arbos-secret://STRIPE_KEY/value`, `$STRIPE_KEY` in a shell, or an `op://` reference; send it through `egress` or use it as `$NAME`; never try to read, print, encode or copy a value; a secret with no access → secrets_request and wait for the owner's card; if they deny or pause, stop and say so; logins are typed by browser_fill), routines_list/routines_create/routines_update/routines_delete (cron jobs and one-shot reminders on the relay scheduler), goals_list/goals_create/goals_update/goals_log/goals_delete (long-running objectives across days: plan, status, progress log, check-ins via routines; active goals stay pinned in the working set; autonomy ask|suggest|act), web_search/web_fetch (search the web; read a public page, spec or file as text), submit_feedback (file the user's feedback or bug report for them: captures their screen from their app, or attaches a PNG/JPEG they already provided via image_path; also attaches the recent conversation, tool calls and timings scrubbed of secrets, and returns a reference id; use it whenever the user asks to submit, send or report feedback or a bug — never point them to a menu, settings screen or another channel).\n\n");
        s.push_str(RESEARCH_SECTION);
        s.push_str(&crate::media::prompt_section(&self.cfg));
        s.push_str("## Return contract\n\nEnd your final reply with one fenced ```json block:\n{ \"status\": \"done\" | \"blocked\" | \"partial\" | \"needs_input\", \"summary\": \"<= 5 lines\", \"artifacts\": [ { \"kind\": \"file\" | \"commit\" | \"url\" | \"command\", \"ref\": \"…\", \"note\": \"…\" } ], \"acceptance\": [ { \"criterion\": \"…\", \"met\": true, \"evidence\": \"…\" } ], \"open_items\": [], \"proposed_memory\": [ { \"kind\": \"decision\", \"scope\": \"project\", \"text\": \"…\" } ], \"questions\": [] }\n");
        s
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_subagent(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        in_reply_to: &str,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        route_harness: Option<Harness>,
        data_heavy: bool,
        escalated: bool,
    ) -> Result<()> {
        let default_harness = std::env::var("ARBOS_DEFAULT_HARNESS")
            .ok()
            .and_then(|s| match s.as_str() {
                "pi" => Some(Harness::Pi),
                "claude_code" => Some(Harness::ClaudeCode),
                "prime" => Some(Harness::Prime),
                _ => None,
            })
            .unwrap_or(Harness::Prime);
        let (_, subagent_model) = self.live_models().await;
        let (subagent_id, harness, is_new) = match existing {
            Some(id) => {
                let row = match self.store.subagent(&id)? {
                    Some(r) => r,
                    None => self
                        .store
                        .subagent_by_name(&id)?
                        .ok_or_else(|| anyhow!("unknown subagent {id}"))?,
                };
                let h = match row.harness.as_str() {
                    "pi" => Harness::Pi,
                    "claude_code" => Harness::ClaudeCode,
                    _ => Harness::Prime,
                };
                (row.id, h, false)
            }
            None => {
                let h = pick_harness(
                    route_harness,
                    data_heavy,
                    brief,
                    &subagent_model,
                    default_harness,
                );
                let id = ulid();
                self.store
                    .insert_subagent(&id, name, purpose, h.as_str(), chat_id)?;
                (id, h, true)
            }
        };
        let adapter = self.adapter(harness);
        let sub_name = self
            .store
            .subagent(&subagent_id)?
            .map(|s| s.name)
            .unwrap_or_else(|| name.to_owned());

        // The progress message carries status events and hidden parts only; the main agent writes
        // the prose ack before this and the prose result after (demo review items 1–2).
        let msg_id = ulid();
        {
            let mut h = host.lock().await;
            h.doc
                .start_assistant_message(&msg_id, in_reply_to, &self.cfg.device_id);
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("subagentId", json!(subagent_id)),
                    ("subagentName", json!(sub_name)),
                    (
                        "turn",
                        json!({ "kind": if is_new { "spawn" } else { "wake" } }),
                    ),
                    ("escalated_from_act", json!(escalated)),
                    ("model", json!(subagent_model)),
                    // Messages exchanged with the subagent so far: the brief now, the
                    // result later (clients say "Messaged X" / "N messages with X").
                    ("handoffMessages", json!(1)),
                ],
            );
        }
        let queued_at = Instant::now();
        if self.subagent_slots.available_permits() == 0 {
            let waiting = self
                .store
                .subagents()
                .unwrap_or_default()
                .iter()
                .filter(|s| s.status == "queued")
                .count();
            let _ = self.store.set_subagent_status(&subagent_id, "queued");
            let _ = self
                .store
                .set_subagent_last_status(&subagent_id, &format!("queued ({waiting} ahead)"));
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&msg_id, "working", "Waiting for a free slot…");
        }
        self.write_threads_index().await;
        let _slot = self
            .subagent_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("subagent slots closed"))?;
        let _box_slot = self
            .box_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("box subagent slots closed"))?;
        let queued_ms = queued_at.elapsed().as_millis() as i64;
        if queued_ms > 500 {
            let _ = self
                .post_timing("subagent_queued_ms", queued_ms, Some(harness.as_str()))
                .await;
        }
        {
            let mut h = host.lock().await;
            h.doc.add_status_part(&msg_id, "working", "Working…");
        }
        self.subagent_msgs
            .lock()
            .await
            .insert(subagent_id.clone(), (chat_id.to_owned(), msg_id.clone()));
        let _ = self.store.set_subagent_last_status(&subagent_id, "working");
        let t0 = Instant::now();
        let mut pack = self.build_pack(&sub_name, purpose, brief, data_heavy);
        if let Some(memory_block) = self
            .tree_pack(
                chat_id,
                Some(in_reply_to),
                "subagent",
                &format!("{purpose}\n{brief}"),
            )
            .await
        {
            pack.push_str("\n## Retrieved memory\n\n");
            pack.push_str(&memory_block);
            pack.push('\n');
        }
        std::fs::create_dir_all(self.cfg.state_dir.join("packs"))?;
        std::fs::write(
            self.cfg
                .state_dir
                .join("packs")
                .join(format!("{subagent_id}.md")),
            &pack,
        )?;
        self.store.set_subagent_status(&subagent_id, "running")?;
        self.write_threads_index().await;
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "user",
            Some("user"),
            brief,
            Some(&json!({ "chat_id": chat_id, "pack": "pack-v0" })),
        );

        let mut resumed_fresh = false;
        if is_new {
            adapter
                .start(StartRequest {
                    session_id: subagent_id.clone(),
                    cwd: self.cfg.work_dir.clone(),
                    system_prompt: pack.clone(),
                    model: subagent_model.clone(),
                    thinking: "medium".into(),
                })
                .await?;
            let _ = self
                .post_timing(
                    "subagent_start_ms",
                    t0.elapsed().as_millis() as i64,
                    Some(harness.as_str()),
                )
                .await;
        } else if !adapter.is_open(&subagent_id) {
            match adapter.resume(&subagent_id).await {
                Ok(()) => {
                    let _ = self
                        .post_timing(
                            "subagent_resume_ms",
                            t0.elapsed().as_millis() as i64,
                            Some(harness.as_str()),
                        )
                        .await;
                }
                Err(e) => {
                    // A session that cannot come back (moved box, corrupt store, harness upgrade)
                    // must not sink the hand-off: start the same subagent fresh with its summary.
                    tracing::warn!(subagent = %subagent_id, error = %e, "resume failed; starting the subagent fresh");
                    resumed_fresh = true;
                    adapter
                        .start(StartRequest {
                            session_id: subagent_id.clone(),
                            cwd: self.cfg.work_dir.clone(),
                            system_prompt: pack.clone(),
                            model: subagent_model.clone(),
                            thinking: "medium".into(),
                        })
                        .await
                        .with_context(|| format!("resume failed ({e}); fresh start failed too"))?;
                }
            }
        }
        let stream = adapter
            .stream(&subagent_id)
            .ok_or_else(|| anyhow!("no stream for {subagent_id}"))?;
        let first_prompt = if is_new {
            brief.to_owned()
        } else if resumed_fresh {
            let summary = self
                .store
                .subagent(&subagent_id)
                .ok()
                .flatten()
                .map(|r| r.summary)
                .unwrap_or_default();
            format!(
                "Your previous session could not be resumed, so this is a fresh session for the same thread. What you had done before: {}\n\nContinue:\n\n{brief}",
                if summary.trim().is_empty() { "(no summary recorded)" } else { summary.trim() }
            )
        } else {
            format!("Context update: continue your thread.\n\n{brief}")
        };
        let mut stream = match adapter.prompt(&subagent_id, &first_prompt).await {
            Ok(()) => stream,
            Err(e) if !is_new && !resumed_fresh => {
                // The resumed child died on first contact (broken pipe): same fallback as above.
                tracing::warn!(subagent = %subagent_id, error = %e, "resumed session failed on prompt; starting fresh");
                let _ = adapter.stop(&subagent_id).await;
                adapter
                    .start(StartRequest {
                        session_id: subagent_id.clone(),
                        cwd: self.cfg.work_dir.clone(),
                        system_prompt: pack.clone(),
                        model: subagent_model.clone(),
                        thinking: "medium".into(),
                    })
                    .await
                    .with_context(|| format!("prompt failed ({e}); fresh start failed too"))?;
                let fresh = adapter
                    .stream(&subagent_id)
                    .ok_or_else(|| anyhow!("no stream for {subagent_id}"))?;
                let summary = self
                    .store
                    .subagent(&subagent_id)
                    .ok()
                    .flatten()
                    .map(|r| r.summary)
                    .unwrap_or_default();
                adapter
                    .prompt(
                        &subagent_id,
                        &format!(
                            "Your previous session could not be resumed, so this is a fresh session for the same thread. What you had done before: {}\n\nContinue:\n\n{brief}",
                            if summary.trim().is_empty() { "(no summary recorded)" } else { summary.trim() }
                        ),
                    )
                    .await?;
                fresh
            }
            Err(e) => return Err(e),
        };

        self.subagent_turns.fetch_add(1, Ordering::SeqCst);
        let keepalive = self.spawn_keepalive();
        let mut text = String::new();
        let mut sub_tool_inputs: HashMap<String, Value> = HashMap::new();
        let mut usage_total = json!({});
        let mut first_delta: Option<i64> = None;
        let mut done_err: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(60 * 60);
        loop {
            let ev = tokio::select! {
                e = stream.recv() => e,
                _ = tokio::time::sleep_until(deadline.into()) => { done_err = Some("subagent exceeded 60 minutes".into()); break; }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match ev {
                AgentEvent::TextDelta(d) => {
                    // Raw subagent prose never renders (item 1): kept for the hidden result part.
                    if first_delta.is_none() {
                        first_delta = Some(t0.elapsed().as_millis() as i64);
                    }
                    text.push_str(&d);
                }
                AgentEvent::ReasoningDelta(_) => {}
                AgentEvent::ToolCall { id, name, input } => {
                    sub_tool_inputs.insert(id, input.clone());
                    let label = step_label(&name, &input);
                    let _ = self
                        .store
                        .set_subagent_last_status(&subagent_id, &format!("step: {label}"));
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_call",
                        Some("assistant"),
                        &format!("{name} {input}"),
                        None,
                    );
                }
                AgentEvent::ToolResult {
                    id,
                    name,
                    output,
                    is_error,
                } => {
                    let output = crate::store::redact(&output);
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_output",
                        Some("tool"),
                        &format!("{name}: {output}"),
                        None,
                    );
                    let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                    let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                        Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                    } else {
                        None
                    };
                    let input = sub_tool_inputs.remove(&id).unwrap_or(json!({}));
                    let mut h = host.lock().await;
                    h.doc.add_tool_part(
                        &msg_id,
                        &id,
                        &name,
                        &input,
                        &summary,
                        output_ref.as_deref(),
                        Some(output.len() as i64),
                        is_error,
                    );
                }
                AgentEvent::Usage {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cost_usd,
                } => {
                    usage_total = json!({ "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write, "cost": cost_usd });
                }
                AgentEvent::InputRequested { question } => {
                    let qid = ulid();
                    let _ = self.store.insert_ask(
                        &qid,
                        Some(chat_id),
                        Some(&subagent_id),
                        &question,
                        None,
                    );
                    let mut h = host.lock().await;
                    h.doc.add_question_part(&msg_id, &qid, &question, None);
                    h.doc.add_status_part(&msg_id, "waiting_user", &question);
                }
                AgentEvent::CompactionStarted => {}
                AgentEvent::CompactionEnded { summary } => {
                    let n = self
                        .store
                        .bump_session_compactions(&subagent_id)
                        .unwrap_or(0);
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "compaction",
                        Some("system"),
                        &format!(
                            "session compacted ({n}); summary {} chars",
                            summary.as_deref().map(str::len).unwrap_or(0)
                        ),
                        None,
                    );
                }
                AgentEvent::Done { aborted, error } => {
                    if aborted || error.is_some() {
                        done_err = error.or(Some("aborted".into()));
                    }
                    break;
                }
            }
        }
        keepalive.abort();
        self.subagent_turns.fetch_sub(1, Ordering::SeqCst);
        let total_ms = t0.elapsed().as_millis() as i64;
        let contract = extract_return_contract(&text);
        let status = contract
            .as_ref()
            .and_then(|c| c.get("status"))
            .and_then(Value::as_str)
            .unwrap_or(if done_err.is_some() {
                "blocked"
            } else {
                "done"
            })
            .to_owned();
        let failed = done_err.is_some() || status == "blocked";
        {
            let mut h = host.lock().await;
            if let Some(e) = &done_err {
                h.doc.add_error_part(&msg_id, "subagent_error", e);
            }
            // Raw output + contract: Details drawer only (`hidden: true`).
            h.doc.add_hidden_part(
                &msg_id,
                "subagent_result",
                &text,
                &json!({
                    "subagentId": subagent_id,
                    "status": status,
                    "returnContract": contract.clone().unwrap_or(Value::Null),
                    "durationMs": total_ms,
                }),
            );
            h.doc.add_status_part(
                &msg_id,
                if failed { "failed" } else { "done" },
                if failed { "Failed" } else { "Done" },
            );
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("usage", usage_total.clone()),
                    ("returnContract", contract.clone().unwrap_or(Value::Null)),
                ],
            );
            h.doc.finish_message(
                &msg_id,
                if done_err.is_some() {
                    MessageStatus::Error
                } else {
                    MessageStatus::Done
                },
            );
        }
        self.subagent_msgs.lock().await.remove(&subagent_id);
        let _ = self
            .store
            .set_subagent_last_status(&subagent_id, if failed { "failed" } else { "done" });
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "assistant",
            Some("assistant"),
            &text,
            Some(&json!({ "chat_id": chat_id, "message_id": msg_id })),
        );
        let new_status = match status.as_str() {
            "needs_input" => "waiting_user",
            _ => "idle",
        };
        self.store.set_subagent_status(&subagent_id, new_status)?;
        if let Some(c) = &contract {
            if let Some(s) = c.get("summary").and_then(Value::as_str) {
                let _ = self.store.set_subagent_summary(&subagent_id, s, &json!([]));
            }
            if let Some(qs) = c.get("questions").and_then(Value::as_array) {
                for q in qs.iter().filter_map(Value::as_str) {
                    let _ =
                        self.store
                            .insert_ask(&ulid(), Some(chat_id), Some(&subagent_id), q, None);
                }
            }
        }
        let _ = self
            .post_timing("subagent_turn_total_ms", total_ms, Some(harness.as_str()))
            .await;
        if let Some(fd) = first_delta {
            let _ = self
                .post_timing("subagent_ttft_ms", fd, Some(harness.as_str()))
                .await;
        }
        self.write_threads_index().await;
        // T3 index of the subagent's own session file (history_search source) + write pass, off path.
        {
            let me = self.clone();
            let sid = subagent_id.clone();
            let proposals = contract
                .as_ref()
                .and_then(|c| c.get("proposed_memory"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let summary_text = contract
                .as_ref()
                .and_then(|c| c.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let text2 = text.clone();
            let chat = chat_id.to_owned();
            tokio::spawn(async move {
                me.index_session_history(&sid, harness).await;
                let body = if summary_text.is_empty() {
                    text2
                } else {
                    summary_text
                };
                let _ = me
                    .memory
                    .write_pass(
                        &format!("subagent:{sid}"),
                        &sid,
                        "subagent",
                        &body,
                        &proposals,
                    )
                    .await;
                let _ = me.memory.maybe_compact(&chat).await;
                let _ = me.write_system_prompt();
            });
        }
        drop(_box_slot);
        drop(_slot);
        // The user hears about it from the main agent, in prose, in this chat.
        {
            let mut h = host.lock().await;
            h.doc
                .set_message_fields(&msg_id, &[("handoffMessages", json!(2))]);
        }
        let result_text = match &contract {
            Some(c) => format!("{text}\n\nParsed return contract:\n{c}"),
            None => text.clone(),
        };
        self.run_result_turn(
            chat_id,
            host,
            in_reply_to,
            &subagent_id,
            &sub_name,
            &status,
            total_ms,
            &result_text,
            done_err.as_deref(),
        )
        .await
    }

    /// Tail the subagent's session JSONL into T3 (pre-compaction entries included).
    pub async fn index_session_history(&self, session_id: &str, harness: Harness) {
        let adapter = self.adapter(harness);
        let cursor = self.store.session_cursor(session_id).ok().flatten();
        match adapter.read_history(session_id, cursor).await {
            Ok((entries, next)) => {
                for e in &entries {
                    if let Some((role, text)) = crate::harness::entry_text(e) {
                        let _ = self.store.t3_append(
                            session_id,
                            Some(session_id),
                            Some(session_id),
                            "session_entry",
                            Some(&role),
                            &text,
                            Some(&json!({ "entry_id": e.get("id") })),
                        );
                    }
                }
                if let Some(c) = next {
                    let _ = self.store.set_session_cursor(session_id, &c);
                }
            }
            Err(e) => tracing::debug!(error = %e, session_id, "history index"),
        }
    }

    /// `threads` (T1 index) + `status` in the project doc.
    pub async fn write_threads_index(&self) {
        let Some(h) = self.sync.host(arbos_core::wire::PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        let h = h.lock().await;
        let threads = h.doc.doc.get_map("threads");
        for s in self.store.subagents().unwrap_or_default() {
            let v = json!({ "name": s.name, "purpose": s.purpose, "summary": s.summary, "tags": s.tags, "status": s.status, "lastTurnAt": s.last_turn_at, "harness": s.harness });
            let lv: loro::LoroValue = serde_json::from_value(v).unwrap_or(loro::LoroValue::Null);
            threads.insert(&s.id, lv).ok();
        }
        h.doc.doc.commit();
    }

    /// MCP `spawn_subagent` / `wake_subagent` from inside an act turn (D66 escalation).
    pub async fn spawn_from_tool(
        self: &Arc<Self>,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        isolate: bool,
    ) -> Result<String> {
        let _ = isolate;
        let (chat_id, user_msg) = {
            let t = self.turn.lock().await;
            match t.as_ref() {
                Some(t) => (t.chat_id.clone(), t.user_msg_id.clone()),
                None => (
                    self.store
                        .chats()?
                        .first()
                        .map(|c| c.id.clone())
                        .unwrap_or_default(),
                    String::new(),
                ),
            }
        };
        let host = self
            .sync
            .host(&chat_id)
            .await
            .ok_or_else(|| anyhow!("chat room not open"))?;
        if let Some(t) = self.turn.lock().await.as_mut() {
            t.escalate = false;
            let mut h = host.lock().await;
            h.doc
                .add_status_part(&t.assistant_msg_id, "working", "Working…");
        }
        let me = self.clone();
        let (n, p, b) = (name.to_owned(), purpose.to_owned(), brief.to_owned());
        let id_holder = Arc::new(Mutex::new(String::new()));
        let idh = id_holder.clone();
        tokio::spawn(async move {
            let _ = idh;
            if let Err(e) = me
                .run_subagent(
                    &chat_id, &host, &user_msg, existing, &n, &p, &b, None, false, true,
                )
                .await
            {
                tracing::warn!(error = %e, "spawn_from_tool");
            }
        });
        Ok("subagent started; its result will be posted in this chat".to_owned())
    }

    /// Stop every child in order (subagents, then pi) so session files and kernel snapshots land.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.pi_ready.store(false, Ordering::SeqCst);
        for a in self.adapters.values() {
            a.shutdown_all().await;
        }
        if let Some(pi) = self.pi.lock().await.take() {
            pi.shutdown().await;
        }
    }

    /// True while the main agent or any subagent is mid-turn.
    pub fn any_turn_running(&self) -> bool {
        self.turn_running.load(Ordering::SeqCst) || self.subagent_turns.load(Ordering::SeqCst) > 0
    }

    pub fn is_healthy(&self) -> bool {
        self.pi_ready.load(Ordering::SeqCst)
    }
}

/// Short label for a `step` status event: the command head for `bash`, the path for file tools,
/// otherwise the tool name and its first string argument. One line, ≤ 72 chars.
pub fn step_label(tool_name: &str, args: &Value) -> String {
    let first_str = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| args.get(k).and_then(Value::as_str))
            .or_else(|| {
                args.as_object()
                    .and_then(|o| o.values().find_map(Value::as_str))
            })
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let body = match tool_name {
        "bash" | "shell" | "execute" => first_str(&["command", "cmd"]),
        "read" | "write" | "edit" | "read_file" | "write_file" | "edit_file" | "glob" | "grep" => {
            first_str(&["path", "file_path", "pattern"])
        }
        _ => first_str(&["query", "url", "name", "code"]),
    }
    .unwrap_or_default();
    let label = if body.is_empty() {
        tool_name.to_owned()
    } else {
        format!("{tool_name} {body}")
    };
    let mut out: String = label.chars().take(72).collect();
    if out.chars().count() < label.chars().count() {
        out.push('…');
    }
    out
}

pub fn route_name(r: &ValidRoute) -> &'static str {
    match r {
        ValidRoute::Reply { .. } => "reply",
        ValidRoute::Act { .. } => "act",
        ValidRoute::Wake { .. } => "wake",
        ValidRoute::New { .. } => "new",
    }
}

/// Main-turn wall-clock deadline in seconds. `ARBOS_TURN_DEADLINE_SECS` (default 900).
/// A user row from a voice-call tool (`ask_agent` / `look`): `voice: true` and/or `source: "voice"`.
fn message_from_voice(m: &arbos_core::doc::MessageView) -> bool {
    m.rest.get("voice").and_then(Value::as_bool) == Some(true)
        || m.rest.get("source").and_then(Value::as_str) == Some("voice")
}

fn turn_deadline_secs() -> u64 {
    std::env::var("ARBOS_TURN_DEADLINE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(TURN_DEADLINE_SECS)
}

/// A failed `codemode` script (D86): the sandbox's own limits — the script deadline, the 256 MB VM
/// heap, the output-item cap — get a chat line; a plain script error (a thrown `Error`, a bad tool
/// name) returns `None` and stays between the model and its tools.
fn classify_script_limit(result: &str) -> Option<(&'static str, bool)> {
    let lower = result.to_ascii_lowercase();
    if lower.contains("script timed out") {
        return Some((
            "A script I was running hit its time limit; I'm continuing in smaller steps.",
            false,
        ));
    }
    if lower.contains("out of memory") {
        return Some((
            "A script I was running hit its memory limit; I'm continuing with smaller data.",
            false,
        ));
    }
    if lower.contains("script sandbox failed") || lower.contains("too many output items") {
        return Some((
            "A script I was running hit a sandbox limit; I'm continuing with smaller steps.",
            false,
        ));
    }
    classify_hard_limit(result)
}

/// Map a provider / tool error to a short chat announcement and whether to escalate to a subagent.
/// Returns `None` when the error is not a known hard limit (ordinary tool failures stay quiet).
fn classify_hard_limit(err: &str) -> Option<(&'static str, bool)> {
    let lower = err.to_ascii_lowercase();
    if lower.contains("spend_cap")
        || lower.contains("credits_exhausted")
        || lower.contains("credits are used up")
        || lower.contains("daily spend cap")
        || lower.contains("insufficient credits")
        || (lower.contains("spend") && lower.contains("cap"))
    {
        return Some((
            "I hit the spend limit for today, so I have to stop here.",
            false,
        ));
    }
    if lower.contains("context_length")
        || lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("prompt is too long")
        || lower.contains("too many tokens")
        || lower.contains("token limit")
        || (lower.contains("max_tokens")
            && (lower.contains("exceed") || lower.contains("limit") || lower.contains("over")))
    {
        return Some((
            "I hit the model's context limit; I'm summarizing and continuing in a fresh context.",
            true,
        ));
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline exceeded")
        || lower.contains("time-out")
    {
        return Some((
            "A tool or model call timed out; I'll try another approach.",
            false,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D91: the window size pi would start the next turn from, as pi counts it.
    #[test]
    fn context_tokens_follow_pi() {
        assert_eq!(
            context_tokens(
                &json!({ "input": 1000, "output": 200, "cacheRead": 60000, "cacheWrite": 3000 })
            ),
            64200
        );
        assert_eq!(
            context_tokens(
                &json!({ "totalTokens": 70000, "input": 1, "output": 1, "cacheRead": 1, "cacheWrite": 1 })
            ),
            70000
        );
        assert_eq!(context_tokens(&json!({})), 0);
    }

    /// D86: per-turn totals sum every model call; the doc keeps the last call's usage and gains the
    /// totals and the three call counts.
    #[test]
    fn turn_metrics_sum_model_calls() {
        let mut m = TurnMetrics::default();
        assert!(m.usage_for_doc().is_null());
        m.add_model_call(Some(&json!({ "input": 4, "output": 70, "cacheRead": 0, "cacheWrite": 11000, "cost": { "total": 0.0014 } })));
        m.add_model_call(Some(&json!({ "input": 2, "output": 30, "cacheRead": 11000, "cacheWrite": 300, "cost": { "total": 0.0002 } })));
        m.add_model_call(None);
        m.usage = Some(json!({ "input": 2, "output": 30 }));
        m.tool_calls = 2;
        m.nested_calls = 5;
        let v = m.usage_for_doc();
        assert_eq!(v["modelCalls"], 3);
        assert_eq!(v["toolCalls"], 2);
        assert_eq!(v["nestedCalls"], 5);
        assert_eq!(
            v["input"], 2,
            "last call's usage stays where clients read it"
        );
        assert_eq!(v["total"]["input"], 6);
        assert_eq!(v["total"]["cacheRead"], 11000);
        assert_eq!(v["total"]["cacheWrite"], 11300);
        assert!((v["total"]["cost"].as_f64().unwrap() - 0.0016).abs() < 1e-9);
    }

    /// D86: sandbox limits of a script are announced; a plain script error is not.
    #[test]
    fn script_limits_are_announced_plain_errors_are_not() {
        assert!(classify_script_limit("Script failed\nWall time 900.1 seconds\nOutput:\nScript error:\nScript timed out: deadline of 900000 ms passed").is_some());
        assert!(classify_script_limit(
            "Script failed\nScript error:\nInternalError: out of memory"
        )
        .is_some());
        assert!(classify_script_limit("Script failed\nScript error:\nTypeError: tools.arbos_submit_feedback is not a function").is_none());
        assert!(classify_script_limit(
            "Script failed\nScript error:\nError: not executed: approval rejected"
        )
        .is_none());
    }

    /// The code-mode prompt names the exact script identifiers of the project tools, so the model
    /// does not spend a round trip (or a TypeError) discovering them.
    #[test]
    fn code_mode_section_names_script_identifiers() {
        for id in [
            "tools.mcp__arbos__web_search",
            "tools.mcp__arbos__egress",
            "tools.mcp__arbos__secrets_list",
            "tools.read(",
            "tools.bash(",
        ] {
            assert!(CODE_MODE_SECTION.contains(id), "missing {id}");
        }
        assert!(CODE_MODE_SECTION
            .contains("Never call `route`, `ask_user` or `send_message` from a program"));
    }

    /// Routing rule: the inline `reply` streams out of partial JSON exactly as written.
    #[test]
    fn reply_extractor_streams_partial_json() {
        let mut ex = ReplyExtractor::default();
        let mut out = String::new();
        for d in [
            "{\"action\": \"reply",
            "\", \"reply\": \"Hi ",
            "there\\n\\\"you\\\"",
            " — ok\"}",
        ] {
            out.push_str(&ex.feed(d));
        }
        assert_eq!(out, "Hi there\n\"you\" — ok");
        let mut ex2 = ReplyExtractor::default();
        assert_eq!(
            ex2.feed("{\"action\":\"act\",\"plan\":\"x\",\"reply\":null}"),
            ""
        );
    }

    /// Clone-style routing thoughts in `ack` leave only the user-facing sentence.
    #[test]
    fn strip_thought_prefix_keeps_clone_ack() {
        let raw = "The user wants me to clone a GitHub repo. That's real work — cloning can take a bit and might exceed a quick act if the repo is large. The instructions say prefer act over new only when it fits 8 calls and ~60s, no build. A git clone of one repo is typically one command. I'll act and clone it into the project folder.\n\nI should not use /work itself. Clone into the project folder, e.g. subtensor/.\n\nAck first via route.Cloning RaoFoundation/subtensor into this project now.";
        let cleaned = ProseFilter::strip_thought_prefix(raw);
        assert_eq!(
            cleaned.trim(),
            "Cloning RaoFoundation/subtensor into this project now."
        );
    }

    /// Meta-steering echoed before a greeting (bold-wrapped) leaves only the greeting.
    #[test]
    fn clean_prose_strips_dont_mention_routing_prefix() {
        let raw = "**Don't mention routing. Short greeting.** Hey. I'm here. What do you need?";
        assert_eq!(
            ProseFilter::clean_prose(raw).trim(),
            "Hey. I'm here. What do you need?"
        );
    }

    /// CoT that drafts with `Reply:` then keeps only the drafted answer.
    #[test]
    fn clean_prose_keeps_text_after_reply_label() {
        let raw = "Looking at the conversation, I didn't say that to the user — that was an internal instruction I was thinking about. I should not reveal system instructions. The message says they saw routing words. Reply: I didn't say that to you. If you saw those words somewhere, they weren't a message I sent. What were you looking at?";
        assert_eq!(
            ProseFilter::clean_prose(raw).trim(),
            "I didn't say that to you. If you saw those words somewhere, they weren't a message I sent. What were you looking at?"
        );
    }

    /// Permission CoT in its own paragraph before the real answer (feedback #1202).
    #[test]
    fn clean_prose_strips_self_steer_thought_paragraph() {
        let raw = "Arbos is a guest, not the account owner — Const is. The actual rule hasn't changed: Kenobi can't trigger rentals without Const's approval, so I only need to state that briefly and not treat Arbos as having authority over the account. Arbos, it's Const's account, not yours.\n\nKenobi, 42 nodes on Lium right now. Prices are per GPU per hour.\n\nNo RTX 5070s listed at the moment. Renting still needs Const's go-ahead since it bills to his account.";
        let cleaned = ProseFilter::clean_prose(raw);
        assert_eq!(
            cleaned.trim(),
            "Kenobi, 42 nodes on Lium right now. Prices are per GPU per hour. No RTX 5070s listed at the moment. Renting still needs Const's go-ahead since it bills to his account."
        );
        assert!(!cleaned.contains("I only need to state"));
        assert!(!cleaned.contains("not treat Arbos"));
        assert!(!cleaned.contains("Arbos is a guest"));
    }

    /// Self-steer mid-paragraph without a blank line still drops the thought lead-in.
    #[test]
    fn clean_prose_strips_through_self_steer_without_blank_line() {
        let raw = "Arbos is a guest, not the account owner — Const is. The actual rule hasn't changed: Kenobi can't trigger rentals without Const's approval, so I only need to state that briefly and not treat Arbos as having authority over the account. Kenobi, 42 nodes on Lium right now.";
        let cleaned = ProseFilter::clean_prose(raw);
        assert_eq!(cleaned.trim(), "Kenobi, 42 nodes on Lium right now.");
        assert!(!cleaned.contains("I only need to state"));
        assert!(!cleaned.contains("Arbos is a guest"));
    }

    /// Status prose with a glued tool-narration tail drops the narration and keeps spacing.
    #[test]
    fn clean_prose_strips_glued_checking_tail() {
        let raw = "I have good data on the research loop from the retrieved memory. The baseline is done at val_loss 6.2568 with a 124M model, and the accum sweep (bs4/bs2/bs1) is running—no improvement yet. I'll report status from what I already have.Checking the latest experiment results.";
        let cleaned = ProseFilter::clean_prose(raw);
        assert_eq!(
            cleaned.trim(),
            "I have good data on the research loop from the retrieved memory. The baseline is done at val_loss 6.2568 with a 124M model, and the accum sweep (bs4/bs2/bs1) is running—no improvement yet. I'll report status from what I already have."
        );
        assert!(!cleaned.contains("Checking"));
        assert!(!cleaned.contains("have.Checking"));
    }

    /// A lone glued answer after thought-aloud still becomes just the answer.
    #[test]
    fn clean_prose_keeps_glued_austin_answer() {
        let raw = "The user is asking a simple factual question. I should reply directly.Austin.";
        assert_eq!(ProseFilter::clean_prose(raw).trim(), "Austin.");
    }

    /// Two real sentences glued without a space get a space, not a drop.
    #[test]
    fn clean_prose_fixes_glued_real_sentences() {
        let raw = "Baseline is done.Accum sweep is still running.";
        assert_eq!(
            ProseFilter::clean_prose(raw).trim(),
            "Baseline is done. Accum sweep is still running."
        );
    }

    /// Streaming: after real prose has started, a glued Checking-tail is held and dropped.
    #[test]
    fn feed_holds_then_drops_glued_checking_tail() {
        let mut f = ProseFilter::default();
        let first = f.feed("I'll report status from what I already have.\n");
        assert!(
            first.contains("already have."),
            "prose should stream: {first:?}"
        );
        let mid = f.feed("Checking the latest experiment results");
        assert!(
            !mid.contains("Checking"),
            "incomplete narration must not land: {mid:?}"
        );
        let end = f.feed(".");
        assert!(
            !end.contains("Checking"),
            "finished narration must stay out: {end:?}"
        );
        let flushed = f.flush();
        assert!(
            !flushed.contains("Checking"),
            "flush must not revive narration: {flushed:?}"
        );
    }
}
