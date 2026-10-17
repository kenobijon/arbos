"""Score the relevance judge on the labelled utterance set.

    source .venv/bin/activate
    ARBOS_RELAY_URL=https://relay.arbos.life ARBOS_RELAY_ADMIN_TOKEN=… python tests/eval_judge.py [--project <id>] [--chattiness balanced] [--provider jev|llm]

Prints precision / recall per class (speak, act, silent; the kill words are checked exactly),
the "respond vs silent" confusion, latency and cost, and writes `tests/fixtures/judge-results.json`.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

import aiohttp

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from arbos_voice.config import Config  # noqa: E402
from arbos_voice.judge import Decision, Judge, JudgeContext  # noqa: E402
from arbos_voice.relay import Relay  # noqa: E402

FIXTURES = Path(__file__).parent / "fixtures" / "judge-utterances.jsonl"
RESULTS = Path(__file__).parent / "fixtures" / "judge-results.json"

RESPOND = {"speak", "act"}


def prf(rows: list[dict], cls: str) -> tuple[float, float, int]:
    tp = sum(1 for r in rows if r["label"] == cls and r["got"] == cls)
    fp = sum(1 for r in rows if r["label"] != cls and r["got"] == cls)
    fn = sum(1 for r in rows if r["label"] == cls and r["got"] != cls)
    p = tp / (tp + fp) if tp + fp else 1.0
    r = tp / (tp + fn) if tp + fn else 1.0
    return p, r, tp + fn


async def run(args: argparse.Namespace) -> int:
    cfg = Config()
    async with aiohttp.ClientSession() as http:
        relay = Relay(cfg, http)
        judge = Judge(relay, args.project, cfg.judge_model, cfg.judge_fallback_model)
        if args.provider == "llm":
            judge._jev = _raise  # type: ignore[method-assign]
        rows = []
        for line in FIXTURES.read_text().splitlines():
            if not line.strip():
                continue
            case = json.loads(line)
            ctx = JudgeContext(
                agent_name="Arbos",
                agent_role="Jacob's project agent for the Arbos repo: it runs commands on its machine, reads logs, looks things up on the web, edits files and posts in the chat",
                chat_kind="group",
                wake_name="Forest",
                chattiness=args.chattiness,
                recent=case.get("recent", []),
                agent_has_open_work=case.get("agent_has_open_work", False),
                agent_last_spoke_s_ago=20.0 if case.get("recent") else None,
            )
            t0 = time.monotonic()
            d: Decision = await judge.decide(case["text"], case["from"], ctx)
            ms = int((time.monotonic() - t0) * 1000)
            got = d.action
            rows.append(
                {
                    "id": case["id"],
                    "from": case["from"],
                    "text": case["text"],
                    "label": case["label"],
                    "got": got,
                    "ok": got == case["label"],
                    "p_speak": d.p_speak,
                    "p_act": d.p_act,
                    "p_silent": d.p_silent,
                    "p_addressed": d.p_addressed,
                    "provider": d.provider,
                    "forced": d.forced,
                    "latency_ms": ms,
                    "cost_usd": d.cost_usd,
                    "note": case.get("note", ""),
                }
            )
            mark = "ok " if got == case["label"] else "MISS"
            print(f"{mark} #{case['id']:>2} {case['label']:<6} → {got:<6} speak={d.p_speak} act={d.p_act} silent={d.p_silent} {ms}ms  {case['text'][:70]}")

    n = len(rows)
    correct = sum(1 for r in rows if r["ok"])
    print()
    print(f"chattiness={args.chattiness} provider={args.provider}: {correct}/{n} exact ({correct / n:.1%})")
    for cls in ("speak", "act", "silent", "stop", "leave"):
        p, r, support = prf(rows, cls)
        if support:
            print(f"  {cls:<7} precision {p:.2f}  recall {r:.2f}  (n={support})")
    # The decision that matters most for a quiet agent: respond vs stay silent.
    resp_rows = [
        {"label": "respond" if r["label"] in RESPOND else "silent", "got": "respond" if r["got"] in RESPOND else "silent"}
        for r in rows
        if r["label"] not in ("stop", "leave")
    ]
    p, r, support = prf(resp_rows, "respond")
    print(f"  respond-vs-silent: precision {p:.2f}  recall {r:.2f}  (n respond={support})")
    conf: dict[str, Counter] = defaultdict(Counter)
    for row in rows:
        conf[row["label"]][row["got"]] += 1
    print("  confusion (label → got):", {k: dict(v) for k, v in conf.items()})
    model_rows = [r for r in rows if not r["forced"] and r["provider"] != "rules"]
    if model_rows:
        lat = sorted(r["latency_ms"] for r in model_rows)
        cost = sum(r["cost_usd"] or 0 for r in model_rows)
        print(f"  model calls: {len(model_rows)}, latency p50 {lat[len(lat) // 2]} ms, p95 {lat[int(len(lat) * 0.95) - 1]} ms, cost ${cost:.5f} (${cost / len(model_rows):.6f} each)")
    RESULTS.write_text(
        json.dumps(
            {
                "chattiness": args.chattiness,
                "provider": args.provider,
                "exact": correct / n,
                "respond_precision": p,
                "respond_recall": r,
                "rows": rows,
            },
            indent=2,
        )
    )
    print(f"wrote {RESULTS}")
    return 0


async def _raise(*_a, **_k):
    raise RuntimeError("jev disabled for this run")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--project", default=os.environ.get("ARBOS_VOICE_EVAL_PROJECT", "relay-admin"))
    ap.add_argument("--chattiness", default="balanced", choices=["quiet", "balanced", "chatty"])
    ap.add_argument("--provider", default="jev", choices=["jev", "llm"])
    args = ap.parse_args()
    raise SystemExit(asyncio.run(run(args)))


if __name__ == "__main__":
    main()
