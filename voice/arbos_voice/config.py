"""Environment-driven configuration. Everything has a default except the three keys."""

from __future__ import annotations

import os
from dataclasses import dataclass, field


def _env(name: str, default: str) -> str:
    v = os.environ.get(name, "").strip()
    return v or default


def _env_f(name: str, default: float) -> float:
    try:
        return float(_env(name, str(default)))
    except ValueError:
        return default


@dataclass(frozen=True)
class Config:
    relay_url: str = field(default_factory=lambda: _env("ARBOS_RELAY_URL", "http://127.0.0.1:7700").rstrip("/"))
    relay_token: str = field(default_factory=lambda: _env("ARBOS_RELAY_ADMIN_TOKEN", ""))
    openai_api_key: str = field(default_factory=lambda: _env("OPENAI_API_KEY", ""))
    livekit_url: str = field(default_factory=lambda: _env("LIVEKIT_URL", ""))
    livekit_api_key: str = field(default_factory=lambda: _env("LIVEKIT_API_KEY", ""))
    livekit_api_secret: str = field(default_factory=lambda: _env("LIVEKIT_API_SECRET", ""))
    agent_name: str = field(default_factory=lambda: _env("ARBOS_VOICE_AGENT_NAME", "arbos-voice"))
    model: str = field(default_factory=lambda: _env("ARBOS_VOICE_MODEL", "gpt-realtime-2.1"))
    voice: str = field(default_factory=lambda: _env("ARBOS_VOICE_VOICE", "marin"))
    stt_model: str = field(default_factory=lambda: _env("ARBOS_VOICE_STT_MODEL", "gpt-4o-mini-transcribe"))
    judge_model: str = field(default_factory=lambda: _env("ARBOS_VOICE_JUDGE_MODEL", "typesafe/jev-1.14"))
    judge_fallback_model: str = field(
        default_factory=lambda: _env("ARBOS_VOICE_JUDGE_FALLBACK_MODEL", "anthropic/claude-haiku-5.5")
    )
    usage_every_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_USAGE_EVERY_S", 30.0))
    # Turn-taking in group calls (spec §3): the agent waits for this much room silence.
    group_silence_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_GROUP_SILENCE_S", 0.7))
    # Idle hang-up (spec §6).
    duplex_idle_ask_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_IDLE_ASK_S", 180.0))
    duplex_idle_leave_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_IDLE_LEAVE_S", 240.0))
    group_idle_leave_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_GROUP_IDLE_LEAVE_S", 1200.0))
    call_max_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_CALL_MAX_S", 3600.0))
    # How long a spoken request waits for the agent before "working on it".
    tool_quick_wait_s: float = field(default_factory=lambda: _env_f("ARBOS_VOICE_TOOL_QUICK_WAIT_S", 8.0))
    log_dir: str = field(default_factory=lambda: _env("ARBOS_VOICE_LOG_DIR", ""))

    def check(self) -> list[str]:
        missing = []
        if not self.relay_token:
            missing.append("ARBOS_RELAY_ADMIN_TOKEN")
        if not self.openai_api_key:
            missing.append("OPENAI_API_KEY")
        if not self.livekit_url or not self.livekit_api_key or not self.livekit_api_secret:
            missing.append("LIVEKIT_URL / LIVEKIT_API_KEY / LIVEKIT_API_SECRET")
        return missing


CONFIG = Config()
