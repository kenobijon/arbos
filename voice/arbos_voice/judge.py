"""Relevance judge for group calls (spec §3, Jacob's 2026-10-16 correction).

Every finished utterance gets one decision: `speak`, `act`, or `silent`.

* `forced` rules first: the agent's name, the wake name, "@mention", or a kill word in the
  utterance force a reply (or a stop/leave) — no model call.
* Otherwise one Decisions-API request to TypeSafe Jev (`typesafe/jev-1.14`, through the relay's
  `/proxy/decisions`): a `choice` question over {speak, act, silent} with explicit criteria and
  the recent transcript as state. The probabilities are calibrated, so the owner's chattiness
  setting is a threshold on P(not silent): quiet 0.85, balanced 0.60, chatty 0.40.
* If Jev errors, a small chat model answers the same question in JSON (fail-open to the LLM,
  then to `silent`).

The judge is pure over its inputs so the fixture set under `voice/tests/fixtures` can score it.
"""

from __future__ import annotations

import logging
import re
import time
from dataclasses import dataclass, field
from typing import Any

from .relay import Relay

log = logging.getLogger("arbos-voice.judge")

CHATTINESS_THRESHOLD: dict[str, float] = {"quiet": 0.85, "balanced": 0.60, "chatty": 0.40}
# Named directly, the agent still stays quiet when the judge is this sure nothing is asked
# ("thanks Arbos, that's all").
NAMED_SILENT_FLOOR = 0.9

KILL_STOP = ("stop",)
KILL_LEAVE = ("leave", "leave the call", "hang up", "go away")

LLM_SYSTEM = """You decide what an AI agent that is present on a group voice call should do about the latest thing someone said.
The agent can speak out loud and can do work on a computer (run commands, look things up, edit files, post in the chat).
Answer with JSON: {"decision": "speak" | "act" | "silent", "confidence": 0-1, "reason": "one line"}.

speak: someone asked the agent a question it can answer from the conversation or general knowledge; a correction or fact the group clearly needs; the agent following up on its own work; someone addressed the agent by name.
act: someone asked or told the agent (even implicitly, e.g. "can someone check X", "we need the logs", "somebody should look that up") to do work that needs its tools or computer.
silent: people are talking to each other; small talk; a remark that needs no reply; a request clearly for another person by name; the agent was just quoted or negated ("don't ask the agent")."""


@dataclass
class Decision:
    action: str  # speak | act | silent | stop | leave
    p_speak: float | None = None
    p_act: float | None = None
    p_silent: float | None = None
    p_addressed: float | None = None
    provider: str = "rules"
    model: str | None = None
    latency_ms: int = 0
    cost_usd: float | None = None
    forced: bool = False
    reason: str = ""
    raw: Any = None

    def as_event(self, speaker: str, text: str) -> dict:
        return {
            "kind": "decision",
            "speaker": speaker,
            "text": text[:500],
            "decision": self.action,
            "forced": self.forced,
            "p_speak": self.p_speak,
            "p_act": self.p_act,
            "p_silent": self.p_silent,
            "p_addressed": self.p_addressed,
            "provider": self.provider,
            "model": self.model,
            "latency_ms": self.latency_ms,
            "cost_usd": self.cost_usd,
            "reason": self.reason,
        }


@dataclass
class JudgeContext:
    agent_name: str
    agent_role: str
    chat_kind: str
    wake_name: str | None
    chattiness: str = "balanced"
    # Oldest first: [{"from": "@sam", "text": "..."}]
    recent: list[dict] = field(default_factory=list)
    agent_last_spoke_s_ago: float | None = None
    agent_has_open_work: bool = False


def _norm(s: str) -> str:
    return re.sub(r"[^a-z0-9@ ]+", " ", s.lower()).strip()


def names_of(ctx: JudgeContext) -> list[str]:
    """The agent's names, each also without spaces ("Alpha Lab" is transcribed "AlphaLab")."""
    names = {"arbos", ctx.agent_name.lower().strip()}
    if ctx.wake_name:
        names.add(ctx.wake_name.lower().strip())
    out = set()
    for n in names:
        if n:
            out.add(n)
            out.add(n.replace(" ", ""))
    return sorted(out, key=len, reverse=True)


def _vocative(t: str, names: list[str]) -> bool:
    """Is a name used to address the agent (start or end of the sentence, or set off by a pause)?"""
    words = t.split()
    for n in names:
        parts = n.split()
        k = len(parts)
        for i in range(len(words) - k + 1):
            if words[i : i + k] != parts:
                continue
            if i <= 2 or i + k >= len(words) - 1:
                return True
    return any(w.startswith("@") for w in words)


def forced_decision(text: str, ctx: JudgeContext) -> Decision | None:
    """Name / @mention / kill words. Returns None when the rules say nothing."""
    t = _norm(text)
    if not t:
        return Decision(action="silent", forced=True, reason="empty")
    names = names_of(ctx)
    if not any(n in t for n in names) and not any(w.startswith("@") for w in t.split()):
        return None
    # Negated or quoted: "don't ask Arbos", "no need to ask Arbos" → silent.
    if re.search(
        r"\b(don t|dont|do not|no need to|without|never) (ask|bother|call|wake|need) (the agent|"
        + "|".join(map(re.escape, names))
        + r")\b",
        t,
    ):
        return Decision(action="silent", forced=True, reason="negated mention")
    # Kill words: "<name>, stop" / "<name> leave".
    for n in names:
        idx = t.find(n)
        if idx < 0:
            continue
        after = t[idx + len(n) :].strip()
        head = " ".join(after.split()[:3])
        if head.startswith(KILL_STOP) and len(after.split()) <= 3:
            return Decision(action="stop", forced=True, reason="kill word")
        if any(head.startswith(k) for k in KILL_LEAVE) and len(after.split()) <= 4:
            return Decision(action="leave", forced=True, reason="kill word")
    if _vocative(t, names):
        return Decision(action="speak", forced=True, reason="addressed by name", p_addressed=1.0)
    # Mentioned in passing ("I quoted Arbos in the doc"): the judge decides, with the hint.
    return Decision(action="judge", forced=False, reason="mentioned, not addressed")


def jev_state(text: str, speaker: str, ctx: JudgeContext) -> dict:
    return {
        "agent": {
            "name": ctx.agent_name,
            "role": ctx.agent_role or "the group's AI assistant; it can run commands on its computer, look things up, read and edit files, and post in the chat",
            "also_answers_to": [n for n in names_of(ctx) if n != ctx.agent_name.lower()],
            "last_spoke_seconds_ago": ctx.agent_last_spoke_s_ago,
            "has_work_in_progress": ctx.agent_has_open_work,
        },
        "call": {"kind": "voice call with several people and the agent" if ctx.chat_kind != "dm" else "voice call", "chat_kind": ctx.chat_kind},
        "agent_named_in_utterance": any(n in _norm(text) for n in names_of(ctx)),
        "recent_transcript": ctx.recent[-10:],
        "utterance": {"from": speaker, "text": text[:2000]},
    }


def jev_questions(ctx: JudgeContext) -> dict:
    name = ctx.agent_name
    return {
        "action": {
            "type": "choice",
            "instructions": f"What should the agent \"{name}\" do about `utterance`, said on a live call where it is present and can hear everyone? Judge from `recent_transcript`, from who said it and what it asks. Prefer silent when people are talking to each other.",
            "criteria": {
                "speak": f"`utterance` asks {name} (by name or clearly) a question it can answer from the conversation or general knowledge; asks the group a question that {name} is the right one to answer; states something wrong that {name} should correct and the group clearly needs corrected; answers a question {name} just asked; or asks about work {name} is doing or did.",
                "act": f"`utterance` asks or tells someone — {name} directly, or anyone (\"can someone…\", \"we need…\", \"somebody should…\", \"let's get…\") — to do work that needs a computer or tools: run or check something, look something up, read or change files, post or send something, fetch numbers or logs. {name} is the one on the call who can do it.",
                "silent": f"people are talking to each other; small talk, reactions, acknowledgements, planning among humans; a request clearly for another named person; {name} was only mentioned in passing, quoted, or told not to act; nothing in it is for {name}.",
            },
        },
        "addressed": {
            "type": "noul",
            "instructions": f"Is `utterance` directed at the agent \"{name}\" (explicitly or implicitly) so that {name} should respond?",
            "criteria": {
                "true": f"a request, question or task that {name} should respond to, said to {name} or to anyone who can do it",
                "false": "side talk between people, or a request for another named person",
            },
        },
    }


def apply_threshold(d: Decision, chattiness: str) -> Decision:
    """Turn calibrated probabilities into an action with the owner's chattiness threshold."""
    thr = CHATTINESS_THRESHOLD.get(chattiness, CHATTINESS_THRESHOLD["balanced"])
    p_speak = d.p_speak or 0.0
    p_act = d.p_act or 0.0
    p_silent = d.p_silent if d.p_silent is not None else max(0.0, 1.0 - p_speak - p_act)
    respond = 1.0 - p_silent
    if respond >= thr:
        d.action = "act" if p_act > p_speak else "speak"
    else:
        d.action = "silent"
    d.reason = f"P(respond)={respond:.2f} vs {chattiness} threshold {thr:.2f}"
    return d


class Judge:
    def __init__(self, relay: Relay, project_id: str, model: str, fallback_model: str):
        self.relay = relay
        self.project_id = project_id
        self.model = model
        self.fallback_model = fallback_model

    async def decide(self, text: str, speaker: str, ctx: JudgeContext) -> Decision:
        forced = forced_decision(text, ctx)
        if forced is not None and forced.action in ("stop", "leave", "silent"):
            return forced
        t0 = time.monotonic()
        try:
            d = await self._jev(text, speaker, ctx)
        except Exception as e:
            log.warning("jev failed (%s); falling back to %s", e, self.fallback_model)
            try:
                d = await self._llm(text, speaker, ctx)
            except Exception as e2:
                log.warning("llm judge failed too (%s); staying silent", e2)
                d = Decision(action="silent", provider="none", reason=f"judges failed: {e}; {e2}")
        d.latency_ms = int((time.monotonic() - t0) * 1000)
        if forced is not None and forced.action == "speak":
            # Addressed by name: answer unless the model is very sure it was only a thanks/aside.
            d.forced = True
            if (d.p_silent or 0.0) >= NAMED_SILENT_FLOOR:
                d.action = "silent"
                d.reason = "addressed by name but clearly needs no reply; " + d.reason
            else:
                d.action = "act" if (d.p_act or 0) > (d.p_speak or 0) and (d.p_act or 0) >= 0.3 else "speak"
                d.reason = "addressed by name; " + d.reason
        return d

    async def _jev(self, text: str, speaker: str, ctx: JudgeContext) -> Decision:
        state = jev_state(text, speaker, ctx)
        questions = jev_questions(ctx)
        resp = await self.relay.decide(self.project_id, self.model, state, questions)
        answers = resp.get("answers", {})
        a = answers.get("action", {})
        probs = a.get("probabilities", {}) or {}
        d = Decision(
            action="silent",
            p_speak=float(probs.get("speak", 0.0)),
            p_act=float(probs.get("act", 0.0)),
            p_silent=float(probs.get("silent", 0.0)),
            p_addressed=(answers.get("addressed", {}) or {}).get("noul"),
            provider="jev",
            model=resp.get("model", self.model),
            cost_usd=(resp.get("usage", {}) or {}).get("cost"),
            raw=answers,
        )
        return apply_threshold(d, ctx.chattiness)

    async def _llm(self, text: str, speaker: str, ctx: JudgeContext) -> Decision:
        user = {
            "agent": {"name": ctx.agent_name, "role": ctx.agent_role, "also_answers_to": names_of(ctx)},
            "recent_transcript": ctx.recent[-10:],
            "utterance": {"from": speaker, "text": text[:2000]},
        }
        parsed, usage = await self.relay.chat_json(self.project_id, self.fallback_model, LLM_SYSTEM, str(user))
        action = str(parsed.get("decision", "silent")).lower()
        conf = float(parsed.get("confidence", 0.5) or 0.5)
        if action not in ("speak", "act", "silent"):
            action = "silent"
        d = Decision(
            action=action,
            provider="llm",
            model=self.fallback_model,
            reason=str(parsed.get("reason", ""))[:200],
            cost_usd=usage.get("cost"),
            raw=parsed,
        )
        # Map the LLM's answer onto probabilities so the chattiness threshold applies the same way.
        if action == "silent":
            d.p_silent, d.p_speak, d.p_act = conf, (1 - conf) / 2, (1 - conf) / 2
        elif action == "speak":
            d.p_speak, d.p_silent, d.p_act = conf, 1 - conf, 0.0
        else:
            d.p_act, d.p_silent, d.p_speak = conf, 1 - conf, 0.0
        return apply_threshold(d, ctx.chattiness)
