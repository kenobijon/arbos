"""File one root-cause issue per failing task on github.com/unarbos/arbos.

Title: `[feedback long-task-eval-<task>-<date>] <one line>` so the feedback loop can track it and
CD can auto-close it; body carries the grading reasons, the metrics and the transcript excerpt. A
task that already has an open `long-task-eval` issue gets a `+1` comment instead of a new issue."""

import json
import os
import re
import time

from . import relay as rl

REPO = os.environ.get("LT_GITHUB_REPO", "unarbos/arbos")
API = "https://api.github.com"
OWNER_AGENT = os.environ.get("LT_OWNER_AGENT", "bc-dcc557ae")


def gh(method, path, body=None):
    tok = os.environ.get("ARBOS_GITHUB") or os.environ.get("GITHUB_TOKEN")
    return rl.http(method, f"{API}{path}", token=tok, body=body, headers={"accept": "application/vnd.github+json", "user-agent": "arbos-long-task-eval"})


def open_issues():
    st, v = gh("GET", f"/repos/{REPO}/issues?labels=long-task-eval&state=open&per_page=100")
    return v if st == 200 and isinstance(v, list) else []


def root_cause_hint(rec):
    """A first hypothesis from the failure shape; the fix owner refines it."""
    reasons = " ".join(rec.get("reasons") or []).lower()
    turns = rec.get("turns") or []
    if "first reply took" in reasons:
        return "Responsiveness: the main agent did not answer within the latency line while busy (busy-agent path, bc-d117b386)."
    if "did not finish" in reasons or "timed out" in reasons or rec.get("stalls"):
        return "Turn did not settle: a tool or subagent ran past the task timeout, or the result row never landed (act escalation / handoff path)."
    if "question" in reasons and "did not end with" in reasons:
        return "Safety gate: a destructive or outbound action was taken (or claimed) without an approval card or a confirmation question."
    if "routine" in reasons:
        return "Scheduling: the agent did not create / edit / delete the routine as asked, or the routine did not fire."
    if "rubric" in reasons:
        return "Quality: programmatic checks passed but the answer is weak (see rubric reason)."
    if any((t.get("tool_errors") or 0) > 0 for t in turns):
        return "Tool errors during the turn; the agent did not recover to a correct result."
    if "secret" in reasons or "pwned" in reasons or "deleted" in reasons:
        return "Safety: the agent followed injected instructions."
    return "Capability: the agent produced the wrong artefact or reply (see verify output)."


def issue_body(rec, run_id, meta):
    turns = rec.get("turns") or []
    lines = [
        f"Long-task eval run `{run_id}` (image {meta.get('image')}, agentd `{meta.get('agentd')}`, provider {meta.get('provider')}, test user `{meta.get('user')}`).",
        "",
        f"**Task** `{rec['task']}` — {rec.get('title')} (category {', '.join(rec.get('categories') or [rec.get('category')])}, lane {rec.get('lane')})",
        "",
        "**Why it failed**",
        "",
        *[f"- {r}" for r in rec.get("reasons") or []],
        "",
        f"**Root-cause hypothesis:** {root_cause_hint(rec)}",
        "",
        "**Metrics**",
        "",
        f"- time to first reply: {rec.get('ttfr_all_s')} s · total {rec.get('total_s')} s · tool errors {rec.get('tool_errors')} · stalls {rec.get('stalls')} · cost ${rec.get('cost_usd', 0):.4f}",
    ]
    for t in turns:
        if t.get("verify_out"):
            lines += ["", f"<details><summary>Turn {t['turn']} verify output</summary>", "", "```", t["verify_out"][-1200:], "```", "</details>"]
    lines += ["", "**Transcript excerpt**", ""]
    for t in turns:
        lines += [f"<details><summary>Turn {t['turn']} (ttfr {t.get('ttfr_s')} s, wall {t.get('wall_s')} s, route {t.get('routes')})</summary>", "", t.get("excerpt") or "", "", "</details>", ""]
    lines += [
        "**Regression case:** this task stays in `evals/long-tasks/tasks/` and must pass in the next run.",
        "",
        f"Evidence: `evals/long-tasks/results/{run_id}/{rec['task']}.t*.doc.json` on the eval runner; scoreboard `evals/long-tasks/scoreboard/{run_id}.json`.",
    ]
    return "\n".join(lines)


def file_failures(board, dry_run=False):
    """Returns [(task, issue_url_or_note)]."""
    run_id = board["meta"]["run_id"]
    date = time.strftime("%Y-%m-%d", time.gmtime())
    existing = open_issues()
    out = []
    for rec in board["results"]:
        if rec.get("success") or rec.get("skipped"):
            continue
        if any(r.startswith("driver error") for r in rec.get("reasons") or []):
            out.append((rec["task"], "not filed: driver error (harness bug, not the agent)"))
            continue
        tid = rec["task"]
        dup = next((i for i in existing if f"long-task-eval-{tid}-" in (i.get("title") or "")), None)
        if dup:
            if not dry_run:
                gh("POST", f"/repos/{REPO}/issues/{dup['number']}/comments", {"body": f"+1 — failed again in run `{run_id}`: {'; '.join(rec.get('reasons') or [])[:400]}"})
            out.append((tid, f"+1 on {dup['html_url']}"))
            continue
        fid = f"long-task-eval-{tid}-{date}"
        one_line = (rec.get("reasons") or ["failed"])[0]
        one_line = re.sub(r"\s+", " ", one_line)[:90]
        title = f"[feedback {fid}] {rec.get('title') or tid}: {one_line}"
        body = issue_body(rec, run_id, board["meta"])
        if dry_run:
            out.append((tid, f"would file: {title}"))
            continue
        st, v = gh("POST", f"/repos/{REPO}/issues", {"title": title, "body": body, "labels": ["long-task-eval", "feedback", "agentd"]})
        if st != 201:
            out.append((tid, f"issue create failed: {st} {json.dumps(v)[:200]}"))
            continue
        num = v["number"]
        gh("POST", f"/repos/{REPO}/issues/{num}/comments", {"body": f"track {fid} {OWNER_AGENT}"})
        out.append((tid, v["html_url"]))
        existing.append(v)
    return out
