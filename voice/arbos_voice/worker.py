"""arbos-voice: the LiveKit Agents worker that is the project agent's voice (Calls Phase 2, D75).

One process serves every call; LiveKit dispatches a job per (call, agent) with the relay's
metadata. The job joins as `a_<project_id>`, speaks with OpenAI `gpt-realtime-2.1`, and:

* **1:1 (duplex)** — the human's audio goes straight to the speech model (server VAD, barge-in
  by the model and by the framework); input/output transcripts become chat rows.
* **group** — nobody's audio reaches the speech model. A per-human listener (Silero VAD →
  `gpt-4o-mini-transcribe` per utterance) produces a speaker-labelled transcript; the judge
  (`judge.py`) decides speak / act / silent per utterance; only then does the model get the text
  and answer with audio. The agent waits for 700 ms of room silence before speaking and is cut
  at once when anyone starts talking.

Tools: one, `ask_agent(request)`, forwarded through the relay to agentd on the owner's machine.
Costs, time to first audio and barge-in cut time are measured here and reported to the relay
every 30 s and at the end; the relay's answer carries the cost stops.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import sys
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

import aiohttp
from livekit import rtc
from livekit.agents import (
    Agent,
    AgentSession,
    AutoSubscribe,
    JobContext,
    JobProcess,
    JobRequest,
    RoomInputOptions,
    RoomOutputOptions,
    RunContext,
    WorkerOptions,
    cli,
    function_tool,
)
from livekit.agents.metrics import RealtimeModelMetrics
from livekit.agents.voice.events import (
    AgentStateChangedEvent,
    ConversationItemAddedEvent,
    MetricsCollectedEvent,
    SpeechCreatedEvent,
    UserInputTranscribedEvent,
    UserStateChangedEvent,
)
from livekit.plugins import openai, silero
from openai.types.realtime.audio_transcription import AudioTranscription
from openai.types.realtime.realtime_audio_input_turn_detection import ServerVad

from . import __version__
from .config import CONFIG, Config
from .judge import CHATTINESS_THRESHOLD, Decision, Judge, JudgeContext, forced_decision
from .listener import GroupListeners, Utterance
from .meter import CallMeter
from .prompts import ASK_AGENT_DESCRIPTION, system_prompt
from .relay import EventBatcher, Relay, RelayError, ToolPending

log = logging.getLogger("arbos-voice")

ATTENTION_WINDOW_S = 20.0
GOODBYE = {
    "call_cap": "I've hit the spending cap for this call, so I'll drop off now. Everything we said is in the chat.",
    "voice_daily_cap": "I've reached today's voice budget, so I'll leave the call. I'm still here in the text chat.",
    "daily_cap": "Today's spending cap is reached, so I'll leave the call. I'm still here in the text chat.",
    "credits": "There are no credits left, so I'll leave the call.",
    "kill_switch": "Arbos is in read-only mode right now, so I'll leave the call.",
    "call_max": "We've been on for an hour, which is my limit per call. I'll drop off now.",
    "idle": "I'll drop off since it's gone quiet. Call me again any time.",
    "asked": "Okay, leaving. Bye.",
    "alone": "",
}


def now_ms() -> int:
    return int(time.time() * 1000)


def speaker_of(participant: rtc.RemoteParticipant | None, identity: str) -> dict:
    """`sender{}` of a human from their LiveKit metadata (set by the relay's join token)."""
    out: dict[str, Any] = {"kind": "user", "id": identity[2:] if identity.startswith("u_") else identity, "handle": "", "name": ""}
    if participant is not None:
        try:
            meta = json.loads(participant.metadata or "{}")
            if isinstance(meta, dict):
                out.update({k: v for k, v in meta.items() if k in ("id", "handle", "name", "avatar_url", "kind") and v})
        except json.JSONDecodeError:
            pass
        if not out.get("name"):
            out["name"] = participant.name or ""
    return out


def label_of(speaker: dict) -> str:
    return f"@{speaker['handle']}" if speaker.get("handle") else (speaker.get("name") or "someone")


@dataclass
class CallState:
    mode: str = "duplex"  # duplex | group
    agent_state: str = "initializing"
    user_state: str = "listening"
    linked_identity: str | None = None
    # Timers for the measurements.
    user_turn_ended_at: float | None = None
    barge_started_at: float | None = None
    pending_ttfa_from: float | None = None
    # Group mode.
    last_asker: dict | None = None
    last_reply_ended_at: float | None = None
    last_trigger_at: float = field(default_factory=time.monotonic)
    last_human_speech_at: float = field(default_factory=time.monotonic)
    window: list[dict] = field(default_factory=list)
    open_work: int = 0
    idle_asked: bool = False
    leaving: bool = False
    stop_reason: str | None = None


class ArbosVoiceAgent(Agent):
    def __init__(self, call: VoiceCall, instructions: str):
        super().__init__(instructions=instructions, tools=[call.make_tool()])
        self.call = call


class VoiceCall:
    def __init__(self, ctx: JobContext, cfg: Config, meta: dict, http: aiohttp.ClientSession):
        self.ctx = ctx
        self.cfg = cfg
        self.meta = meta
        self.call_id: str = meta["call_id"]
        self.project_id: str = meta["project_id"]
        self.chat_id: str = meta["chat_id"]
        self.agent_sender: dict = meta.get("agent") or {}
        self.agent_name: str = self.agent_sender.get("name") or "Arbos"
        self.chattiness: str = meta.get("chattiness") or "balanced"
        self.wake_name: str | None = meta.get("wake_name")
        self.relay = Relay(cfg, http)
        self.events = EventBatcher(self.relay, self.call_id, self.project_id)
        self.meter = CallMeter(model=cfg.model, stt_model=cfg.stt_model)
        self.judge = Judge(self.relay, self.project_id, cfg.judge_model, cfg.judge_fallback_model)
        self.state = CallState()
        self.info: dict = {}
        self.session: AgentSession | None = None
        self.agent: ArbosVoiceAgent | None = None
        self.listeners: GroupListeners | None = None
        self.rt_model: openai.realtime.RealtimeModel | None = None
        self._done = asyncio.Event()
        self._tasks: set[asyncio.Task] = set()
        self._speak_lock = asyncio.Lock()
        self._pending_tools: set[asyncio.Task] = set()

    # ---- helpers -----------------------------------------------------------------------------------

    def spawn(self, coro) -> asyncio.Task:
        t = asyncio.create_task(coro)
        self._tasks.add(t)
        t.add_done_callback(self._tasks.discard)
        return t

    def humans(self) -> list[rtc.RemoteParticipant]:
        return [p for p in self.ctx.room.remote_participants.values() if p.identity.startswith("u_")]

    def participant(self, identity: str) -> rtc.RemoteParticipant | None:
        return self.ctx.room.remote_participants.get(identity)

    def event(self, kind: str, **fields) -> None:
        self.events.push({"kind": kind, **fields})

    def transcript(self, speaker: dict | None, text: str, *, truncated: bool = False) -> None:
        self.event("transcript", speaker=speaker if speaker else {"kind": "agent"}, text=text, truncated=truncated)

    def agent_speaking(self) -> bool:
        return self.state.agent_state == "speaking"

    # ---- lifecycle ---------------------------------------------------------------------------------

    async def run(self) -> None:
        ctx = self.ctx
        await ctx.connect(auto_subscribe=AutoSubscribe.SUBSCRIBE_ALL)
        log.info("joined room %s as %s for call %s (project %s)", ctx.room.name, ctx.room.local_participant.identity, self.call_id, self.project_id)
        try:
            self.info = await self.relay.call_info(self.call_id, self.project_id)
        except Exception as e:
            log.error("call info failed: %s", e)
            self.info = {}
        caps = (self.info.get("caps") or {})
        if caps.get("stop"):
            log.warning("cost stop %s before joining; leaving", caps["stop"])
            self.event("left", reason=caps["stop"])
            await self.events.close()
            return
        settings = self.info.get("settings") or {}
        self.chattiness = settings.get("chattiness") or self.chattiness
        self.wake_name = settings.get("wake_name") or self.wake_name
        self.events.start()
        self.event("joined", mode=self.state.mode, version=__version__)

        # Wait for a human if the room is empty at dispatch (the caller is still connecting).
        if not self.humans():
            try:
                await asyncio.wait_for(self._wait_for_human(), timeout=45.0)
            except asyncio.TimeoutError:
                log.info("nobody joined within 45 s; leaving")
                await self.finish("alone")
                return

        self.state.mode = "group" if len(self.humans()) >= 2 else "duplex"
        instructions = self.instructions()
        self.agent = ArbosVoiceAgent(self, instructions)
        self.rt_model = openai.realtime.RealtimeModel(
            model=self.cfg.model,
            voice=self.cfg.voice,
            api_key=self.cfg.openai_api_key,
            turn_detection=self.turn_detection_for(self.state.mode),
            input_audio_transcription=AudioTranscription(model=self.cfg.stt_model),
        )
        self.session = AgentSession(
            llm=self.rt_model,
            allow_interruptions=True,
            turn_detection="realtime_llm" if self.state.mode == "duplex" else "manual",
            user_away_timeout=None,
        )
        self.hook_session(self.session)
        self.hook_room()
        linked = self.humans()[0].identity if self.state.mode == "duplex" else None
        self.state.linked_identity = linked
        input_opts: dict[str, Any] = {
            "audio_enabled": self.state.mode == "duplex",
            "text_enabled": False,
            "close_on_disconnect": False,
        }
        if linked:
            input_opts["participant_identity"] = linked
        await self.session.start(
            self.agent,
            room=ctx.room,
            room_input_options=RoomInputOptions(**input_opts),
            room_output_options=RoomOutputOptions(transcription_enabled=True, audio_enabled=True),
        )
        if self.state.mode == "group":
            await self.start_group_listening()
        self.hook_realtime()
        log.info("session started in %s mode (humans=%d, chattiness=%s)", self.state.mode, len(self.humans()), self.chattiness)
        self.spawn(self.usage_loop())
        self.spawn(self.idle_loop())
        await self.greet()
        await self._done.wait()

    async def _wait_for_human(self) -> None:
        while not self.humans():
            await asyncio.sleep(0.2)

    def instructions(self) -> str:
        ctxpack = (self.info.get("context") or {}) if isinstance(self.info.get("context"), dict) else {}
        extra = ctxpack.get("system") or ""
        if not extra:
            p = self.info.get("project") or {}
            bits = []
            if p.get("description"):
                bits.append(p["description"])
            if p.get("instructions"):
                bits.append("Owner instructions:\n" + str(p["instructions"])[:4000])
            extra = "\n\n".join(bits)
        recent = ctxpack.get("recent") or []
        if recent:
            extra += "\n\n# Last lines of this chat\n" + "\n".join(f"- {r.get('from')}: {r.get('text')}" for r in recent[-12:])
            self.state.window = [{"from": r.get("from"), "text": r.get("text")} for r in recent[-8:]]
        return system_prompt(
            name=self.agent_name,
            group=self.state.mode == "group",
            extra=extra,
            people=self.info.get("humans") or [],
        )

    def turn_detection_for(self, mode: str):
        if mode == "duplex":
            return ServerVad(
                type="server_vad",
                threshold=0.5,
                prefix_padding_ms=200,
                silence_duration_ms=350,
                create_response=True,
                interrupt_response=True,
            )
        return None

    async def greet(self) -> None:
        assert self.session
        humans = self.info.get("humans") or []
        owner = next((h for h in humans if h.get("role") == "owner"), None)
        owner_label = f"@{owner['handle']}" if owner and owner.get("handle") else "the owner"
        if self.state.mode == "group":
            line = f"{self.agent_name}, {owner_label}'s agent, has joined and can hear this call."
            self.event("system", text=line)
            await self.wait_for_silence()
            self.session.say(line, allow_interruptions=True)
        else:
            self.event("system", text=f"{self.agent_name} joined the call")
            self.session.generate_reply(
                instructions="Greet the person in one short sentence (say hi and ask what they need). No more than twelve words."
            )

    async def finish(self, reason: str) -> None:
        if self.state.leaving:
            return
        self.state.leaving = True
        self.state.stop_reason = reason
        self.meter.stop_reason = reason
        log.info("leaving call %s: %s", self.call_id, reason)
        try:
            if self.session and reason != "alone" and GOODBYE.get(reason):
                handle = self.session.say(GOODBYE[reason], allow_interruptions=False)
                try:
                    await asyncio.wait_for(handle.wait_for_playout(), timeout=12.0)
                except asyncio.TimeoutError:
                    pass
        except Exception as e:
            log.warning("goodbye failed: %s", e)
        self.event("system", text=f"{self.agent_name} left the call" + ("" if reason in ("alone", "asked", "idle") else f" · {reason.replace('_', ' ')}"))
        self.event("left", reason=reason)
        if self.listeners:
            await self.listeners.stop()
        try:
            await self.relay.usage(self.call_id, self.project_id, self.meter.usage(ended=True))
        except Exception as e:
            log.warning("final usage post failed: %s", e)
        await self.events.close()
        log.info("call %s summary: %s", self.call_id, json.dumps(self.meter.summary()))
        if self.session:
            try:
                await asyncio.wait_for(self.session.aclose(), timeout=5.0)
            except Exception:
                pass
        for t in list(self._tasks):
            t.cancel()
        self._done.set()
        self.ctx.shutdown(reason=reason)

    # ---- session events ------------------------------------------------------------------------------

    def hook_session(self, session: AgentSession) -> None:
        @session.on("agent_state_changed")
        def _agent_state(ev: AgentStateChangedEvent) -> None:
            now = time.monotonic()
            prev = self.state.agent_state
            self.state.agent_state = ev.new_state
            if ev.new_state == "speaking":
                src = self.state.pending_ttfa_from
                if src is not None:
                    ttfa = int((now - src) * 1000)
                    self.meter.ttfa_ms.append(ttfa)
                    self.state.pending_ttfa_from = None
                    self.event("metric", ttfa_ms=ttfa, mode=self.state.mode)
                    log.info("TTFA %d ms (%s)", ttfa, self.state.mode)
            if prev == "speaking" and ev.new_state != "speaking":
                self.state.last_reply_ended_at = now
                if self.state.barge_started_at is not None:
                    cut = int((now - self.state.barge_started_at) * 1000)
                    self.meter.barge_ms.append(cut)
                    self.state.barge_started_at = None
                    self.event("metric", barge_ms=cut, mode=self.state.mode)
                    log.info("barge-in cut %d ms (%s)", cut, self.state.mode)

        @session.on("user_state_changed")
        def _user_state(ev: UserStateChangedEvent) -> None:
            now = time.monotonic()
            self.state.user_state = ev.new_state
            if self.state.mode != "duplex":
                return
            if ev.new_state == "speaking":
                self.state.last_human_speech_at = now
                self.state.idle_asked = False
                if self.agent_speaking() and self.state.barge_started_at is None:
                    self.state.barge_started_at = now
            elif ev.old_state == "speaking" and ev.new_state == "listening":
                self.state.user_turn_ended_at = now
                self.state.pending_ttfa_from = now

        @session.on("user_input_transcribed")
        def _user_transcribed(ev: UserInputTranscribedEvent) -> None:
            if self.state.mode != "duplex" or not ev.is_final:
                return
            text = (ev.transcript or "").strip()
            if not text:
                return
            identity = self.state.linked_identity or ""
            speaker = speaker_of(self.participant(identity), identity)
            self.transcript(speaker, text)
            self.state.last_asker = speaker
            self.state.window.append({"from": label_of(speaker), "text": text})
            self.state.window = self.state.window[-12:]
            fd = forced_decision(text, self.judge_ctx())
            if fd is not None and fd.action in ("stop", "leave"):
                self.spawn(self.kill_word(fd.action))

        @session.on("conversation_item_added")
        def _item_added(ev: ConversationItemAddedEvent) -> None:
            item = ev.item
            if getattr(item, "role", None) != "assistant":
                return
            text = (item.text_content or "").strip()
            if not text:
                return
            self.transcript(None, text, truncated=bool(getattr(item, "interrupted", False)))
            self.state.window.append({"from": f"{self.agent_name} (agent)", "text": text})
            self.state.window = self.state.window[-12:]

        @session.on("speech_created")
        def _speech(ev: SpeechCreatedEvent) -> None:
            self.meter.turns_spoken += 1

        @session.on("metrics_collected")
        def _metrics(ev: MetricsCollectedEvent) -> None:
            m = ev.metrics
            if isinstance(m, RealtimeModelMetrics):
                self.meter.add_realtime(
                    input_tokens=m.input_tokens,
                    output_tokens=m.output_tokens,
                    details_in=m.input_token_details,
                    details_out=m.output_token_details,
                    ttft_s=m.ttft,
                )

        @session.on("error")
        def _error(ev) -> None:
            log.warning("session error: %s", getattr(ev, "error", ev))

    def hook_realtime(self) -> None:
        """Wall-clock stamps of the speech model's own VAD, for the barge-in measurement."""
        try:
            rt = self.agent.realtime_llm_session if self.agent else None
        except Exception:
            rt = None
        if rt is None:
            return

        @rt.on("input_speech_started")
        def _started(_ev) -> None:
            log.info("rt input_speech_started wall=%d agent_state=%s", now_ms(), self.state.agent_state)

        @rt.on("input_speech_stopped")
        def _stopped(_ev) -> None:
            log.info("rt input_speech_stopped wall=%d", now_ms())

    def hook_room(self) -> None:
        room = self.ctx.room

        @room.on("participant_connected")
        def _joined(p: rtc.RemoteParticipant) -> None:
            if p.identity.startswith("u_"):
                log.info("human joined: %s", p.identity)
                self.spawn(self.on_humans_changed())

        @room.on("participant_disconnected")
        def _left(p: rtc.RemoteParticipant) -> None:
            if p.identity.startswith("u_"):
                log.info("human left: %s", p.identity)
                if self.listeners:
                    self.spawn(self.listeners.detach(p.identity))
                self.spawn(self.on_humans_changed())

        @room.on("track_subscribed")
        def _track(track: rtc.Track, pub: rtc.RemoteTrackPublication, p: rtc.RemoteParticipant) -> None:
            if self.listeners and self.state.mode == "group":
                self.listeners.attach(p, track)

        @room.on("disconnected")
        def _disconnected(*_a) -> None:
            log.info("room disconnected")
            self.spawn(self.finish(self.state.stop_reason or "room_closed"))

    async def on_humans_changed(self) -> None:
        await asyncio.sleep(0.3)
        humans = self.humans()
        if not humans:
            await asyncio.sleep(10.0)
            if not self.humans() and not self.state.leaving:
                await self.finish("alone")
            return
        want = "group" if len(humans) >= 2 else "duplex"
        if want != self.state.mode:
            await self.switch_mode(want)
        elif want == "duplex" and self.state.linked_identity != humans[0].identity and self.session:
            self.state.linked_identity = humans[0].identity
            self.session.room_io.set_participant(humans[0].identity)

    async def switch_mode(self, mode: str) -> None:
        assert self.session and self.agent and self.rt_model
        log.info("mode %s → %s", self.state.mode, mode)
        self.state.mode = mode
        self.meter.mode = mode if self.meter.mode == mode else "mixed"
        rt = self.agent.realtime_llm_session
        if mode == "group":
            self.session.input.set_audio_enabled(False)
            if rt:
                rt.update_options(turn_detection=None)
            self.session.update_options(turn_detection="manual")
            self.state.linked_identity = None
            await self.start_group_listening()
            await self.agent.update_instructions(self.instructions())
            line = f"{self.agent_name} can hear everyone on this call now."
            self.event("system", text=line)
        else:
            if self.listeners:
                await self.listeners.stop()
                self.listeners = None
            human = self.humans()[0]
            self.state.linked_identity = human.identity
            self.session.room_io.set_participant(human.identity)
            if rt:
                rt.update_options(turn_detection=self.turn_detection_for("duplex"))
            self.session.update_options(turn_detection="realtime_llm")
            self.session.input.set_audio_enabled(True)
            await self.agent.update_instructions(self.instructions())
        self.event("metric", mode_switch=mode)

    # ---- group mode ---------------------------------------------------------------------------------

    async def start_group_listening(self) -> None:
        if self.listeners:
            return
        vad = self.ctx.proc.userdata.get("vad") or silero.VAD.load()
        stt = openai.STT(model=self.cfg.stt_model, api_key=self.cfg.openai_api_key, use_realtime=False)
        self.listeners = GroupListeners(
            self.ctx.room,
            vad,
            stt,
            on_speech_start=self.on_group_speech_start,
            on_speech_end=self.on_group_speech_end,
            on_utterance=self.on_group_utterance,
            on_stt_seconds=self.meter.add_stt,
        )
        await self.listeners.start()

    def on_group_speech_start(self, identity: str, at: float) -> None:
        self.state.last_human_speech_at = at
        self.state.idle_asked = False
        if self.agent_speaking() and self.session:
            if self.state.barge_started_at is None:
                self.state.barge_started_at = at
            log.info("barge-in by %s", identity)
            self.session.interrupt()

    def on_group_speech_end(self, identity: str, at: float) -> None:
        pass

    def judge_ctx(self) -> JudgeContext:
        last = self.state.last_reply_ended_at
        return JudgeContext(
            agent_name=self.agent_name,
            agent_role=(self.info.get("project") or {}).get("description") or "",
            chat_kind=(self.info.get("chat") or {}).get("kind") or "group",
            wake_name=self.wake_name,
            chattiness=self.chattiness,
            recent=self.state.window[-10:],
            agent_last_spoke_s_ago=(time.monotonic() - last) if last else None,
            agent_has_open_work=self.state.open_work > 0,
        )

    async def on_group_utterance(self, u: Utterance) -> None:
        speaker = speaker_of(self.participant(u.identity), u.identity)
        label = label_of(speaker)
        self.transcript(speaker, u.text)
        ctx = self.judge_ctx()
        decision = await self.judge.decide(u.text, label, ctx)
        self.meter.add_decision(decision.cost_usd)
        # Attention window: the person the agent just answered gets the benefit of the doubt.
        in_window = (
            self.state.last_asker is not None
            and self.state.last_asker.get("id") == speaker.get("id")
            and self.state.last_reply_ended_at is not None
            and time.monotonic() - self.state.last_reply_ended_at <= ATTENTION_WINDOW_S
        )
        if decision.action == "silent" and in_window and decision.p_silent is not None and not decision.forced:
            thr = CHATTINESS_THRESHOLD.get(self.chattiness, 0.6) - 0.2
            if 1.0 - decision.p_silent >= thr:
                decision.action = "act" if (decision.p_act or 0) > (decision.p_speak or 0) else "speak"
                decision.reason = "attention window; " + decision.reason
        self.event("decision", **{k: v for k, v in decision.as_event(label, u.text).items() if k != "kind"})
        log.info(
            "judge %s → %s (speak=%.2f act=%.2f silent=%.2f, %s, %d ms) «%s»",
            label, decision.action, decision.p_speak or 0, decision.p_act or 0, decision.p_silent or 0,
            decision.provider, decision.latency_ms, u.text[:80],
        )
        self.state.window.append({"from": label, "text": u.text})
        self.state.window = self.state.window[-12:]
        if decision.action in ("stop", "leave"):
            await self.kill_word(decision.action)
            return
        if decision.action == "silent":
            return
        self.state.last_asker = speaker
        self.state.last_trigger_at = time.monotonic()
        await self.respond_group(u, speaker, decision)

    async def wait_for_silence(self, max_wait: float = 15.0) -> None:
        """Turn-taking: never start while a human is talking; wait for `group_silence_s` of quiet."""
        if not self.listeners:
            return
        deadline = time.monotonic() + max_wait
        while time.monotonic() < deadline:
            quiet_for = time.monotonic() - self.state.last_human_speech_at
            if not self.listeners.anyone_speaking() and quiet_for >= self.cfg.group_silence_s:
                return
            await asyncio.sleep(0.05)

    async def respond_group(self, u: Utterance, speaker: dict, decision: Decision) -> None:
        assert self.session
        async with self._speak_lock:
            await self.wait_for_silence()
            label = label_of(speaker)
            lines = "\n".join(f"{w['from']}: {w['text']}" for w in self.state.window[-8:-1])
            user_input = (f"Recent lines on the call:\n{lines}\n\n" if lines else "") + f"{label} just said: {u.text}"
            if decision.action == "act":
                instr = (
                    f"{label} asked for work that needs your tools. Say one very short line like 'On it' addressed to {label}, "
                    f"then call ask_agent with their request (as they asked it, including who asked). Keep it under ten words before the tool."
                )
            else:
                instr = f"Answer {label} out loud in one or two short sentences. If the answer needs the computer, files or recent data, use ask_agent instead."
            self.state.pending_ttfa_from = u.ended_at
            self.session.generate_reply(user_input=user_input, instructions=instr)

    async def kill_word(self, action: str) -> None:
        assert self.session
        if action == "stop":
            log.info("kill word: stop")
            self.session.interrupt()
            for t in list(self._pending_tools):
                t.cancel()
            self.event("system", text=f"{self.agent_name} was told to stop")
        else:
            await self.finish("asked")

    # ---- tool ----------------------------------------------------------------------------------------

    def make_tool(self):
        call = self

        @function_tool(name="ask_agent", description=ASK_AGENT_DESCRIPTION)
        async def ask_agent(context: RunContext, request: str) -> str:
            return await call.ask_agent(request)

        return ask_agent

    async def ask_agent(self, request: str) -> str:
        speaker = self.state.last_asker or (
            speaker_of(self.participant(self.state.linked_identity or ""), self.state.linked_identity or "") if self.state.linked_identity else {}
        )
        self.meter.tool_calls += 1
        self.state.open_work += 1
        request_id = uuid.uuid4().hex[:12]
        self.event("tool", name="ask_agent", status="started", request=request[:500], request_id=request_id, speaker=label_of(speaker) if speaker else None)
        log.info("ask_agent[%s] from %s: %s", request_id, label_of(speaker) if speaker else "?", request[:120])
        t0 = time.monotonic()

        async def run() -> dict:
            return await self.relay.tool(
                self.call_id, self.project_id, speaker=speaker, request=request, mode=self.state.mode, request_id=request_id
            )

        task = asyncio.create_task(run())
        self._pending_tools.add(task)
        task.add_done_callback(self._pending_tools.discard)
        try:
            result = await asyncio.wait_for(asyncio.shield(task), timeout=self.cfg.tool_quick_wait_s)
        except asyncio.TimeoutError:
            self.spawn(self.deliver_later(task, request_id, speaker))
            return "The agent is working on it; this may take a while. Tell the person you will report back when it is done, then stop talking."
        except asyncio.CancelledError:
            self.state.open_work -= 1
            return "Cancelled."
        except ToolPending:
            self.state.open_work -= 1
            return "The agent is still working on it and will post the result in the chat when it is done."
        except RelayError as e:
            self.state.open_work -= 1
            self.event("tool", name="ask_agent", status="failed", request_id=request_id, error=str(e.body)[:300], ms=int((time.monotonic() - t0) * 1000))
            if e.status == 503:
                return "I can't reach my computer right now, so I can't do that. Say so briefly."
            if e.status == 402:
                return "Spending is capped right now, so I can't run that. Say so briefly."
            return f"The request failed: {str(e.body)[:200]}. Say so briefly."
        except Exception as e:
            self.state.open_work -= 1
            return f"The request failed: {e}. Say so briefly."
        self.state.open_work -= 1
        ms = int((time.monotonic() - t0) * 1000)
        self.event("tool", name="ask_agent", status="done" if result.get("ok") else "failed", request_id=request_id, ms=ms, message_id=result.get("message_id"))
        if not result.get("ok"):
            return f"The agent could not do it: {result.get('error') or 'unknown error'}. Say so briefly."
        text = (result.get("text") or "").strip()
        log.info("ask_agent[%s] done in %d ms: %s", request_id, ms, text[:120])
        return f"Result from the agent (summarise it in one or two spoken sentences; the full text is already in the chat):\n{text[:3000]}"

    async def deliver_later(self, task: asyncio.Task, request_id: str, speaker: dict) -> None:
        try:
            result = await task
        except ToolPending:
            self.state.open_work -= 1
            return
        except Exception as e:
            self.state.open_work -= 1
            log.warning("deferred tool %s failed: %s", request_id, e)
            return
        self.state.open_work -= 1
        if self.state.leaving or not self.session:
            return
        text = (result.get("text") or "").strip() if result.get("ok") else f"it failed: {result.get('error')}"
        self.event("tool", name="ask_agent", status="done" if result.get("ok") else "failed", request_id=request_id, message_id=result.get("message_id"), deferred=True)
        label = label_of(speaker) if speaker else "them"
        async with self._speak_lock:
            await self.wait_for_silence()
            self.session.generate_reply(
                instructions=f"Your earlier task for {label} finished. Tell {label} what matters in one or two spoken sentences (the full text is in the chat). Result:\n{text[:3000]}"
            )

    # ---- loops ----------------------------------------------------------------------------------------

    async def usage_loop(self) -> None:
        while not self.state.leaving:
            await asyncio.sleep(self.cfg.usage_every_s)
            if self.state.leaving:
                return
            try:
                resp = await self.relay.usage(self.call_id, self.project_id, self.meter.usage())
            except Exception as e:
                log.warning("usage post failed: %s", e)
                continue
            stop = (resp.get("caps") or {}).get("stop")
            if stop:
                await self.finish(stop)
                return
            if self.meter.seconds() >= self.cfg.call_max_s:
                await self.finish("call_max")
                return

    async def idle_loop(self) -> None:
        while not self.state.leaving:
            await asyncio.sleep(5.0)
            if self.state.leaving or not self.session:
                return
            now = time.monotonic()
            if self.state.mode == "duplex":
                quiet = now - self.state.last_human_speech_at
                if quiet >= self.cfg.duplex_idle_leave_s:
                    await self.finish("idle")
                    return
                if quiet >= self.cfg.duplex_idle_ask_s and not self.state.idle_asked:
                    self.state.idle_asked = True
                    self.session.generate_reply(instructions="It has been quiet for a while. Ask in four words or fewer whether they are still there.")
            else:
                if now - self.state.last_trigger_at >= self.cfg.group_idle_leave_s:
                    await self.finish("idle")
                    return


# ---- worker -----------------------------------------------------------------------------------------


def prewarm(proc: JobProcess) -> None:
    proc.userdata["vad"] = silero.VAD.load(min_silence_duration=0.5, activation_threshold=0.5)


async def request_fnc(req: JobRequest) -> None:
    """Accept the dispatch as `a_<project_id>` with the agent's `sender{}` as metadata."""
    try:
        meta = json.loads(req.job.metadata or "{}")
    except json.JSONDecodeError:
        meta = {}
    identity = meta.get("identity") or f"a_{meta.get('project_id', 'agent')}"
    agent = meta.get("agent") or {}
    await req.accept(
        name=agent.get("name") or "Arbos",
        identity=identity,
        metadata=json.dumps(agent) if agent else "",
        attributes={"arbos.kind": "agent", "arbos.call_id": str(meta.get("call_id", "")), "arbos.project_id": str(meta.get("project_id", ""))},
    )


async def entrypoint(ctx: JobContext) -> None:
    try:
        meta = json.loads(ctx.job.metadata or "{}")
    except json.JSONDecodeError:
        meta = {}
    if not meta.get("call_id") or not meta.get("project_id") or not meta.get("chat_id"):
        log.error("dispatch without call metadata: %r", ctx.job.metadata)
        return
    ctx.log_context_fields = {"call_id": meta["call_id"], "project_id": meta["project_id"]}
    async with aiohttp.ClientSession() as http:
        call = VoiceCall(ctx, CONFIG, meta, http)
        ctx.add_shutdown_callback(lambda: call.finish(call.state.stop_reason or "shutdown"))
        await call.run()


def main() -> None:
    logging.basicConfig(
        level=os.environ.get("ARBOS_VOICE_LOG_LEVEL", "INFO"),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        stream=sys.stdout,
    )
    missing = CONFIG.check()
    if missing:
        log.error("missing configuration: %s", ", ".join(missing))
        raise SystemExit(2)
    cli.run_app(
        WorkerOptions(
            entrypoint_fnc=entrypoint,
            request_fnc=request_fnc,
            prewarm_fnc=prewarm,
            agent_name=CONFIG.agent_name,
            ws_url=CONFIG.livekit_url,
            api_key=CONFIG.livekit_api_key,
            api_secret=CONFIG.livekit_api_secret,
            num_idle_processes=1,
            load_threshold=0.9,
            host="127.0.0.1",
        )
    )


if __name__ == "__main__":
    main()
