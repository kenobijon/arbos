"""A simulated human on an Arbos call: joins the LiveKit room with a relay call token, speaks
scripted lines (OpenAI TTS), listens to the agent, and measures.

    source .venv/bin/activate
    python tests/sim_participant.py --relay https://relay.arbos.life --token <device access token> \
        --chat <chat_id> [--project <project_id>] --script tests/scripts/duplex.json --out /tmp/sim-run

Script = JSON list of steps, run in order:
    {"say": "text"}                       speak the line (TTS), then wait until the agent has
                                          answered and gone quiet (TTFA is measured from the end
                                          of our audio to the agent's first audio)
    {"say": "text", "wait": false}        speak and move on at once
    {"barge": "text", "ask": "q", "after_ms": 900}
                                          say `ask` (something with a long answer), wait for the
                                          agent to start speaking, let it talk for `after_ms`,
                                          then speak over it; measures how fast its audio stops
    {"sleep": 2.5}                        say nothing for a while
    {"wait_quiet": 20}                    wait (max s) until the agent has been silent for 1.5 s
    {"hangup": true}                      leave the call (POST /calls/{id}/leave)
    {"camera": "photo.jpg"}               turn the camera on (or switch the picture): publishes a
                                          640x480 @ 10 fps video track showing the image, like a
                                          phone pointed at it (`--camera` sets it from the start)
    {"camera_off": true}                  mute the camera (frames stop; the agent's "seeing" clears)

Output: `<out>/agent.wav` (what the agent said), `<out>/mix.wav` (both sides, for the recording),
`<out>/events.jsonl` (timestamps), and a summary on stdout. Several participants can run at once
against the same chat with different device tokens (group calls).
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import logging
import os
import sys
import time
import wave
from pathlib import Path

import aiohttp
import numpy as np
from livekit import rtc
from PIL import Image

log = logging.getLogger("sim")

SAMPLE_RATE = 24000
FRAME_MS = 20
FRAME_SAMPLES = SAMPLE_RATE * FRAME_MS // 1000
SPEECH_RMS = 350  # int16 RMS above which the agent "is speaking"
QUIET_S = 1.2


def voiced_bounds(pcm: np.ndarray, thresh: float = 300.0) -> tuple[float, float]:
    """(first, last) voiced second of a clip, so timings start at the first word, not the file."""
    n = max(1, len(pcm) // FRAME_SAMPLES)
    rms = [float(np.sqrt(np.mean(pcm[i * FRAME_SAMPLES : (i + 1) * FRAME_SAMPLES].astype(np.float32) ** 2))) for i in range(n)]
    voiced = [i for i, r in enumerate(rms) if r > thresh]
    if not voiced:
        return 0.0, len(pcm) / SAMPLE_RATE
    return voiced[0] * FRAME_MS / 1000, (voiced[-1] + 1) * FRAME_MS / 1000


def wav_write(path: Path, pcm: np.ndarray, rate: int = SAMPLE_RATE) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.astype("<i2").tobytes())


async def tts(text: str, voice: str, cache: Path, api_key: str) -> np.ndarray:
    """OpenAI TTS → 24 kHz mono int16 (cached by text+voice)."""
    key = hashlib.sha1(f"{voice}:{text}".encode()).hexdigest()[:16]
    f = cache / f"{key}.pcm"
    if f.exists():
        return np.frombuffer(f.read_bytes(), dtype="<i2")
    async with aiohttp.ClientSession() as s:
        async with s.post(
            "https://api.openai.com/v1/audio/speech",
            headers={"authorization": f"Bearer {api_key}"},
            json={"model": "gpt-4o-mini-tts", "voice": voice, "input": text, "response_format": "pcm"},
        ) as r:
            r.raise_for_status()
            data = await r.read()
    cache.mkdir(parents=True, exist_ok=True)
    f.write_bytes(data)
    return np.frombuffer(data, dtype="<i2")


class Participant:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.room: rtc.Room | None = None
        self.source: rtc.AudioSource | None = None
        self.track: rtc.LocalAudioTrack | None = None
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.events = open(self.out / "events.jsonl", "a")
        self.agent_pcm: list[np.ndarray] = []
        self.mix: list[tuple[float, np.ndarray, str]] = []
        self.t0 = time.monotonic()
        self.agent_speaking = False
        self.agent_last_audio_at = 0.0
        self.agent_started_at = 0.0
        self.our_speech_ended_at: float | None = None
        self.our_speech_onset_at: float | None = None
        # Early cut-ins: the agent starts talking before our utterance's last voiced frame.
        self.saying_until: float | None = None
        self.early_cut_in_this_turn = False
        self.early_cut_ins = 0
        self.turns = 0
        self.ttfa_ms: list[int] = []
        self.barge_ms: list[int] = []
        self.call: dict = {}
        self._agent_identity: str | None = None
        self.cam_source: rtc.VideoSource | None = None
        self.cam_track: rtc.LocalVideoTrack | None = None
        self.cam_pub: rtc.LocalTrackPublication | None = None
        self.cam_rgba: bytes | None = None
        self.cam_task: asyncio.Task | None = None
        self.seen_by_agent: bool | None = None

    def now(self) -> float:
        return time.monotonic() - self.t0

    def event(self, kind: str, **f) -> None:
        rec = {"t": round(self.now(), 3), "wall": int(time.time() * 1000), "kind": kind, **f}
        self.events.write(json.dumps(rec) + "\n")
        self.events.flush()
        log.info("%s %s", kind, {k: v for k, v in f.items() if k != "text"} | ({"text": f["text"][:60]} if "text" in f else {}))

    async def start_call(self, http: aiohttp.ClientSession) -> dict:
        a = self.args
        h = {"authorization": f"Bearer {a.token}", "content-type": "application/json", "X-Arbos-Client": "sim/0.1"}
        if a.join:
            async with http.post(f"{a.relay}/calls/{a.join}/join", headers=h) as r:
                r.raise_for_status()
                d = await r.json()
                self.event("joined_call", call_id=a.join)
                return d["call"]
        path = f"/projects/{a.project}/chats/{a.chat}/calls" if a.project and a.project != "people" else f"/chats/{a.chat}/calls"
        async with http.post(f"{a.relay}{path}", headers=h, json={"kind": "voice"}) as r:
            r.raise_for_status()
            d = await r.json()
            self.event("call", status=r.status, call_id=d["call"]["call_id"], agents=[x.get("name") for x in d["call"].get("agents", [])])
            return d["call"]

    async def leave(self, http: aiohttp.ClientSession) -> None:
        h = {"authorization": f"Bearer {self.args.token}"}
        async with http.post(f"{self.args.relay}/calls/{self.call['call_id']}/leave", headers=h) as r:
            d = await r.json()
            self.event("left_call", **d)

    def on_track(self, track: rtc.Track, pub: rtc.RemoteTrackPublication, p: rtc.RemoteParticipant) -> None:
        if track.kind != rtc.TrackKind.KIND_AUDIO:
            return
        self.event("track", identity=p.identity, name=p.name)
        asyncio.create_task(self.pump(track, p.identity))

    async def pump(self, track: rtc.Track, identity: str) -> None:
        stream = rtc.AudioStream(track, sample_rate=SAMPLE_RATE, num_channels=1)
        is_agent = identity.startswith("a_")
        async for ev in stream:
            frame = ev.frame
            pcm = np.frombuffer(frame.data, dtype="<i2")
            if not is_agent:
                self.mix.append((self.now(), pcm.copy(), identity))
                continue
            self.agent_pcm.append(pcm.copy())
            self.mix.append((self.now(), pcm.copy(), identity))
            rms = float(np.sqrt(np.mean(pcm.astype(np.float32) ** 2))) if len(pcm) else 0.0
            now = self.now()
            if rms > SPEECH_RMS:
                self.agent_last_audio_at = now
                if not self.agent_speaking:
                    self.agent_speaking = True
                    self.agent_started_at = now
                    extra = {}
                    if self.saying_until is not None and now < self.saying_until - 0.15 and not self.early_cut_in_this_turn:
                        self.early_cut_in_this_turn = True
                        self.early_cut_ins += 1
                        extra["early_cut_in"] = round(self.saying_until - now, 2)
                    if self.our_speech_ended_at is not None:
                        ttfa = int((now - self.our_speech_ended_at) * 1000)
                        self.ttfa_ms.append(ttfa)
                        extra["ttfa_ms"] = ttfa
                        self.our_speech_ended_at = None
                    self.event("agent_speaking", **extra)
            elif self.agent_speaking and now - self.agent_last_audio_at > 0.35:
                self.agent_speaking = False
                self.event("agent_quiet", spoke_s=round(self.agent_last_audio_at - self.agent_started_at, 2))

    # ---- camera -----------------------------------------------------------------------------------

    CAM_W, CAM_H, CAM_FPS = 640, 480, 10

    def on_data(self, pkt: rtc.DataPacket) -> None:
        if pkt.topic != "agent_vision":
            return
        try:
            msg = json.loads(pkt.data.decode())
        except Exception:
            return
        me = self.room.local_participant.identity if self.room else ""
        if msg.get("participant_identity") == me:
            self.seen_by_agent = bool(msg.get("seeing"))
        self.event("agent_vision", **{k: v for k, v in msg.items() if k != "type"}, mine=msg.get("participant_identity") == me)

    def on_attributes(self, changed: dict, p: rtc.Participant) -> None:
        if "arbos.seeing" in changed:
            self.event("agent_attr", identity=p.identity, seeing=changed["arbos.seeing"])

    async def camera_on(self, path: str) -> None:
        img = Image.open(path).convert("RGBA")
        # Letterbox into the camera frame, as a phone would show it.
        scale = min(self.CAM_W / img.width, self.CAM_H / img.height)
        img = img.resize((max(1, int(img.width * scale)), max(1, int(img.height * scale))))
        canvas = Image.new("RGBA", (self.CAM_W, self.CAM_H), (20, 20, 20, 255))
        canvas.paste(img, ((self.CAM_W - img.width) // 2, (self.CAM_H - img.height) // 2))
        self.cam_rgba = canvas.tobytes()
        if self.cam_source is None:
            self.cam_source = rtc.VideoSource(self.CAM_W, self.CAM_H)
            self.cam_track = rtc.LocalVideoTrack.create_video_track("camera", self.cam_source)
            self.cam_pub = await self.room.local_participant.publish_track(
                self.cam_track, rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_CAMERA, simulcast=False)
            )
            self.cam_task = asyncio.create_task(self.camera_pump())
            self.event("camera_on", image=path)
        else:
            if self.cam_track and self.cam_track.muted:
                self.cam_track.unmute()
            self.event("camera_switch", image=path)

    async def camera_pump(self) -> None:
        period = 1.0 / self.CAM_FPS
        while True:
            if self.cam_rgba and self.cam_source:
                frame = rtc.VideoFrame(self.CAM_W, self.CAM_H, rtc.VideoBufferType.RGBA, self.cam_rgba)
                self.cam_source.capture_frame(frame)
            await asyncio.sleep(period)

    async def camera_off(self) -> None:
        if self.cam_track:
            self.cam_track.mute()
            self.event("camera_off")

    async def say(self, text, *, record_end: bool = True) -> None:
        """`text` is a string, or a list of strings and numbers (seconds of silence) so one
        utterance can hold a mid-sentence pause — the test for early cut-ins."""
        if isinstance(text, list):
            parts = []
            for seg in text:
                if isinstance(seg, (int, float)):
                    parts.append(np.zeros(int(float(seg) * SAMPLE_RATE), dtype="<i2"))
                else:
                    parts.append(await tts(seg, self.args.voice, Path(self.args.cache), os.environ["OPENAI_API_KEY"]))
            pcm = np.concatenate(parts)
            text = " … ".join(str(x) for x in text if isinstance(x, str))
        else:
            pcm = await tts(text, self.args.voice, Path(self.args.cache), os.environ["OPENAI_API_KEY"])
        onset, offset = voiced_bounds(pcm)
        self.saying_until = self.now() + offset
        self.early_cut_in_this_turn = False
        self.event("say", text=text, seconds=round(len(pcm) / SAMPLE_RATE, 2), onset=round(onset, 2), offset=round(offset, 2))
        t_start = self.now()
        self.our_speech_onset_at = t_start + onset
        start = time.monotonic()
        for n, i in enumerate(range(0, len(pcm), FRAME_SAMPLES)):
            chunk = pcm[i : i + FRAME_SAMPLES]
            if len(chunk) < FRAME_SAMPLES:
                chunk = np.concatenate([chunk, np.zeros(FRAME_SAMPLES - len(chunk), dtype="<i2")])
            frame = rtc.AudioFrame(data=chunk.tobytes(), sample_rate=SAMPLE_RATE, num_channels=1, samples_per_channel=FRAME_SAMPLES)
            await self.source.capture_frame(frame)
            self.mix.append((self.now(), chunk.copy(), "me"))
            # Real-time pacing: a microphone delivers 20 ms every 20 ms.
            due = start + (n + 1) * FRAME_MS / 1000
            delay = due - time.monotonic()
            if delay > 0:
                await asyncio.sleep(delay)
        self.event("said", seconds=round(self.now() - t_start, 2))
        self.turns += 1
        if record_end:
            # End of speech = the last voiced frame, as a person would hear it.
            self.our_speech_ended_at = t_start + offset

    async def wait_agent_turn(self, max_s: float = 25.0, quiet_s: float = QUIET_S) -> None:
        """Wait until the agent spoke and has been quiet for `quiet_s`."""
        deadline = time.monotonic() + max_s
        spoke = False
        while time.monotonic() < deadline:
            if self.agent_speaking:
                spoke = True
            elif spoke and self.now() - self.agent_last_audio_at >= quiet_s:
                return
            await asyncio.sleep(0.02)
        self.event("wait_timeout", spoke=spoke)

    async def wait_quiet(self, max_s: float) -> None:
        deadline = time.monotonic() + max_s
        while time.monotonic() < deadline:
            if not self.agent_speaking and self.now() - self.agent_last_audio_at >= 1.5:
                return
            await asyncio.sleep(0.05)

    async def barge(self, text: str, after_ms: int, ask: str | None = None) -> None:
        if ask:
            await self.say(ask)
        deadline = time.monotonic() + 30
        while not self.agent_speaking and time.monotonic() < deadline:
            await asyncio.sleep(0.01)
        if not self.agent_speaking:
            self.event("barge_skipped")
            return
        await asyncio.sleep(after_ms / 1000)
        self.event("barge_start")
        task = asyncio.create_task(self.say(text))
        while self.our_speech_onset_at is None or self.our_speech_onset_at > self.now():
            await asyncio.sleep(0.005)
            if task.done():
                break
        onset = self.our_speech_onset_at or self.now()
        # When does the agent's audio stop after our first word?
        cut: int | None = None
        while not task.done():
            if cut is None and self.now() - self.agent_last_audio_at > 0.25 and self.agent_last_audio_at >= onset - 0.3:
                cut = int((self.agent_last_audio_at - onset) * 1000)
            await asyncio.sleep(0.01)
        await task
        if cut is None and self.agent_last_audio_at >= onset - 0.3:
            cut = int((self.agent_last_audio_at - onset) * 1000)
        if cut is not None:
            self.barge_ms.append(max(0, cut))
            self.event("barge_cut", cut_ms=max(0, cut), onset_wall=int((time.time() - (self.now() - onset)) * 1000))

    async def run(self) -> None:
        a = self.args
        # LiveKit objects bind to the running loop: create them here, not in __init__.
        self.room = rtc.Room()
        self.source = rtc.AudioSource(SAMPLE_RATE, 1)
        self.track = rtc.LocalAudioTrack.create_audio_track("mic", self.source)
        async with aiohttp.ClientSession() as http:
            self.call = await self.start_call(http)
            self.room.on("track_subscribed", self.on_track)
            self.room.on("data_received", self.on_data)
            self.room.on("participant_attributes_changed", self.on_attributes)
            self.room.on("participant_connected", lambda p: self.event("participant_connected", identity=p.identity, name=p.name))
            self.room.on("participant_disconnected", lambda p: self.event("participant_disconnected", identity=p.identity))
            await self.room.connect(self.call["livekit_url"], self.call["token"], options=rtc.RoomOptions(auto_subscribe=True))
            self.event("connected", room=self.room.name, identity=self.room.local_participant.identity)
            await self.room.local_participant.publish_track(self.track, rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE))
            self.event("published")
            if a.camera:
                await self.camera_on(a.camera)
            if a.wait_agent:
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline and not any(p.identity.startswith("a_") for p in self.room.remote_participants.values()):
                    await asyncio.sleep(0.1)
                if not any(p.identity.startswith("a_") for p in self.room.remote_participants.values()):
                    self.event("no_agent")
                    if not a.stay:
                        await self.leave(http)
                    await self.room.disconnect()
                    self.save()
                    sys.exit("the agent did not join within 20 s")
                # Let the agent finish its greeting before the script starts talking.
                await asyncio.sleep(a.settle)
                await self.wait_quiet(10.0)
            script = json.loads(Path(a.script).read_text()) if a.script else []
            for step in script:
                if "say" in step:
                    await self.say(step["say"])
                    if step.get("wait", True):
                        await self.wait_agent_turn(step.get("max", 25.0))
                elif "barge" in step:
                    await self.barge(step["barge"], int(step.get("after_ms", 900)), ask=step.get("ask"))
                    await self.wait_agent_turn(step.get("max", 25.0))
                elif "sleep" in step:
                    await asyncio.sleep(float(step["sleep"]))
                elif "wait_quiet" in step:
                    await self.wait_quiet(float(step["wait_quiet"]))
                elif "camera" in step:
                    await self.camera_on(step["camera"])
                elif "camera_off" in step:
                    await self.camera_off()
                elif "hangup" in step:
                    break
            await asyncio.sleep(a.tail)
            if not a.stay:
                await self.leave(http)
            await self.room.disconnect()
        self.save()

    def save(self) -> None:
        if self.agent_pcm:
            wav_write(self.out / "agent.wav", np.concatenate(self.agent_pcm))
        if self.mix:
            total = int(self.now() * SAMPLE_RATE) + SAMPLE_RATE
            buf = np.zeros(total, dtype=np.float32)
            for t, pcm, _who in self.mix:
                i = int(t * SAMPLE_RATE)
                j = min(total, i + len(pcm))
                buf[i:j] += pcm[: j - i].astype(np.float32)
            wav_write(self.out / "mix.wav", np.clip(buf, -32768, 32767).astype("<i2"))
        srt = sorted(self.ttfa_ms)
        summary = {
            "call_id": self.call.get("call_id"),
            "ttfa_ms": self.ttfa_ms,
            "ttfa_p50": srt[len(srt) // 2] if srt else None,
            "ttfa_p95": srt[min(len(srt) - 1, int(round(0.95 * (len(srt) - 1))))] if srt else None,
            "turns": self.turns,
            "early_cut_ins": self.early_cut_ins,
            "barge_ms": self.barge_ms,
            "agent_seconds": round(sum(len(p) for p in self.agent_pcm) / SAMPLE_RATE, 1),
            "seen_by_agent": self.seen_by_agent,
            "out": str(self.out),
        }
        (self.out / "summary.json").write_text(json.dumps(summary, indent=2))
        print(json.dumps(summary, indent=2))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--relay", default=os.environ.get("ARBOS_RELAY_URL", "https://relay.arbos.life"))
    ap.add_argument("--token", default=os.environ.get("ARBOS_DEVICE_TOKEN"), help="device access token")
    ap.add_argument("--chat", help="chat id (people chat or project chat)")
    ap.add_argument("--project", default=None, help="project id for a project chat ('people' or empty for people chats)")
    ap.add_argument("--join", default=None, help="join an existing call id instead of starting one")
    ap.add_argument("--script", default=None)
    ap.add_argument("--voice", default="alloy", help="OpenAI TTS voice for this participant")
    ap.add_argument("--out", default="/tmp/sim-run")
    ap.add_argument("--cache", default="/tmp/sim-tts-cache")
    ap.add_argument("--settle", type=float, default=1.0, help="seconds to wait after the agent joins before the script (then until its greeting ends)")
    ap.add_argument("--tail", type=float, default=2.0, help="seconds to keep recording after the script")
    ap.add_argument("--no-wait-agent", dest="wait_agent", action="store_false")
    ap.add_argument("--stay", action="store_true", help="do not leave the call at the end (another participant ends it)")
    ap.add_argument("--camera", default=None, help="image file to show on a camera track from the start")
    args = ap.parse_args()
    if not args.token:
        sys.exit("--token (or ARBOS_DEVICE_TOKEN) is required")
    if not args.chat and not args.join:
        sys.exit("--chat or --join is required")
    logging.basicConfig(level="INFO", format="%(asctime)s %(message)s")
    asyncio.run(Participant(args).run())


if __name__ == "__main__":
    main()
