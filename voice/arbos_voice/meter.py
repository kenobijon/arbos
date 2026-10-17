"""Per-call meter: tokens → dollars, time to first audio, barge-in cut time, counts.

Everything the relay stores per (call, agent) comes from `usage()`; everything in the report
comes from the same numbers, so the bill and the measurements agree.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field

from .pricing import realtime_cost, stt_price


def percentile(values: list[int], p: float) -> int | None:
    if not values:
        return None
    s = sorted(values)
    idx = min(len(s) - 1, max(0, int(round(p * (len(s) - 1)))))
    return s[idx]


@dataclass
class CallMeter:
    model: str
    stt_model: str
    started: float = field(default_factory=time.monotonic)
    mode: str = "duplex"
    audio_in: int = 0
    audio_out: int = 0
    text_in: int = 0
    text_out: int = 0
    cached_audio: int = 0
    cached_text: int = 0
    image_in: int = 0
    stt_seconds: float = 0.0
    judge_usd: float = 0.0
    decisions: int = 0
    tool_calls: int = 0
    turns_spoken: int = 0
    responses: int = 0
    ttfa_ms: list[int] = field(default_factory=list)
    ttft_ms: list[int] = field(default_factory=list)
    barge_ms: list[int] = field(default_factory=list)
    stop_reason: str | None = None

    def seconds(self) -> float:
        return time.monotonic() - self.started

    def add_realtime(self, *, input_tokens: int, output_tokens: int, details_in, details_out, ttft_s: float | None) -> None:
        self.responses += 1
        a_in = int(getattr(details_in, "audio_tokens", 0) or 0)
        t_in = int(getattr(details_in, "text_tokens", 0) or 0)
        i_in = int(getattr(details_in, "image_tokens", 0) or 0)
        cached = getattr(details_in, "cached_tokens_details", None)
        c_audio = int(getattr(cached, "audio_tokens", 0) or 0) if cached else 0
        c_text = int(getattr(cached, "text_tokens", 0) or 0) if cached else 0
        if not cached:
            # Older shapes: one cached_tokens number; treat it as text (the cheaper assumption is
            # wrong in the model's favour, so assume audio when the response had audio input).
            c_all = int(getattr(details_in, "cached_tokens", 0) or 0)
            if a_in > 0:
                c_audio = min(c_all, a_in)
                c_text = min(c_all - c_audio, t_in)
            else:
                c_text = min(c_all, t_in)
        if a_in + t_in + i_in == 0:
            t_in = int(input_tokens or 0)
        a_out = int(getattr(details_out, "audio_tokens", 0) or 0)
        t_out = int(getattr(details_out, "text_tokens", 0) or 0)
        if a_out + t_out == 0:
            t_out = int(output_tokens or 0)
        self.audio_in += a_in
        self.text_in += t_in
        self.image_in += i_in
        self.cached_audio += c_audio
        self.cached_text += c_text
        self.audio_out += a_out
        self.text_out += t_out
        if ttft_s is not None and ttft_s > 0:
            self.ttft_ms.append(int(ttft_s * 1000))

    def add_stt(self, seconds: float) -> None:
        self.stt_seconds += max(0.0, seconds)

    def add_decision(self, cost_usd: float | None) -> None:
        self.decisions += 1
        self.judge_usd += float(cost_usd or 0.0)

    def realtime_usd(self) -> float:
        return realtime_cost(
            self.model,
            audio_in=self.audio_in,
            audio_out=self.audio_out,
            text_in=self.text_in,
            text_out=self.text_out,
            cached_audio=self.cached_audio,
            cached_text=self.cached_text,
            image_in=self.image_in,
        )

    def stt_usd(self) -> float:
        return self.stt_seconds / 60.0 * stt_price(self.stt_model)

    def usd(self) -> float:
        return self.realtime_usd() + self.stt_usd() + self.judge_usd

    def usage(self, *, ended: bool = False) -> dict:
        secs = self.seconds()
        return {
            "mode": self.mode,
            "seconds": round(secs, 1),
            "audio_in_tokens": self.audio_in,
            "audio_out_tokens": self.audio_out,
            "text_in_tokens": self.text_in,
            "text_out_tokens": self.text_out,
            "cached_tokens": self.cached_audio + self.cached_text,
            "stt_seconds": round(self.stt_seconds, 1),
            "decisions": self.decisions,
            "tool_calls": self.tool_calls,
            "turns_spoken": self.turns_spoken,
            "usd": round(self.usd(), 6),
            "ttfa_ms_p50": percentile(self.ttfa_ms, 0.5),
            "ttfa_ms_p95": percentile(self.ttfa_ms, 0.95),
            "barge_ms_p50": percentile(self.barge_ms, 0.5),
            "barge_ms_max": max(self.barge_ms) if self.barge_ms else None,
            "ended": ended,
            "stop_reason": self.stop_reason,
        }

    def summary(self) -> dict:
        secs = max(self.seconds(), 1e-6)
        return {
            **self.usage(),
            "realtime_usd": round(self.realtime_usd(), 6),
            "stt_usd": round(self.stt_usd(), 6),
            "judge_usd": round(self.judge_usd, 6),
            "usd_per_minute": round(self.usd() / (secs / 60.0), 5),
            "usd_per_hour": round(self.usd() / (secs / 3600.0), 4),
            "ttft_ms_p50": percentile(self.ttft_ms, 0.5),
            "ttfa_ms": self.ttfa_ms,
            "barge_ms": self.barge_ms,
            "responses": self.responses,
        }
