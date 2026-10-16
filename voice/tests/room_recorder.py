"""Record a LiveKit call room to WAV as a hidden, subscribe-only participant (proof recordings).

    python tests/room_recorder.py --room call_… --out /tmp/rec [--url wss://livekit.arbos.life]

Needs LIVEKIT_API_KEY / LIVEKIT_API_SECRET (mints its own token: identity `rec_<n>`, hidden,
canSubscribe only). The voice worker ignores identities that do not start with `u_`, and the
relay webhook ignores unknown identities, so the recorder changes nothing about the call.

Writes `<out>/mix.wav` (everyone, 24 kHz mono), `<out>/<identity>.wav` per track, and
`<out>/timeline.jsonl` (wall-clock ms for track start/stop and speech on/off per participant),
plus `<out>/t0.txt` = wall-clock ms of sample 0 (to line the audio up with a screen recording).
Stop with SIGINT or when the room closes.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import time
import wave
from pathlib import Path

import numpy as np
from livekit import api, rtc

SAMPLE_RATE = 24000
SPEECH_RMS = 350


def wav_write(path: Path, pcm: np.ndarray) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        w.writeframes(pcm.astype("<i2").tobytes())


class Recorder:
    def __init__(self, out: Path):
        self.out = out
        out.mkdir(parents=True, exist_ok=True)
        self.t0_wall = time.time()
        self.t0 = time.monotonic()
        self.tracks: dict[str, list[tuple[float, np.ndarray]]] = {}
        self.timeline = open(out / "timeline.jsonl", "a")
        self.speaking: dict[str, bool] = {}
        self.last_audio: dict[str, float] = {}
        (out / "t0.txt").write_text(str(int(self.t0_wall * 1000)))

    def now(self) -> float:
        return time.monotonic() - self.t0

    def mark(self, kind: str, **f) -> None:
        rec = {"t": round(self.now(), 3), "wall": int(time.time() * 1000), "kind": kind, **f}
        self.timeline.write(json.dumps(rec) + "\n")
        self.timeline.flush()
        print(json.dumps(rec), flush=True)

    async def pump(self, track: rtc.Track, identity: str, name: str) -> None:
        self.mark("track_start", identity=identity, name=name)
        stream = rtc.AudioStream(track, sample_rate=SAMPLE_RATE, num_channels=1)
        buf = self.tracks.setdefault(identity, [])
        async for ev in stream:
            pcm = np.frombuffer(ev.frame.data, dtype="<i2").copy()
            t = self.now()
            buf.append((t, pcm))
            rms = float(np.sqrt(np.mean(pcm.astype(np.float32) ** 2))) if len(pcm) else 0.0
            if rms > SPEECH_RMS:
                self.last_audio[identity] = t
                if not self.speaking.get(identity):
                    self.speaking[identity] = True
                    self.mark("speech_on", identity=identity)
            elif self.speaking.get(identity) and t - self.last_audio.get(identity, 0) > 0.4:
                self.speaking[identity] = False
                self.mark("speech_off", identity=identity)
        self.mark("track_end", identity=identity)

    def save(self) -> None:
        total = int(self.now() * SAMPLE_RATE) + SAMPLE_RATE
        mix = np.zeros(total, dtype=np.float32)
        for identity, chunks in self.tracks.items():
            track = np.zeros(total, dtype=np.float32)
            for t, pcm in chunks:
                i = int(t * SAMPLE_RATE)
                j = min(total, i + len(pcm))
                if j > i:
                    track[i:j] += pcm[: j - i].astype(np.float32)
            wav_write(self.out / f"{identity}.wav", np.clip(track, -32768, 32767).astype("<i2"))
            mix += track
        wav_write(self.out / "mix.wav", np.clip(mix, -32768, 32767).astype("<i2"))
        print("saved", self.out, "seconds", round(self.now(), 1))


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--room", required=True)
    ap.add_argument("--url", default=os.environ.get("LIVEKIT_URL", "wss://livekit.arbos.life"))
    ap.add_argument("--out", default="/tmp/rec")
    ap.add_argument("--identity", default=f"rec_{int(time.time()) % 100000}")
    ap.add_argument("--max", type=float, default=900.0)
    args = ap.parse_args()
    token = (
        api.AccessToken(os.environ["LIVEKIT_API_KEY"], os.environ["LIVEKIT_API_SECRET"])
        .with_identity(args.identity)
        .with_name("recorder")
        .with_grants(api.VideoGrants(room_join=True, room=args.room, can_subscribe=True, can_publish=False, can_publish_data=False, hidden=True))
        .to_jwt()
    )
    rec = Recorder(Path(args.out))
    room = rtc.Room()
    done = asyncio.Event()

    @room.on("track_subscribed")
    def _sub(track: rtc.Track, pub: rtc.RemoteTrackPublication, p: rtc.RemoteParticipant) -> None:
        if track.kind == rtc.TrackKind.KIND_AUDIO:
            asyncio.create_task(rec.pump(track, p.identity, p.name))

    @room.on("participant_connected")
    def _joined(p: rtc.RemoteParticipant) -> None:
        rec.mark("participant_connected", identity=p.identity, name=p.name)

    @room.on("participant_disconnected")
    def _left(p: rtc.RemoteParticipant) -> None:
        rec.mark("participant_disconnected", identity=p.identity)

    @room.on("disconnected")
    def _disc(*_a) -> None:
        rec.mark("room_disconnected")
        done.set()

    loop = asyncio.get_running_loop()
    loop.add_signal_handler(signal.SIGINT, done.set)
    loop.add_signal_handler(signal.SIGTERM, done.set)
    await room.connect(args.url, token, options=rtc.RoomOptions(auto_subscribe=True))
    rec.mark("connected", room=room.name, participants=[p.identity for p in room.remote_participants.values()])
    try:
        await asyncio.wait_for(done.wait(), timeout=args.max)
    except asyncio.TimeoutError:
        rec.mark("max_reached")
    await room.disconnect()
    rec.save()


if __name__ == "__main__":
    asyncio.run(main())
