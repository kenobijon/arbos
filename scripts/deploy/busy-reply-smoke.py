#!/usr/bin/env python3
"""Production smoke: a busy agent still answers a new message within seconds.

The scenario is Doug's (Jacob, 2026-10-22): the agent is in the middle of a long act turn
(here: `bash sleep`), and the user asks "what is going on now?" in the same chat and then
"hello?" in a brand-new chat. Both must get a first visible reply within --gate-seconds
(default 5) while the long turn is still running. Before the fix both waited for the long turn
to end (measured 93–98 s on the test box).

Runs against one test user on the relay (default: arbosdemo-alice) with a device minted through
the admin API and revoked at the end. Needs `arbosctl` on PATH and ARBOS_RELAY_ADMIN_TOKEN in
the environment (the deploy worker passes it). Prints one JSON report line; exit 0 = gate held,
1 = gate failed, 2 = the smoke itself could not run (no verdict).

    busy-reply-smoke.py [--relay URL] [--user USER_ID] [--gate-seconds 5] [--busy-seconds 90]
"""
import argparse
import json
import os
import re
import select
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

DEFAULT_USER = "test_01M50KGEEN5RRVNRRPFBMF2N8R"  # arbosdemo-alice (test user)
BUSY_PROMPT = (
    "Use the bash tool yourself to run exactly this command: sleep {n} && echo finished  "
    "(set the tool timeout to {t} seconds). Wait for it to end, then tell me in one line that "
    "it finished. Do it yourself with bash in this turn; do not hand it to a subagent and do not "
    "background it."
)
PROBE_SAME = "what is going on now?"
PROBE_OTHER = "hello? are you there?"


def api(relay, token, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"{relay}{path}",
        data=data,
        method=method,
        headers={"Authorization": f"Bearer {token}", "content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read()
        return r.status, (json.loads(raw) if raw.strip() else None)


class Timed:
    """Run `arbosctl chat send … --stream` and time what a user would see on screen."""

    def __init__(self, cmd, log):
        self.cmd, self.log = cmd, log
        self.t_sent = self.t_row = self.t_text = self.t_done = None
        self.rows_before, self.rows_after = set(), set()
        self.cur_row, self.first_text = None, ""

    def _handle(self, fd, data):
        now = time.time()
        text = data.decode("utf-8", "replace")
        self.log.write(f"{now:.3f} {fd} {text!r}\n")
        if fd == 2:
            if self.t_sent is None and "[sent message" in text:
                self.t_sent = now
            return
        if self.t_sent is None:
            for m in re.finditer(r"\[assistant (\w{6})\] |\[(\w{6}) · \w+: ", text):
                self.rows_before.add(m.group(1) or m.group(2))
            return
        for m in re.finditer(r"\[assistant (\w{6})\] ", text):
            rid = m.group(1)
            if rid in self.rows_before:
                self.cur_row = None
                continue
            self.rows_after.add(rid)
            self.cur_row = rid
        for m in re.finditer(r"\[(\w{6}) · (\w+): ([^\]]*)\]", text):
            rid, kind, _label = m.groups()
            if rid not in self.rows_before and kind in ("working", "queued", "deciding"):
                self.rows_after.add(rid)
                if self.t_row is None:
                    self.t_row = now
            if kind == "done" and rid in self.rows_after and self.t_done is None:
                self.t_done = now
        stripped = re.sub(r"\[assistant \w{6}\] |\[\w{6} · \w+: [^\]]*\]|\[\w{6} done [^\]]*\]|\s", "", text)
        if stripped and self.cur_row in self.rows_after and self.t_text is None:
            self.t_text = now
            self.first_text = re.sub(r"\[[^\]]*\]", "", text).strip()[:200]

    def run(self):
        p = subprocess.Popen(self.cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        fds = {p.stdout.fileno(): 1, p.stderr.fileno(): 2}
        while fds:
            r, _, _ = select.select(list(fds), [], [], 0.2)
            for f in r:
                d = os.read(f, 65536)
                if d:
                    self._handle(fds[f], d)
                else:
                    del fds[f]
        p.wait()
        t_exit = time.time()

        def dl(t):
            return None if (t is None or self.t_sent is None) else round(t - self.t_sent, 2)

        return {
            "row_s": dl(self.t_row),
            "first_text_s": dl(self.t_text),
            "done_s": dl(self.t_done) if self.t_done else dl(t_exit),
            "first_text": self.first_text,
            "exit": p.returncode,
        }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--relay", default=os.environ.get("RELAY_URL", "https://relay.arbos.life"))
    ap.add_argument("--user", default=os.environ.get("SMOKE_USER_ID", DEFAULT_USER))
    ap.add_argument("--gate-seconds", type=float, default=float(os.environ.get("SMOKE_GATE_S", "5")))
    ap.add_argument("--busy-seconds", type=int, default=90)
    ap.add_argument("--probe-delay", type=float, default=10.0)
    ap.add_argument("--log", default=None, help="append the raw arbosctl stream here")
    args = ap.parse_args()
    admin = os.environ.get("ARBOS_RELAY_ADMIN_TOKEN", "").strip()
    if not admin:
        print(json.dumps({"ok": None, "error": "ARBOS_RELAY_ADMIN_TOKEN missing"}))
        return 2
    relay = args.relay.rstrip("/")
    work = tempfile.mkdtemp(prefix="busy-reply-smoke-")
    log = open(args.log or os.path.join(work, "stream.log"), "a")
    device = None
    token = None
    report = {"ok": None, "relay": relay, "user": args.user, "gate_s": args.gate_seconds}
    try:
        st, dev = api(relay, admin, "POST", f"/admin/users/{args.user}/devices",
                      {"name": "busy-reply smoke (deploy worker; revoked at the end)"})
        if st != 200 and st != 201:
            report["error"] = f"device mint HTTP {st}"
            print(json.dumps(report))
            return 2
        token, device = dev["access_token"], dev["device_id"]
        cfgs = []
        for name in ("busy", "same", "other"):
            d = os.path.join(work, name)
            os.makedirs(d)
            path = os.path.join(d, "arbosctl.json")
            with open(path, "w") as fh:
                json.dump({"relay_url": relay, "device_token": token, "device_id": device}, fh)
            os.chmod(path, 0o600)
            cfgs.append(path)
        _, projects = api(relay, token, "GET", "/projects")
        projects = projects if isinstance(projects, list) else (projects or {}).get("projects", [])
        if not projects:
            report["error"] = "the smoke user has no project"
            print(json.dumps(report))
            return 2
        project = projects[0]["id"]
        report["project"] = project
        env = dict(os.environ)

        def open_chat(cfg, title):
            env["ARBOSCTL_CONFIG"] = cfg
            out = subprocess.run(["arbosctl", "chat", "open", project, "--title", title],
                                 env=env, capture_output=True, text=True, timeout=60)
            cid = out.stdout.strip().splitlines()[-1].strip() if out.stdout.strip() else ""
            if not re.fullmatch(r"[0-9A-Z]{26}", cid):
                raise RuntimeError(f"chat open failed: {out.stdout} {out.stderr}")
            return cid

        stamp = time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime())
        chat1 = open_chat(cfgs[0], f"busy-reply smoke {stamp}")
        busy_env = dict(env, ARBOSCTL_CONFIG=cfgs[0])
        busy = subprocess.Popen(
            ["arbosctl", "chat", "send", project, chat1,
             BUSY_PROMPT.format(n=args.busy_seconds, t=args.busy_seconds + 60),
             "--stream", "--timeout", str(args.busy_seconds + 240)],
            env=busy_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(args.probe_delay)
        # Each probe runs under its own arbosctl config (own room replica), via os.environ.
        os.environ["ARBOSCTL_CONFIG"] = cfgs[1]
        same = Timed(["arbosctl", "chat", "send", project, chat1, PROBE_SAME,
                      "--stream", "--timeout", "120"], log)
        r_same = same.run()
        chat2 = open_chat(cfgs[2], f"busy-reply smoke (other chat) {stamp}")
        os.environ["ARBOSCTL_CONFIG"] = cfgs[2]
        other = Timed(["arbosctl", "chat", "send", project, chat2, PROBE_OTHER,
                       "--stream", "--timeout", "120"], log)
        r_other = other.run()
        busy_still_running = busy.poll() is None
        try:
            busy.wait(timeout=args.busy_seconds + 300)
        except subprocess.TimeoutExpired:
            busy.kill()
        report.update({
            "chat_busy": chat1,
            "chat_other": chat2,
            "same_chat": r_same,
            "other_chat": r_other,
            "busy_turn_still_running_at_probes": busy_still_running,
        })
        verdicts = []
        for name, r in (("same_chat", r_same), ("other_chat", r_other)):
            ft = r.get("first_text_s")
            ok = ft is not None and ft <= args.gate_seconds and bool(r.get("first_text"))
            verdicts.append(ok)
            report[f"{name}_ok"] = ok
        report["ok"] = all(verdicts) and busy_still_running
        if not busy_still_running:
            report["note"] = "the long turn ended before the probes; the gate was not exercised"
        print(json.dumps(report))
        return 0 if report["ok"] else 1
    except Exception as e:  # noqa: BLE001 — one report line, whatever broke
        report["error"] = str(e)[:400]
        print(json.dumps(report))
        return 2
    finally:
        if token and device:
            try:
                api(relay, token, "DELETE", f"/auth/devices/{device}")
            except (urllib.error.URLError, urllib.error.HTTPError, OSError):
                pass
        log.close()


if __name__ == "__main__":
    sys.exit(main())
