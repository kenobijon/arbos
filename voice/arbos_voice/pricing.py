"""List prices (USD) used to log cost per call. Read 2026-10-16; see docs/spec-agent-voice-calls.md §1.

The relay charges what the worker reports, so these numbers are the bill. Keep them current.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class RealtimePrice:
    """USD per 1M tokens."""

    audio_in: float
    audio_out: float
    text_in: float
    text_out: float
    cached_audio_in: float
    cached_text_in: float
    image_in: float = 0.0


REALTIME_PRICES: dict[str, RealtimePrice] = {
    "gpt-realtime-2.1": RealtimePrice(
        audio_in=32.0, audio_out=64.0, text_in=4.0, text_out=16.0, cached_audio_in=0.40, cached_text_in=0.40, image_in=5.0
    ),
    "gpt-realtime": RealtimePrice(
        audio_in=32.0, audio_out=64.0, text_in=4.0, text_out=16.0, cached_audio_in=0.40, cached_text_in=0.40, image_in=5.0
    ),
    "gpt-realtime-2.1-mini": RealtimePrice(
        audio_in=10.0, audio_out=20.0, text_in=0.60, text_out=2.40, cached_audio_in=0.30, cached_text_in=0.06
    ),
    "gpt-realtime-mini": RealtimePrice(
        audio_in=10.0, audio_out=20.0, text_in=0.60, text_out=2.40, cached_audio_in=0.30, cached_text_in=0.06
    ),
}

# Per minute of audio sent to the transcription model (batch per utterance).
STT_USD_PER_MINUTE: dict[str, float] = {
    "gpt-4o-mini-transcribe": 0.003,
    "gpt-4o-transcribe": 0.006,
    "whisper-1": 0.006,
}


def realtime_price(model: str) -> RealtimePrice:
    m = model.lower()
    if m in REALTIME_PRICES:
        return REALTIME_PRICES[m]
    if "mini" in m:
        return REALTIME_PRICES["gpt-realtime-mini"]
    return REALTIME_PRICES["gpt-realtime"]


def stt_price(model: str) -> float:
    m = model.lower()
    return STT_USD_PER_MINUTE.get(m, 0.003 if "mini" in m else 0.006)


def realtime_cost(
    model: str,
    *,
    audio_in: int,
    audio_out: int,
    text_in: int,
    text_out: int,
    cached_audio: int = 0,
    cached_text: int = 0,
    image_in: int = 0,
) -> float:
    p = realtime_price(model)
    # Cached tokens are a subset of the input tokens; they are billed at the cached rate instead.
    audio_in_uncached = max(0, audio_in - cached_audio)
    text_in_uncached = max(0, text_in - cached_text)
    usd = (
        audio_in_uncached * p.audio_in
        + cached_audio * p.cached_audio_in
        + text_in_uncached * p.text_in
        + cached_text * p.cached_text_in
        + audio_out * p.audio_out
        + text_out * p.text_out
        + image_in * p.image_in
    ) / 1_000_000.0
    return usd
