"""Make one spoken sample per gpt-realtime voice, with the realtime model itself (the voice
people will hear on calls), and write `voices.json` for the relay's `GET /voices`.

    OPENAI_API_KEY=… python tools/make_voice_samples.py --out /var/lib/arbos-relay/voices [--model gpt-realtime-2.1]

Writes `<out>/<voice>.mp3` (and `.wav`), `<out>/voices.json`. A voice that the API refuses is
left out and reported. Run once; the relay serves the files with long cache headers.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import json
import os
import subprocess
import wave
from pathlib import Path

import aiohttp

LINE = "Hi, I'm your Arbos agent. How can I help?"

# Candidates from the OpenAI SDK's realtime voice type (openai.types.realtime.realtime_audio_config_output.Voice).
CANDIDATES = [
    ("marin", "Marin", "Warm, clear and even. The default."),
    ("cedar", "Cedar", "Deep and calm."),
    ("alloy", "Alloy", "Neutral and balanced."),
    ("ash", "Ash", "Soft, low and steady."),
    ("ballad", "Ballad", "Gentle, a little musical."),
    ("coral", "Coral", "Bright and friendly."),
    ("echo", "Echo", "Smooth and measured."),
    ("sage", "Sage", "Light and quick."),
    ("shimmer", "Shimmer", "Clear, upbeat and brisk."),
    ("verse", "Verse", "Expressive and lively."),
]


async def sample(session: aiohttp.ClientSession, key: str, model: str, voice: str) -> bytes:
    url = f"wss://api.openai.com/v1/realtime?model={model}"
    pcm = bytearray()
    async with session.ws_connect(url, headers={"Authorization": f"Bearer {key}"}, timeout=aiohttp.ClientWSTimeout(ws_close=20)) as ws:
        await ws.send_json(
            {
                "type": "session.update",
                "session": {
                    "type": "realtime",
                    "output_modalities": ["audio"],
                    "audio": {"output": {"voice": voice, "format": {"type": "audio/pcm", "rate": 24000}}},
                },
            }
        )
        await ws.send_json(
            {
                "type": "response.create",
                "response": {"instructions": f"Say exactly this line, warmly, and nothing else: {LINE}"},
            }
        )
        async for msg in ws:
            if msg.type != aiohttp.WSMsgType.TEXT:
                break
            ev = json.loads(msg.data)
            t = ev.get("type", "")
            if t in ("response.output_audio.delta", "response.audio.delta"):
                pcm.extend(base64.b64decode(ev["delta"]))
            elif t == "error":
                raise RuntimeError(ev.get("error", {}).get("message", str(ev)))
            elif t == "response.done":
                status = ev.get("response", {}).get("status")
                if status != "completed":
                    raise RuntimeError(f"response {status}: {ev.get('response', {}).get('status_details')}")
                break
    if len(pcm) < 24000:
        raise RuntimeError(f"only {len(pcm)} bytes of audio")
    return bytes(pcm)


def write_wav(path: Path, pcm: bytes) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(24000)
        w.writeframes(pcm)


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--model", default="gpt-realtime-2.1")
    ap.add_argument("--only", default=None, help="comma-separated voice ids")
    args = ap.parse_args()
    key = os.environ["OPENAI_API_KEY"]
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    wanted = set(args.only.split(",")) if args.only else None
    voices = []
    async with aiohttp.ClientSession() as session:
        for vid, name, desc in CANDIDATES:
            if wanted and vid not in wanted:
                continue
            try:
                pcm = await sample(session, key, args.model, vid)
            except Exception as e:
                print(f"{vid}: FAILED — {e}")
                continue
            wav = out / f"{vid}.wav"
            write_wav(wav, pcm)
            mp3 = out / f"{vid}.mp3"
            subprocess.run(["ffmpeg", "-y", "-v", "error", "-i", str(wav), "-codec:a", "libmp3lame", "-b:a", "96k", str(mp3)], check=True)
            secs = len(pcm) / 2 / 24000
            print(f"{vid}: {secs:.1f} s → {mp3.name}")
            voices.append({"id": vid, "name": name, "description": desc, "seconds": round(secs, 1), "line": LINE, "model": args.model})
    (out / "voices.json").write_text(json.dumps({"default": "marin", "voices": voices}, indent=2))
    print(f"wrote {out / 'voices.json'} ({len(voices)} voices)")


if __name__ == "__main__":
    asyncio.run(main())
