#!/usr/bin/env python3
"""Fold a partial re-run into a scoreboard: records of the re-run replace the same tasks' records
in the base run (marked `rerun`), aggregates are recomputed, the re-run file is removed.

  merge.py --base <run-id> --rerun <run-id> --note "<why the tasks were re-run>"
"""

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import summarize  # noqa: E402

SB = Path(__file__).resolve().parent / "scoreboard"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--rerun", required=True)
    ap.add_argument("--note", default="")
    args = ap.parse_args()
    base = json.loads((SB / f"{args.base}.json").read_text())
    rerun = json.loads((SB / f"{args.rerun}.json").read_text())
    replaced = {r["task"]: r for r in rerun["results"]}
    results = []
    for r in base["results"]:
        if r["task"] in replaced:
            new = dict(replaced.pop(r["task"]))
            new["rerun"] = {"run_id": args.rerun, "note": args.note, "first_attempt": {"success": r.get("success"), "reasons": r.get("reasons")}}
            results.append(new)
        else:
            results.append(r)
    for r in replaced.values():
        r["rerun"] = {"run_id": args.rerun, "note": args.note}
        results.append(r)
    meta = dict(base["meta"])
    meta["run_cost_usd"] = round((meta.get("run_cost_usd") or 0) + (rerun["meta"].get("run_cost_usd") or 0), 4)
    meta["judge_cost_usd"] = round((meta.get("judge_cost_usd") or 0) + (rerun["meta"].get("judge_cost_usd") or 0), 4)
    meta.setdefault("reruns", []).append({"run_id": args.rerun, "tasks": sorted(r["task"] for r in rerun["results"]), "note": args.note})
    meta["ended_at"] = max(meta.get("ended_at") or "", rerun["meta"].get("ended_at") or "")
    board = summarize(meta, results)
    (SB / f"{args.base}.json").write_text(json.dumps(board, indent=1))
    (SB / f"{args.rerun}.json").unlink()
    print(json.dumps(board["summary"], indent=1))


if __name__ == "__main__":
    main()
