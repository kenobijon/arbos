"""Gated listening for group calls (spec §3): one cheap listener per human.

Each human's audio track goes through Silero VAD; a finished utterance (end of speech) is sent
as one batch to the transcription model, labelled with the speaker, and handed to the worker.
Nothing reaches the speech model until the judge says so. START_OF_SPEECH is reported at once
for barge-in and turn-taking.
"""

from __future__ import annotations

import asyncio
import logging
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass

from livekit import rtc
from livekit.agents import stt as agents_stt
from livekit.agents import vad as agents_vad

log = logging.getLogger("arbos-voice.listener")

SAMPLE_RATE = 16000
MIN_UTTERANCE_S = 0.35


@dataclass
class Utterance:
    identity: str
    text: str
    speech_s: float
    started_at: float
    ended_at: float
    transcribed_at: float


class ParticipantListener:
    def __init__(
        self,
        participant: rtc.RemoteParticipant,
        track: rtc.Track,
        vad: agents_vad.VAD,
        stt: agents_stt.STT,
        *,
        on_speech_start: Callable[[str, float], None],
        on_speech_end: Callable[[str, float], None],
        on_utterance: Callable[[Utterance], Awaitable[None]],
        on_stt_seconds: Callable[[float], None],
    ):
        self.identity = participant.identity
        self.track = track
        self.vad = vad
        self.stt = stt
        self.on_speech_start = on_speech_start
        self.on_speech_end = on_speech_end
        self.on_utterance = on_utterance
        self.on_stt_seconds = on_stt_seconds
        self.speaking = False
        self._task: asyncio.Task | None = None
        self._pending: set[asyncio.Task] = set()

    def start(self) -> None:
        self._task = asyncio.create_task(self._run(), name=f"listener:{self.identity}")

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except (asyncio.CancelledError, Exception):
                pass
        for t in list(self._pending):
            t.cancel()

    async def _run(self) -> None:
        stream = rtc.AudioStream(self.track, sample_rate=SAMPLE_RATE, num_channels=1)
        vad_stream = self.vad.stream()

        async def pump() -> None:
            async for ev in stream:
                vad_stream.push_frame(ev.frame)
            vad_stream.end_input()

        pump_task = asyncio.create_task(pump())
        speech_started_at = 0.0
        try:
            async for ev in vad_stream:
                now = time.monotonic()
                if ev.type == agents_vad.VADEventType.START_OF_SPEECH:
                    self.speaking = True
                    speech_started_at = now
                    self.on_speech_start(self.identity, now)
                elif ev.type == agents_vad.VADEventType.END_OF_SPEECH:
                    self.speaking = False
                    self.on_speech_end(self.identity, now)
                    if ev.speech_duration < MIN_UTTERANCE_S or not ev.frames:
                        continue
                    frames = list(ev.frames)
                    t = asyncio.create_task(self._transcribe(frames, ev.speech_duration, speech_started_at, now))
                    self._pending.add(t)
                    t.add_done_callback(self._pending.discard)
        finally:
            pump_task.cancel()
            await vad_stream.aclose()
            await stream.aclose()

    async def _transcribe(self, frames: list[rtc.AudioFrame], speech_s: float, started_at: float, ended_at: float) -> None:
        self.on_stt_seconds(speech_s)
        try:
            ev = await self.stt.recognize(frames)
        except Exception as e:
            log.warning("stt failed for %s: %s", self.identity, e)
            return
        text = ""
        if ev.alternatives:
            text = (ev.alternatives[0].text or "").strip()
        if not text:
            return
        await self.on_utterance(
            Utterance(
                identity=self.identity,
                text=text,
                speech_s=speech_s,
                started_at=started_at,
                ended_at=ended_at,
                transcribed_at=time.monotonic(),
            )
        )


class GroupListeners:
    """Listeners for every human in the room; follows joins, leaves and track changes."""

    def __init__(self, room: rtc.Room, vad: agents_vad.VAD, stt: agents_stt.STT, **callbacks):
        self.room = room
        self.vad = vad
        self.stt = stt
        self.callbacks = callbacks
        self.listeners: dict[str, ParticipantListener] = {}
        self._active = False

    def is_human(self, participant: rtc.RemoteParticipant) -> bool:
        return participant.identity.startswith("u_")

    def anyone_speaking(self) -> bool:
        return any(l.speaking for l in self.listeners.values())

    def speaking_identities(self) -> list[str]:
        return [i for i, l in self.listeners.items() if l.speaking]

    async def start(self) -> None:
        self._active = True
        for p in self.room.remote_participants.values():
            self._attach_participant(p)

    async def stop(self) -> None:
        self._active = False
        for l in list(self.listeners.values()):
            await l.stop()
        self.listeners.clear()

    def _attach_participant(self, p: rtc.RemoteParticipant) -> None:
        if not self.is_human(p):
            return
        for pub in p.track_publications.values():
            if pub.track is not None and pub.kind == rtc.TrackKind.KIND_AUDIO:
                self.attach(p, pub.track)

    def attach(self, participant: rtc.RemoteParticipant, track: rtc.Track) -> None:
        if not self._active or not self.is_human(participant) or track.kind != rtc.TrackKind.KIND_AUDIO:
            return
        if participant.identity in self.listeners:
            return
        l = ParticipantListener(participant, track, self.vad, self.stt, **self.callbacks)
        self.listeners[participant.identity] = l
        l.start()
        log.info("listening to %s", participant.identity)

    async def detach(self, identity: str) -> None:
        l = self.listeners.pop(identity, None)
        if l:
            await l.stop()
            log.info("stopped listening to %s", identity)
