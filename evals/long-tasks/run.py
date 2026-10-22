#!/usr/bin/env python3
"""Long-task agent eval runner (D94).

Runs every task in tasks/<category>/<id>.json against the dedicated test agent through the real
product path (relay → agentd → image on the test user's sandbox), in parallel lanes (one project
per lane), grades each turn, and writes scoreboard/<run-id>.json plus results/<run-id>/ with the
chat docs, pi sessions and per-task records.

  run.py [--tasks a,b] [--lanes a,b] [--budget-usd 8] [--run-id ID] [--out DIR] [--dry-run]
"""

import argparse
import json
import os
import re
import sys
import threading
import time
import traceback
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lib import box as bx  # noqa: E402
from lib import driver as dr  # noqa: E402
from lib import fixtures as fx  # noqa: E402
from lib import grade as gr  # noqa: E402
from lib import relay as rl  # noqa: E402

HERE = Path(__file__).resolve().parent
TEST_HANDLE = os.environ.get("LT_TEST_HANDLE", "lt-eval")
LANES = ["a", "b", "c", "d"]
# fixtures the agent must fetch over the web live in the repo; main after the suite lands, else the branch
FIXTURES = os.environ.get("LT_FIXTURE_BASE", "https://raw.githubusercontent.com/unarbos/arbos/main/evals/long-tasks/fixtures")


def load_tasks(only=None, lanes=None):
    tasks = []
    for p in sorted(HERE.glob("tasks/*/*.json")):
        d = json.loads(p.read_text())
        d["_file"] = str(p.relative_to(HERE))
        if only and d["id"] not in only:
            continue
        if lanes and d["lane"] not in lanes:
            continue
        tasks.append(d)
    # same_chat_as tasks must follow their parent
    tasks.sort(key=lambda t: (t["lane"], t.get("same_chat_as") or t["id"], 0 if not t.get("same_chat_as") else 1, t["id"]))
    return tasks


def budget_left(run_state):
    spent, cap = rl.spent_today()
    run_cost = spent - run_state["spent_at_start"]
    return run_state["budget"] - run_cost, run_cost, spent, cap


class Lane(threading.Thread):
    def __init__(self, name, project, tasks, box, run_state, out_dir, log):
        super().__init__(daemon=True, name=f"lane-{name}")
        self.lane = name
        self.project = project
        self.tasks = tasks
        self.box = box
        self.rs = run_state
        self.out = out_dir
        self.log = log
        self.results = []
        self.chats = {}  # task id -> last chat id (for same_chat_as)

    def say(self, msg):
        line = f"[{time.strftime('%H:%M:%S')}] [{self.lane}] {msg}"
        print(line, flush=True)
        with self.rs["lock"]:
            self.log.write(line + "\n")
            self.log.flush()

    def run(self):
        for task in self.tasks:
            left, run_cost, spent, cap = budget_left(self.rs)
            if left <= 0:
                self.say(f"budget stop: run cost ${run_cost:.2f} ≥ ${self.rs['budget']}")
                self.results.append({"task": task["id"], "category": task["category"], "lane": self.lane, "success": False, "skipped": "budget", "reasons": ["skipped: budget exhausted"]})
                continue
            try:
                rec = self.run_task(task)
            except Exception as e:
                rec = {"task": task["id"], "category": task["category"], "lane": self.lane, "success": False, "reasons": [f"driver error: {e}"], "traceback": traceback.format_exc()[-1500:]}
                self.say(f"{task['id']}: DRIVER ERROR {e}")
            self.results.append(rec)
            with self.rs["lock"]:
                (self.out / "records.jsonl").open("a").write(json.dumps(rec) + "\n")

    def run_task(self, task):
        tid = task["id"]
        tag = self.rs["run_id"]
        proj_dir = f"/work/{self.project}"
        d_rel = f"lt-{tid}-{tag}"
        D = f"{proj_dir}/{d_rel}"
        env = {"D": D, "P": proj_dir}
        self.say(f"{tid}: start ({D})")
        t_task0 = time.time()
        usage0 = rl.usage_all(self.project)
        gen_truth = {}
        chat_id = None
        if task.get("same_chat_as"):
            chat_id = self.chats.get(task["same_chat_as"])
        turns_out = []
        all_docs = []
        first_chat = None
        for i, turn in enumerate(task["turns"], 1):
            prompt = turn["prompt"].replace("{D}", D).replace("{FIXTURES}", FIXTURES)
            if turn.get("seed"):
                code, out = self.box.run_as_ws(self.project, turn["seed"].replace("{D}", D).replace("{P}", proj_dir), env=env, timeout=180)
                if "fatal" in out.lower() or (code not in (0, None)):
                    self.say(f"{tid}: seed rc={code}: {out[-200:]}")
            if turn.get("generate"):
                files, gen_truth = fx.GENERATORS[turn["generate"]]()
                ok, msg = self.box.put_files(self.project, D, files)
                if not ok:
                    self.say(f"{tid}: fixture upload failed: {msg}")
            if turn.get("pre_wait_s"):
                time.sleep(turn["pre_wait_s"])
            if chat_id is None or turn.get("new_chat"):
                chat_id = rl.chat_open(self.project, f"{tid} {tag}" + (f" t{i}" if turn.get("new_chat") else ""))
                if first_chat is None:
                    first_chat = chat_id
            t0_ms = int(time.time() * 1000)
            res = dr.send_turn(
                self.project,
                chat_id,
                prompt,
                turn.get("timeout_s", task.get("timeout_s", 600)),
                until_first_reply=bool(turn.get("background")),
            )
            if turn.get("after_s"):
                time.sleep(turn["after_s"])
            entries = self.box.session_entries(self.project, chat_id)
            sm = dr.session_metrics(entries, prompt)
            dm = dr.doc_metrics(res["rows"])
            audit = self.box.audit_since(self.project, t0_ms - 2000)
            turn_spec = dict(turn)
            turn_spec["prompt"] = prompt
            for k in ("verify", "show"):
                if turn_spec.get(k):
                    turn_spec[k] = turn_spec[k].replace("{D}", D).replace("{P}", proj_dir)
            ctx = {"reply": dm.get("reply") or dm.get("all_text") or "", "all_text": dm.get("all_text"), "dm": dm, "res": res, "gen_truth": gen_truth}
            g = gr.grade_turn(turn_spec, ctx, self.box, self.project, env)
            turn_rec = {
                "turn": i,
                "chat_id": chat_id,
                "prompt": prompt[:300],
                "ok": g["ok"],
                "reasons": g["reasons"],
                "ttfr_s": res["ttfr_s"],
                "first_status_s": res["first_status_s"],
                "wall_s": res["wall_s"],
                "timed_out": res["timed_out"],
                "stalls": res["stalls"],
                "approvals_rejected": res["rejected"],
                "routes": dm.get("routes"),
                "tool_calls": sm.get("tool_calls"),
                "tool_errors": (sm.get("tool_errors") or 0) + (dm.get("tool_part_errors") or 0),
                "model_calls": sm.get("model_calls"),
                "compactions": sm.get("compactions"),
                "audit_denied": sum(1 for a in audit if a.get("kind") == "tool_call" and a.get("denied")),
                "rubric": g["rubric"],
                "truth": g["truth"],
                "verify_out": g["verify_out"],
                "reply": (dm.get("reply") or "")[:1500],
                "excerpt": dr.transcript_excerpt(prompt, res["rows"], sm),
            }
            turns_out.append(turn_rec)
            all_docs.append(res["doc"])
            (self.out / f"{tid}.t{i}.doc.json").write_text(json.dumps(res["doc"], indent=1))
            (self.out / f"{tid}.t{i}.session.jsonl").write_text("\n".join(json.dumps(e) for e in entries))
            self.say(f"{tid} turn {i}: {'ok' if g['ok'] else 'FAIL ' + str(g['reasons'])[:160]} | ttfr {res['ttfr_s']}s wall {res['wall_s']}s tools {sm.get('tool_calls')} err {turn_rec['tool_errors']}")
        self.chats[tid] = chat_id
        reasons = [f"t{t['turn']}: {r}" for t in turns_out for r in t["reasons"]]
        # background tasks: wait for the result on the box, then look at the whole transcript
        vw = task.get("verify_wait")
        vw_rec = None
        if vw:
            deadline = time.time() + vw["wait_s"]
            verify = vw["verify"].replace("{D}", D)
            out = ""
            while time.time() < deadline:
                code, out = self.box.run_as_ws(self.project, verify, env=env, timeout=120)
                if "PASS" in out:
                    break
                time.sleep(20)
            doc = rl.chat_dump(self.project, first_chat)
            rows = [m for m in doc.get("messages", []) if m.get("role") == "assistant"]
            dm_all = dr.doc_metrics(rows)
            vw_rec = {"verify_out": out[-600:], "waited_s": round(vw["wait_s"] - (deadline - time.time()), 1), "progress_rows": dm_all["progress_rows"], "all_text": (dm_all["all_text"] or "")[:4000]}
            (self.out / f"{tid}.final.doc.json").write_text(json.dumps(doc, indent=1))
            if "PASS" not in out:
                reasons.append(f"background result: {out.strip().splitlines()[-1][:160] if out.strip() else 'no output'}")
            if vw.get("min_progress_rows") and dm_all["progress_rows"] < vw["min_progress_rows"]:
                reasons.append(f"only {dm_all['progress_rows']} progress rows (need {vw['min_progress_rows']})")
            if vw.get("rubric"):
                rb = gr.judge(task["turns"][0]["prompt"], vw["rubric"]["criteria"], dm_all["all_text"], out, out)
                vw_rec["rubric"] = rb
                if rb.get("score") is not None and rb["score"] < vw["rubric"].get("min", 3):
                    reasons.append(f"rubric {rb['score']}/5: {rb['reason']}")
            all_docs.append(doc)
        dc = task.get("doc_checks")
        if dc:
            checks = doc_checks(all_docs)
            if dc.get("max_handoffs") is not None and checks["handoffs"] > dc["max_handoffs"]:
                reasons.append(f"{checks['handoffs']} handoffs (continue-act) > {dc['max_handoffs']}")
            if dc.get("no_duplicate_status") and checks["dup_status"]:
                reasons.append(f"duplicated status lines: {checks['dup_status'][:3]}")
            if dc.get("no_empty_result_rows") and checks["empty_result_rows"]:
                reasons.append(f"{checks['empty_result_rows']} result row(s) with tool cards but no text (reply lost)")
            if dc.get("no_repeated_sentence") and checks["repeated_sentences"]:
                reasons.append(f"a sentence appears twice in one bubble: {checks['repeated_sentences'][:2]}")
            if dc.get("forbid_text_regex"):
                hits = [t[:80] for t in checks["texts"] if re.search(dc["forbid_text_regex"], t, re.I)]
                if hits:
                    reasons.append(f"raw error text shown to the user: {hits[:2]}")
            if dc.get("failure_lines_exactly") is not None:
                n = sum(1 for t in checks["texts"] if re.search(dc.get("failure_regex", "could not|couldn't|failed|unable|did not start|didn't start"), t, re.I))
                if n != dc["failure_lines_exactly"]:
                    reasons.append(f"{n} failure line(s) shown, expected exactly {dc['failure_lines_exactly']}")
        if task.get("cleanup"):
            self.box.run_as_ws(self.project, task["cleanup"].replace("{D}", D).replace("{P}", proj_dir), env=env, timeout=120)
        if task.get("cleanup_routines"):
            for r in rl.routines(self.project):
                if r.get("id"):
                    rl.delete_routine(self.project, r["id"])
        usage1 = rl.usage_all(self.project)
        rel = rl.diff_usage(usage0, usage1)
        judge_usd = sum(((t.get("rubric") or {}).get("usd") or 0) for t in turns_out) + (((vw_rec or {}).get("rubric") or {}).get("usd") or 0)
        rec = {
            "task": tid,
            "title": task.get("title"),
            "category": task["category"],
            "categories": task.get("categories") or [task["category"]],
            "lane": self.lane,
            "regression": task.get("regression"),
            "success": not reasons,
            "reasons": reasons,
            "ttfr_s": turns_out[0]["ttfr_s"] if turns_out else None,
            "ttfr_all_s": [t["ttfr_s"] for t in turns_out],
            "total_s": round(time.time() - t_task0, 1),
            "tool_errors": sum(t["tool_errors"] or 0 for t in turns_out),
            "stalls": sum(t["stalls"] for t in turns_out) + sum(1 for t in turns_out if t["timed_out"]),
            "cost_usd": rel.get("usd", 0.0),
            "judge_usd": round(judge_usd, 5),
            "relay": rel,
            "rubric_min": min([t["rubric"]["score"] for t in turns_out if t.get("rubric") and t["rubric"].get("score") is not None] or [None]) if any(t.get("rubric") for t in turns_out) else None,
            "turns": turns_out,
            "background": vw_rec,
            "dir": D,
        }
        self.say(f"{tid}: {'PASS' if rec['success'] else 'FAIL'} {reasons[:3]} | ${rec['cost_usd']:.4f} | {rec['total_s']}s")
        return rec


def doc_checks(docs):
    """Whole-transcript checks over every chat doc a task touched (rows are deduplicated by id)."""
    handoffs = 0
    dups, repeated, texts = [], [], []
    empty_result_rows = 0
    seen_rows = set()
    for doc in docs:
        for m in doc.get("messages", []):
            if m.get("role") != "assistant" or m.get("id") in seen_rows:
                continue
            seen_rows.add(m.get("id"))
            s = json.dumps(m)
            if m.get("escalatedFromAct") or m.get("escalated_from_act") or "continue-act" in s:
                handoffs += 1
            seen = set()
            for p in m.get("parts", []):
                if p.get("kind") == "status" and not p.get("hidden"):
                    key = (p.get("code"), (p.get("message") or "").strip())
                    if key in seen and key[1]:
                        dups.append(key[1][:60])
                    seen.add(key)
            text = dr.text_of(m)
            tools = [p for p in m.get("parts", []) if p.get("kind") == "tool"]
            if text:
                texts.append(text)
                sentences = [x.strip() for x in re.split(r"(?<=[.!?])\s+", text) if len(x.strip()) > 25]
                for x in sentences:
                    if sentences.count(x) > 1 and x[:60] not in repeated:
                        repeated.append(x[:60])
            elif tools and m.get("turnRole") != "ack" and m.get("status") == "done":
                empty_result_rows += 1
    return {"handoffs": handoffs, "dup_status": dups, "empty_result_rows": empty_result_rows, "repeated_sentences": repeated, "texts": texts}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tasks", default="")
    ap.add_argument("--lanes", default="")
    ap.add_argument("--budget-usd", type=float, default=float(os.environ.get("LT_BUDGET_USD", "8")))
    ap.add_argument("--run-id", default=time.strftime("%Y%m%dT%H%M"))
    ap.add_argument("--out", default="")
    ap.add_argument("--dry-run", action="store_true", help="list the tasks per lane and exit")
    args = ap.parse_args()
    only = set(args.tasks.split(",")) if args.tasks else None
    lanes = set(args.lanes.split(",")) if args.lanes else None
    tasks = load_tasks(only, lanes)
    if args.dry_run:
        for ln in LANES:
            print(ln, [t["id"] for t in tasks if t["lane"] == ln])
        return
    rl.ensure_test_user(TEST_HANDLE, "Long-task eval")
    m = rl.machine()
    if m.get("state") != "awake":
        print("machine not awake; starting", flush=True)
        rl.ctl("machine", "start")
        for _ in range(30):
            time.sleep(10)
            if rl.machine().get("state") == "awake":
                break
    box = bx.Box()
    spent, cap = rl.spent_today()
    if cap and cap - spent < args.budget_usd:
        print(f"WARNING: daily cap headroom ${cap - spent:.2f} < budget ${args.budget_usd}; the relay will stop the agent first", flush=True)
    out_dir = Path(args.out or (HERE / "results" / args.run_id))
    out_dir.mkdir(parents=True, exist_ok=True)
    log = (out_dir / "run.log").open("a")
    run_state = {"run_id": args.run_id, "budget": args.budget_usd, "spent_at_start": spent, "lock": threading.Lock()}
    projects = {ln: rl.ensure_project(f"lane-{ln}") for ln in LANES if any(t["lane"] == ln for t in tasks)}
    meta = {
        "run_id": args.run_id,
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "image": box.image,
        "agentd": box.agentd_sha(),
        "provider": box.provider,
        "user": box.user,
        "projects": projects,
        "tasks": len(tasks),
        "budget_usd": args.budget_usd,
        "spent_today_at_start": spent,
        "judge_model": gr.JUDGE_MODEL,
    }
    print(json.dumps(meta, indent=1), flush=True)
    lanes_t = [Lane(ln, projects[ln], [t for t in tasks if t["lane"] == ln], box, run_state, out_dir, log) for ln in projects]
    for l in lanes_t:
        l.start()
    for l in lanes_t:
        l.join()
    results = [r for l in lanes_t for r in l.results]
    spent_end, _ = rl.spent_today()
    meta["ended_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    meta["spent_today_at_end"] = spent_end
    meta["run_cost_usd"] = round(spent_end - spent, 4)
    meta["judge_cost_usd"] = round(sum(r.get("judge_usd") or 0 for r in results), 4)
    board = summarize(meta, results)
    sb_dir = HERE / "scoreboard"
    sb_dir.mkdir(exist_ok=True)
    (sb_dir / f"{args.run_id}.json").write_text(json.dumps(board, indent=1))
    (out_dir / "scoreboard.json").write_text(json.dumps(board, indent=1))
    print(json.dumps({k: board[k] for k in ("summary",)}, indent=1), flush=True)
    print(f"scoreboard: {sb_dir / (args.run_id + '.json')}")


def pct(n, d):
    return round(100.0 * n / d, 1) if d else None


def median(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    n = len(xs)
    return xs[n // 2] if n % 2 else round((xs[n // 2 - 1] + xs[n // 2]) / 2, 2)


def summarize(meta, results):
    done = [r for r in results if not r.get("skipped")]
    passed = [r for r in done if r["success"]]
    cats = {}
    for r in results:
        for c in r.get("categories") or [r["category"]]:
            cats.setdefault(c, {"tasks": 0, "passed": 0})
            cats[c]["tasks"] += 1
            cats[c]["passed"] += 1 if r["success"] else 0
    for c in cats.values():
        c["pass_pct"] = pct(c["passed"], c["tasks"])
    summary = {
        "tasks": len(results),
        "run": len(done),
        "passed": len(passed),
        "failed": len(done) - len(passed),
        "skipped": len(results) - len(done),
        "pass_pct": pct(len(passed), len(done)),
        "ttfr_p50_s": median([r.get("ttfr_s") for r in done]),
        "ttfr_all_p50_s": median([t for r in done for t in (r.get("ttfr_all_s") or [])]),
        "total_p50_s": median([r.get("total_s") for r in done]),
        "tool_errors": sum(r.get("tool_errors") or 0 for r in done),
        "stalls": sum(r.get("stalls") or 0 for r in done),
        "cost_usd": round(sum(r.get("cost_usd") or 0 for r in done), 4),
        "run_cost_usd": meta.get("run_cost_usd"),
        "judge_cost_usd": meta.get("judge_cost_usd"),
        "categories": cats,
    }
    return {"meta": meta, "summary": summary, "results": results}


if __name__ == "__main__":
    main()
