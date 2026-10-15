#!/usr/bin/env python3
"""Arbos feedback loop: TestFlight screenshot feedback -> Cursor cloud agent -> main -> TestFlight.

Runs every 2 minutes from arbos-feedback-poller.timer (see install-feedback-loop.sh). One tick:

  1. Stop at once when the kill file (/etc/arbos/feedback.disabled) exists.
  2. List new TestFlight screenshot feedback for the app through the App Store Connect API
     (betaFeedbackScreenshotSubmissions) and pick up synthetic items from <state>/inbox/.
  3. Save every new item under <state>/<id>/ (submission.json, screenshot-N.jpg|png).
     Items whose tester email is not on the allow-list are logged and ignored.
  4. Move every open item one step along its state machine:
       queued  -> running   open the GitHub issue, upload the screenshot, start a Cursor agent
                            (rate limit: one launch per FEEDBACK_MIN_INTERVAL_S, FEEDBACK_MAX_PER_DAY a day)
       running -> pushed    the agent's run finished and a commit tagged [feedback <id>] is on main
               -> failed    the run errored, timed out, or finished without a push
       pushed  -> shipped   ios.yml (and release.yml when it runs) succeeded for that commit and the
                            TestFlight build is processed; the issue gets build number + elapsed time
               -> running   ios.yml failed: one follow-up run with the compiler errors (once)
               -> failed    a second failure, or nothing shipped within the wait budget
  5. Append one JSON line per transition to <state>/log.jsonl.

Config comes from the environment (systemd EnvironmentFile=/etc/arbos/feedback.env, root only):
  ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_P8_B64 (base64 of the .p8), ASC_APP_ID
  CURSOR_API_KEY, FEEDBACK_AGENT_MODEL (optional Cursor model id)
  GITHUB_TOKEN, GITHUB_REPO (owner/name)
  FEEDBACK_ALLOWLIST (comma-separated emails) + FEEDBACK_ALLOWLIST_FILE (one email per line)
  FEEDBACK_STATE_DIR (default /var/lib/arbos-feedback), FEEDBACK_KILL_FILE
  FEEDBACK_MIN_INTERVAL_S (600), FEEDBACK_MAX_PER_DAY (6)
  FEEDBACK_AGENT_TIMEOUT_MIN (75), FEEDBACK_SHIP_TIMEOUT_MIN (90)

Only the standard library plus `cryptography` (for the ES256 App Store Connect token).

CLI:  feedback_poller.py [run] | status | set-cursor <ISO-8601> | inject <image> "<comment>" [ios|mac]
"""

from __future__ import annotations

import base64
import contextlib
import datetime as dt
import fcntl
import json
import os
import re
import shutil
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

ASC_API = "https://api.appstoreconnect.apple.com/v1"
CURSOR_API = "https://api.cursor.com/v1"
GITHUB_API = "https://api.github.com"
ASSET_RELEASE_TAG = "feedback-screenshots"
USER_AGENT = "arbos-feedback-poller/1 (+https://github.com/unarbos/arbos)"

TERMINAL = {"shipped", "failed", "ignored"}


def utcnow() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc)


def iso(t: dt.datetime | None = None) -> str:
    return (t or utcnow()).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(s: str) -> dt.datetime:
    s = s.strip()
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    t = dt.datetime.fromisoformat(s)
    if t.tzinfo is None:
        t = t.replace(tzinfo=dt.timezone.utc)
    return t.astimezone(dt.timezone.utc)


def minutes_between(a: str, b: str) -> float:
    return round((parse_iso(b) - parse_iso(a)).total_seconds() / 60, 1)


def log(msg: str) -> None:
    print(f"{iso()} {msg}", flush=True)


# ---------------------------------------------------------------- config


@dataclass
class Config:
    state_dir: Path
    kill_file: Path
    asc_key_id: str
    asc_issuer_id: str
    asc_key_pem: bytes
    asc_app_id: str
    cursor_api_key: str
    agent_model: str
    github_token: str
    github_repo: str
    allowlist: set[str]
    min_interval_s: int
    max_per_day: int
    agent_timeout_min: int
    ship_timeout_min: int

    @staticmethod
    def from_env() -> "Config":
        env = os.environ
        state_dir = Path(env.get("FEEDBACK_STATE_DIR", "/var/lib/arbos-feedback"))
        allow = {e.strip().lower() for e in env.get("FEEDBACK_ALLOWLIST", "").split(",") if e.strip()}
        allow_file = Path(env.get("FEEDBACK_ALLOWLIST_FILE", "/etc/arbos/feedback-allowlist.txt"))
        if allow_file.is_file():
            for line in allow_file.read_text().splitlines():
                line = line.split("#", 1)[0].strip().lower()
                if line:
                    allow.add(line)
        p8_b64 = env.get("ASC_KEY_P8_B64", "")
        pem = base64.b64decode(p8_b64) if p8_b64 else b""
        return Config(
            state_dir=state_dir,
            kill_file=Path(env.get("FEEDBACK_KILL_FILE", "/etc/arbos/feedback.disabled")),
            asc_key_id=env.get("ASC_KEY_ID", ""),
            asc_issuer_id=env.get("ASC_ISSUER_ID", ""),
            asc_key_pem=pem,
            asc_app_id=env.get("ASC_APP_ID", "6812503407"),
            cursor_api_key=env.get("CURSOR_API_KEY", ""),
            agent_model=env.get("FEEDBACK_AGENT_MODEL", ""),
            github_token=env.get("GITHUB_TOKEN", ""),
            github_repo=env.get("GITHUB_REPO", "unarbos/arbos"),
            allowlist=allow,
            min_interval_s=int(env.get("FEEDBACK_MIN_INTERVAL_S", "600")),
            max_per_day=int(env.get("FEEDBACK_MAX_PER_DAY", "6")),
            agent_timeout_min=int(env.get("FEEDBACK_AGENT_TIMEOUT_MIN", "75")),
            ship_timeout_min=int(env.get("FEEDBACK_SHIP_TIMEOUT_MIN", "90")),
        )


# ---------------------------------------------------------------- http


class HttpError(Exception):
    def __init__(self, status: int, body: str, url: str):
        super().__init__(f"HTTP {status} for {url}: {body[:300]}")
        self.status = status
        self.body = body


def http(method: str, url: str, headers: dict[str, str] | None = None, data: bytes | None = None,
         json_body: Any = None, timeout: int = 60) -> tuple[int, dict[str, str], bytes]:
    hdrs = {"User-Agent": USER_AGENT}
    if headers:
        hdrs.update(headers)
    if json_body is not None:
        data = json.dumps(json_body).encode()
        hdrs.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=data, method=method, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        raise HttpError(e.code, body, url) from None


def http_json(method: str, url: str, **kw: Any) -> Any:
    status, _, body = http(method, url, **kw)
    if status == 204 or not body:
        return None
    return json.loads(body)


# ---------------------------------------------------------------- App Store Connect


class Asc:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self._token = ""
        self._token_exp = 0.0

    def token(self) -> str:
        now = time.time()
        if self._token and now < self._token_exp - 60:
            return self._token

        def b64(b: bytes) -> str:
            return base64.urlsafe_b64encode(b).rstrip(b"=").decode()

        key = serialization.load_pem_private_key(self.cfg.asc_key_pem, None)
        header = b64(json.dumps({"alg": "ES256", "kid": self.cfg.asc_key_id, "typ": "JWT"}).encode())
        exp = int(now) + 900
        payload = b64(json.dumps({"iss": self.cfg.asc_issuer_id, "iat": int(now), "exp": exp,
                                  "aud": "appstoreconnect-v1"}).encode())
        sig = key.sign(f"{header}.{payload}".encode(), ec.ECDSA(hashes.SHA256()))
        r, s = decode_dss_signature(sig)
        self._token = f"{header}.{payload}.{b64(r.to_bytes(32, 'big') + s.to_bytes(32, 'big'))}"
        self._token_exp = exp
        return self._token

    def get(self, path_or_url: str) -> Any:
        url = path_or_url if path_or_url.startswith("http") else f"{ASC_API}{path_or_url}"
        return http_json("GET", url, headers={"Authorization": f"Bearer {self.token()}"})

    def screenshot_submissions(self, limit: int = 50) -> tuple[list[dict], dict[str, dict]]:
        q = urllib.parse.urlencode({"sort": "-createdDate", "limit": limit, "include": "build"})
        doc = self.get(f"/apps/{self.cfg.asc_app_id}/betaFeedbackScreenshotSubmissions?{q}")
        builds = {b["id"]: b for b in doc.get("included", []) if b.get("type") == "builds"}
        return doc.get("data", []), builds

    def build_state(self, version: str) -> dict | None:
        q = urllib.parse.urlencode({"filter[app]": self.cfg.asc_app_id, "filter[version]": version,
                                    "fields[builds]": "version,processingState,uploadedDate,expired", "limit": 5})
        doc = self.get(f"/builds?{q}")
        for b in doc.get("data", []):
            if b["attributes"].get("version") == version:
                return b["attributes"]
        return None


# ---------------------------------------------------------------- GitHub


class GitHub:
    def __init__(self, cfg: Config):
        self.repo = cfg.github_repo
        self.h = {"Authorization": f"Bearer {cfg.github_token}", "Accept": "application/vnd.github+json",
                  "X-GitHub-Api-Version": "2022-11-28"}

    def api(self, method: str, path: str, **kw: Any) -> Any:
        url = path if path.startswith("http") else f"{GITHUB_API}{path}"
        return http_json(method, url, headers=self.h, **kw)

    def repo_api(self, method: str, path: str, **kw: Any) -> Any:
        return self.api(method, f"/repos/{self.repo}{path}", **kw)

    def ensure_label(self, name: str, color: str, description: str) -> None:
        try:
            self.repo_api("GET", f"/labels/{urllib.parse.quote(name)}")
        except HttpError as e:
            if e.status != 404:
                raise
            self.repo_api("POST", "/labels", json_body={"name": name, "color": color, "description": description})

    def create_issue(self, title: str, body: str, labels: list[str]) -> dict:
        return self.repo_api("POST", "/issues", json_body={"title": title, "body": body, "labels": labels})

    def comment(self, number: int, body: str) -> None:
        self.repo_api("POST", f"/issues/{number}/comments", json_body={"body": body})

    def set_labels(self, number: int, add: list[str], remove: list[str]) -> None:
        for name in remove:
            with contextlib.suppress(HttpError):
                self.repo_api("DELETE", f"/issues/{number}/labels/{urllib.parse.quote(name)}")
        if add:
            self.repo_api("POST", f"/issues/{number}/labels", json_body={"labels": add})

    def asset_release(self) -> dict:
        try:
            return self.repo_api("GET", f"/releases/tags/{ASSET_RELEASE_TAG}")
        except HttpError as e:
            if e.status != 404:
                raise
        return self.repo_api("POST", "/releases", json_body={
            "tag_name": ASSET_RELEASE_TAG, "target_commitish": "main", "name": "Feedback screenshots",
            "prerelease": True, "draft": False,
            "body": "Screenshots attached to `[feedback …]` issues by scripts/feedback/feedback_poller.py. "
                    "Not a software release.",
        })

    def upload_asset(self, path: Path, name: str, content_type: str) -> str:
        rel = self.asset_release()
        for a in rel.get("assets", []):
            if a["name"] == name:
                return a["browser_download_url"]
        upload_url = rel["upload_url"].split("{", 1)[0] + "?" + urllib.parse.urlencode({"name": name})
        hdrs = dict(self.h)
        hdrs["Content-Type"] = content_type
        try:
            asset = http_json("POST", upload_url, headers=hdrs, data=path.read_bytes(), timeout=180)
            return asset["browser_download_url"]
        except HttpError as e:
            if e.status != 422:
                raise
            rel = self.repo_api("GET", f"/releases/tags/{ASSET_RELEASE_TAG}")
            for a in rel.get("assets", []):
                if a["name"] == name:
                    return a["browser_download_url"]
            raise

    def commits_since(self, since_iso: str) -> list[dict]:
        q = urllib.parse.urlencode({"sha": "main", "since": since_iso, "per_page": 100})
        return self.repo_api("GET", f"/commits?{q}")

    def commit(self, sha: str) -> dict:
        return self.repo_api("GET", f"/commits/{sha}")

    def runs_for_sha(self, sha: str) -> list[dict]:
        q = urllib.parse.urlencode({"head_sha": sha, "per_page": 50})
        return self.repo_api("GET", f"/actions/runs?{q}").get("workflow_runs", [])

    def run_artifacts(self, run_id: int) -> list[dict]:
        return self.repo_api("GET", f"/actions/runs/{run_id}/artifacts").get("artifacts", [])

    def run_error_lines(self, run_id: int, limit: int = 40) -> list[str]:
        jobs = self.repo_api("GET", f"/actions/runs/{run_id}/jobs").get("jobs", [])
        lines: list[str] = []
        for job in jobs:
            if job.get("conclusion") != "failure":
                continue
            try:
                status, hdrs, body = http("GET", f"{GITHUB_API}/repos/{self.repo}/actions/jobs/{job['id']}/logs",
                                          headers=self.h, timeout=120)
            except HttpError:
                continue
            text = body.decode(errors="replace")
            for line in text.splitlines():
                line = re.sub(r"^\S+\s", "", line)  # strip the timestamp column
                if re.search(r"\berror\b|\*\* (ARCHIVE|EXPORT|BUILD) FAILED", line, re.I) and line not in lines:
                    lines.append(line.strip()[:400])
                if len(lines) >= limit:
                    return lines
        return lines

    def releases(self) -> list[dict]:
        return self.repo_api("GET", "/releases?per_page=15")


# ---------------------------------------------------------------- Cursor


class Cursor:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        tok = base64.b64encode(f"{cfg.cursor_api_key}:".encode()).decode()
        self.h = {"Authorization": f"Basic {tok}"}

    def create_agent(self, name: str, prompt: str, images: list[dict]) -> dict:
        body: dict[str, Any] = {
            "prompt": {"text": prompt, "images": images},
            "name": name[:100],
            "repos": [{"url": f"https://github.com/{self.cfg.github_repo}", "startingRef": "main"}],
            "workOnCurrentBranch": True,
            "autoCreatePR": False,
        }
        if self.cfg.agent_model:
            body["model"] = {"id": self.cfg.agent_model}
        return http_json("POST", f"{CURSOR_API}/agents", headers=self.h, json_body=body, timeout=120)

    def followup(self, agent_id: str, prompt: str) -> dict:
        return http_json("POST", f"{CURSOR_API}/agents/{agent_id}/runs", headers=self.h,
                         json_body={"prompt": {"text": prompt}}, timeout=120)

    def run(self, agent_id: str, run_id: str) -> dict:
        return http_json("GET", f"{CURSOR_API}/agents/{agent_id}/runs/{run_id}", headers=self.h)

    def cancel(self, agent_id: str, run_id: str) -> None:
        with contextlib.suppress(HttpError):
            http_json("POST", f"{CURSOR_API}/agents/{agent_id}/runs/{run_id}/cancel", headers=self.h)


# ---------------------------------------------------------------- images


def image_dims(path: Path) -> tuple[int, int] | None:
    data = path.read_bytes()
    if data[:8] == b"\x89PNG\r\n\x1a\n" and len(data) > 24:
        w, h = struct.unpack(">II", data[16:24])
        return w, h
    if data[:2] == b"\xff\xd8":
        i = 2
        while i + 9 < len(data):
            if data[i] != 0xFF:
                i += 1
                continue
            marker = data[i + 1]
            if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
                i += 2
                continue
            seg_len = struct.unpack(">H", data[i + 2:i + 4])[0]
            if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                h, w = struct.unpack(">HH", data[i + 5:i + 9])
                return w, h
            i += 2 + seg_len
    return None


def mime_for(path: Path) -> str:
    return {"png": "image/png", "jpg": "image/jpeg", "jpeg": "image/jpeg", "gif": "image/gif",
            "webp": "image/webp"}.get(path.suffix.lower().lstrip("."), "image/jpeg")


# ---------------------------------------------------------------- state


class Store:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.root = cfg.state_dir
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "inbox").mkdir(exist_ok=True)

    # cursor: {"since": ISO, "seen": [ids]}
    def cursor(self) -> dict:
        p = self.root / "cursor.json"
        if p.is_file():
            return json.loads(p.read_text())
        return {"since": iso(), "seen": []}

    def save_cursor(self, c: dict) -> None:
        c["seen"] = c["seen"][-500:]
        self._write(self.root / "cursor.json", c)

    def item_dir(self, fid: str) -> Path:
        return self.root / fid

    def items(self) -> list[str]:
        out = []
        for d in self.root.iterdir():
            if d.is_dir() and d.name != "inbox" and (d / "state.json").is_file():
                out.append(d.name)
        return sorted(out, key=lambda n: self.state(n).get("created", ""))

    def state(self, fid: str) -> dict:
        p = self.item_dir(fid) / "state.json"
        return json.loads(p.read_text()) if p.is_file() else {}

    def save_state(self, fid: str, st: dict) -> None:
        st["updated"] = iso()
        self._write(self.item_dir(fid) / "state.json", st)

    def submission(self, fid: str) -> dict:
        return json.loads((self.item_dir(fid) / "submission.json").read_text())

    def screenshots(self, fid: str) -> list[Path]:
        return sorted(p for p in self.item_dir(fid).glob("screenshot-*") if p.suffix.lower() in
                      (".jpg", ".jpeg", ".png", ".gif", ".webp"))

    def event(self, fid: str, kind: str, **fields: Any) -> None:
        rec = {"time": iso(), "id": fid, "event": kind, **fields}
        with (self.root / "log.jsonl").open("a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        log(f"[{fid}] {kind} {json.dumps(fields, ensure_ascii=False) if fields else ''}")

    def launches_today(self) -> int:
        today = utcnow().strftime("%Y-%m-%d")
        return sum(1 for ts in self._launches() if ts.startswith(today))

    def last_launch(self) -> dt.datetime | None:
        ls = self._launches()
        return parse_iso(ls[-1]) if ls else None

    def record_launch(self) -> None:
        ls = self._launches()
        ls.append(iso())
        self._write(self.root / "launches.json", ls[-200:])

    def _launches(self) -> list[str]:
        p = self.root / "launches.json"
        return json.loads(p.read_text()) if p.is_file() else []

    @staticmethod
    def _write(path: Path, obj: Any) -> None:
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(obj, indent=2, ensure_ascii=False) + "\n")
        os.replace(tmp, path)


# ---------------------------------------------------------------- the loop


class Loop:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.store = Store(cfg)
        self.asc = Asc(cfg)
        self.gh = GitHub(cfg)
        self.cursor = Cursor(cfg)
        self.prompt_template = (Path(__file__).resolve().parent / "agent-prompt.md").read_text()

    # ---- intake -------------------------------------------------------------------------------

    def intake(self) -> None:
        self.intake_inbox()
        try:
            self.intake_asc()
        except Exception as e:  # noqa: BLE001 - keep the loop alive, report in the journal
            log(f"ASC intake failed: {e}")

    def intake_asc(self) -> None:
        cur = self.store.cursor()
        since = parse_iso(cur["since"])
        subs, builds = self.asc.screenshot_submissions()
        newest = since
        for sub in sorted(subs, key=lambda s: s["attributes"]["createdDate"]):
            fid = sub["id"]
            created = parse_iso(sub["attributes"]["createdDate"])
            if created < since or fid in cur["seen"] or self.store.item_dir(fid).exists():
                continue
            newest = max(newest, created)
            cur["seen"].append(fid)
            try:
                self.save_asc_submission(sub, builds)
            except Exception as e:  # noqa: BLE001
                log(f"[{fid}] could not save submission: {e}")
                cur["seen"].remove(fid)
                shutil.rmtree(self.store.item_dir(fid), ignore_errors=True)
        cur["since"] = iso(newest)
        cur["checked"] = iso()
        self.store.save_cursor(cur)

    def save_asc_submission(self, sub: dict, builds: dict[str, dict]) -> None:
        fid = sub["id"]
        a = sub["attributes"]
        d = self.store.item_dir(fid)
        d.mkdir(parents=True, exist_ok=True)
        build_id = (sub.get("relationships", {}).get("build", {}).get("data") or {}).get("id")
        build = builds.get(build_id, {}).get("attributes", {}) if build_id else {}
        shots = []
        for n, s in enumerate(a.get("screenshots") or [], start=1):
            url = s["url"]
            status, hdrs, body = http("GET", url, timeout=120)
            ext = "png" if body[:8] == b"\x89PNG\r\n\x1a\n" else "jpg"
            p = d / f"screenshot-{n}.{ext}"
            p.write_bytes(body)
            shots.append({"file": p.name, "width": s.get("width"), "height": s.get("height")})
        email = (a.get("email") or "").strip().lower()
        info = {
            "id": fid, "source": "testflight", "created": a["createdDate"], "comment": a.get("comment") or "",
            "email": email, "device": a.get("deviceModel"), "os": a.get("osVersion"),
            "device_platform": a.get("devicePlatform"), "device_family": a.get("deviceFamily"),
            "app_platform": a.get("appPlatform"), "bundle": a.get("buildBundleId"),
            "build": build.get("version"), "build_id": build_id, "locale": a.get("locale"),
            "screen_points": [a.get("screenWidthInPoints"), a.get("screenHeightInPoints")],
            "screenshots": shots, "raw": a,
        }
        (d / "submission.json").write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n")
        self.admit(info)

    def intake_inbox(self) -> None:
        inbox = self.store.root / "inbox"
        for entry in sorted(inbox.iterdir()):
            if not entry.is_dir():
                continue
            images = sorted(p for p in entry.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".gif", ".webp"))
            comment_file = entry / "comment.txt"
            if not images or not comment_file.is_file():
                continue
            stamp = utcnow().strftime("%Y%m%d%H%M%S")
            fid = f"local-{re.sub(r'[^A-Za-z0-9_-]', '-', entry.name)[:40]}-{stamp}"
            d = self.store.item_dir(fid)
            d.mkdir(parents=True)
            shots = []
            for n, img in enumerate(images, start=1):
                target = d / f"screenshot-{n}{img.suffix.lower()}"
                shutil.copy2(img, target)
                dims = image_dims(target)
                shots.append({"file": target.name, "width": dims[0] if dims else None,
                              "height": dims[1] if dims else None})
            email = (entry / "email.txt").read_text().strip().lower() if (entry / "email.txt").is_file() else "inbox@server"
            forced = (entry / "target.txt").read_text().strip().lower() if (entry / "target.txt").is_file() else ""
            info = {
                "id": fid, "source": "inbox", "created": iso(), "comment": comment_file.read_text().strip(),
                "email": email, "device": "synthetic", "os": None, "device_platform": None,
                "device_family": None, "app_platform": None, "bundle": None, "build": None,
                "build_id": None, "locale": None, "screen_points": None, "screenshots": shots,
                "forced_target": forced or None, "raw": {},
            }
            (d / "submission.json").write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n")
            shutil.rmtree(entry)
            self.admit(info)

    def admit(self, info: dict) -> None:
        fid = info["id"]
        trusted = info["source"] == "inbox" or info["email"] in self.cfg.allowlist
        st = {"id": fid, "created": info["created"], "status": "queued" if trusted else "ignored",
              "target": self.pick_target(info), "retries": 0}
        self.store.save_state(fid, st)
        if trusted:
            self.store.event(fid, "queued", email=info["email"], target=st["target"],
                             comment=info["comment"][:200])
        else:
            self.store.event(fid, "ignored", reason="email not on the allow-list", email=info["email"])

    @staticmethod
    def pick_target(info: dict) -> str:
        if info.get("forced_target") in ("ios", "mac"):
            return info["forced_target"]
        if (info.get("device_platform") or "").upper() == "MAC_OS" or (info.get("app_platform") or "").upper() == "MAC_OS":
            return "mac"
        if re.search(r"\bmac(os|book)?\b", info.get("comment") or "", re.I):
            return "mac"
        if info.get("source") == "inbox":
            for s in info.get("screenshots", []):
                w, h = s.get("width") or 0, s.get("height") or 0
                if w >= 1400 and h and w / h >= 1.3:
                    return "mac"
        return "ios"

    # ---- state machine ------------------------------------------------------------------------

    def advance_all(self) -> None:
        for fid in self.store.items():
            st = self.store.state(fid)
            if st.get("status") in TERMINAL:
                continue
            try:
                self.advance(fid, st)
            except Exception as e:  # noqa: BLE001
                log(f"[{fid}] step failed: {e}")

    def advance(self, fid: str, st: dict) -> None:
        status = st["status"]
        if status == "queued":
            self.launch(fid, st)
        elif status == "running":
            self.check_agent(fid, st)
        elif status == "pushed":
            self.check_ship(fid, st)

    def rate_limit_reason(self) -> str | None:
        last = self.store.last_launch()
        if last and (utcnow() - last).total_seconds() < self.cfg.min_interval_s:
            return f"last launch {int((utcnow() - last).total_seconds())}s ago (< {self.cfg.min_interval_s}s)"
        if self.store.launches_today() >= self.cfg.max_per_day:
            return f"{self.cfg.max_per_day} launches already today"
        return None

    def target_info(self, target: str) -> dict:
        if target == "mac":
            return {"name": "Mac app (GPUI)", "path": "crates/arbos-mac",
                    "checks": ("Run `cargo fmt --all --check`, `cargo clippy -p arbos-mac --all-targets --locked -- -D warnings`, "
                               "and `cargo build -p arbos-mac` (the Linux build needs: `sudo apt-get install -y libxkbcommon-dev "
                               "libxkbcommon-x11-dev libwayland-dev libxcb-xkb-dev libasound2-dev libssl-dev libgl1-mesa-dev "
                               "libfontconfig-dev`). All three must pass before you push; `cargo test -p arbos-mac` if tests exist "
                               "for the file you touched.")}
        return {"name": "iPhone app (SwiftUI)", "path": "apps/ios/Arbos",
                "checks": ("You are on a Linux machine with no Xcode, so you cannot compile Swift here. Compensate: re-read every "
                           "line you changed against the surrounding code (types, optionals, modifiers, closing braces), keep the "
                           "change tiny, and do not touch the Xcode project file. If `swiftc` or `swift-format` is installed, run "
                           "`swiftc -parse` on the files you changed. CI (`.github/workflows/ios.yml`) builds and uploads the app "
                           "to TestFlight after your push; a broken build comes back to you as a follow-up with the errors.")}

    def build_prompt(self, fid: str, info: dict, st: dict, issue_url: str) -> str:
        t = self.target_info(st["target"])
        dims = ", ".join(f"{s.get('width')}x{s.get('height')}" for s in info.get("screenshots", []) if s.get("width"))
        src = "TestFlight screenshot feedback" if info["source"] == "testflight" else "synthetic item placed in the poller inbox"
        repl = {
            "FEEDBACK_ID": fid, "COMMENT": info["comment"].strip() or "(no comment)", "SOURCE": src,
            "DEVICE": info.get("device") or "?", "OS_VERSION": info.get("os") or "?",
            "APP_BUILD": info.get("build") or "?", "SCREENSHOT_DIMS": dims or "unknown size",
            "TARGET_NAME": t["name"], "TARGET_PATH": t["path"], "CHECKS": t["checks"], "ISSUE_URL": issue_url,
        }
        out = self.prompt_template
        for k, v in repl.items():
            out = out.replace("{{" + k + "}}", str(v))
        return out

    def issue_title(self, fid: str, info: dict) -> str:
        summary = re.sub(r"\s+", " ", info["comment"]).strip() or "screenshot feedback"
        if len(summary) > 70:
            summary = summary[:67].rstrip() + "…"
        return f"[feedback {fid}] {summary}"

    def launch(self, fid: str, st: dict) -> None:
        reason = self.rate_limit_reason()
        if reason:
            if st.get("deferred_reason") != reason.split(" (")[0]:
                st["deferred_reason"] = reason.split(" (")[0]
                self.store.save_state(fid, st)
                log(f"[{fid}] deferred: {reason}")
            return
        info = self.store.submission(fid)
        shots = self.store.screenshots(fid)
        if not shots:
            st.update(status="failed", error="no screenshot file")
            self.store.save_state(fid, st)
            self.store.event(fid, "failed", error="no screenshot file")
            return

        # 1. screenshot assets + issue
        asset_urls = []
        for n, p in enumerate(shots, start=1):
            asset_urls.append(self.gh.upload_asset(p, f"{fid}-{n}{p.suffix.lower()}", mime_for(p)))
        t = self.target_info(st["target"])
        self.gh.ensure_label("feedback", "0E8A16", "TestFlight screenshot feedback handled by the feedback loop")
        self.gh.ensure_label("shipped", "1D76DB", "Fix is in a TestFlight / edge build")
        self.gh.ensure_label("needs-human", "D93F0B", "The automatic fix did not land")
        body_lines = [
            f"**Comment:** {info['comment'].strip() or '(none)'}",
            "",
            *[f"![screenshot {n}]({u})" for n, u in enumerate(asset_urls, start=1)],
            "",
            "| | |", "|---|---|",
            f"| Feedback id | `{fid}` |",
            f"| Source | {'TestFlight' if info['source'] == 'testflight' else 'synthetic (poller inbox)'} |",
            f"| Tester | {info['email']} |",
            f"| Device | {info.get('device') or '?'} · {info.get('device_platform') or ''} {info.get('os') or ''} |",
            f"| App build | {info.get('build') or '?'} |",
            f"| Submitted | {info['created']} |",
            f"| Target | {t['name']} (`{t['path']}`) |",
            "",
            "_Opened by `scripts/feedback/feedback_poller.py`. Progress is posted as comments; "
            "the fix lands on `main` as `fix(feedback): … [feedback " + fid + "]`._",
        ]
        issue = self.gh.create_issue(self.issue_title(fid, info), "\n".join(body_lines), ["feedback"])
        st.update(issue=issue["number"], issue_url=issue["html_url"], screenshot_urls=asset_urls)
        self.store.save_state(fid, st)

        # 2. Cursor agent
        images = []
        for p in shots[:5]:
            images.append({"data": base64.b64encode(p.read_bytes()).decode(), "mimeType": mime_for(p)})
        prompt = self.build_prompt(fid, info, st, issue["html_url"])
        (self.store.item_dir(fid) / "prompt.md").write_text(prompt)
        self.store.record_launch()
        try:
            created = self.cursor.create_agent(f"feedback {fid}: {info['comment'][:60]}", prompt, images)
        except HttpError as e:
            st.update(status="failed", error=f"cursor create failed: HTTP {e.status} {e.body[:200]}")
            self.store.save_state(fid, st)
            self.store.event(fid, "failed", error=st["error"])
            self.gh.comment(issue["number"], f"Could not start the Cursor agent: `HTTP {e.status}` {e.body[:300]}\n\n"
                            "This item needs a human. Label `needs-human` set.")
            self.gh.set_labels(issue["number"], ["needs-human"], [])
            return
        agent, run = created["agent"], created["run"]
        st.update(status="running", agent_id=agent["id"], agent_url=agent["url"], run_id=run["id"],
                  launched=iso(), model=self.cfg.agent_model or "default")
        st.pop("deferred_reason", None)
        self.store.save_state(fid, st)
        self.store.event(fid, "agent_started", agent=agent["url"], issue=issue["html_url"], target=st["target"])
        self.gh.comment(issue["number"], f"Cursor agent started: {agent['url']} (target `{t['path']}`).")

    def check_agent(self, fid: str, st: dict) -> None:
        run = self.cursor.run(st["agent_id"], st["run_id"])
        status = run.get("status")
        st["run_status"] = status
        if status in ("CREATING", "RUNNING", "PENDING", "QUEUED"):
            if minutes_between(st["launched"], iso()) > self.cfg.agent_timeout_min:
                self.cursor.cancel(st["agent_id"], st["run_id"])
                self.fail(fid, st, f"agent run exceeded {self.cfg.agent_timeout_min} min; cancelled")
            else:
                self.store.save_state(fid, st)
            return
        result = (run.get("result") or "").strip()
        st["agent_result"] = result[:4000]
        if status != "FINISHED":
            self.fail(fid, st, f"agent run ended with {status}", extra=result)
            return
        commit = self.find_commit(fid, st["launched"])
        if not commit:
            self.fail(fid, st, "agent finished without pushing a [feedback] commit to main", extra=result)
            return
        files = [f["filename"] for f in self.gh.commit(commit["sha"]).get("files", [])]
        st.update(status="pushed", commit=commit["sha"], commit_url=commit["html_url"],
                  commit_message=commit["commit"]["message"].splitlines()[0], pushed_at=iso(), files=files,
                  expects_ios=any(f.startswith("apps/ios/") or f == ".github/workflows/ios.yml" for f in files),
                  expects_ci=any(not f.startswith("apps/ios/") for f in files))
        self.store.save_state(fid, st)
        self.store.event(fid, "pushed", commit=commit["sha"], files=files)
        summary = self.summary_line(result)
        self.gh.comment(st["issue"], f"Pushed to `main`: {commit['html_url']} — `{st['commit_message']}`\n\n"
                        f"Files: {', '.join(f'`{f}`' for f in files) or '—'}\n\n"
                        f"Agent summary: {summary}")

    @staticmethod
    def summary_line(result: str) -> str:
        m = re.search(r"^SUMMARY:\s*(.+)$", result, re.M)
        return m.group(1).strip() if m else (result.splitlines()[0][:300] if result else "(no reply text)")

    def find_commit(self, fid: str, since_iso: str) -> dict | None:
        since = (parse_iso(since_iso) - dt.timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
        tag = f"[feedback {fid}]"
        for c in self.gh.commits_since(since):
            if tag in c["commit"]["message"]:
                return c
        return None

    def check_ship(self, fid: str, st: dict) -> None:
        sha = st["commit"]
        runs = self.gh.runs_for_sha(sha)
        by_name: dict[str, dict] = {}
        for r in runs:
            if r["name"] in ("iOS", "CI", "Release") and (r["name"] not in by_name or r["run_number"] > by_name[r["name"]]["run_number"]):
                by_name[r["name"]] = r
        ios, ci, rel = by_name.get("iOS"), by_name.get("CI"), by_name.get("Release")
        st["runs"] = {k: {"url": v["html_url"], "status": v["status"], "conclusion": v["conclusion"]} for k, v in by_name.items()}
        waited = minutes_between(st["pushed_at"], iso())

        # iOS: TestFlight
        if st.get("expects_ios") and ios and ios["status"] == "completed" and not st.get("ios_done"):
            if ios["conclusion"] == "success":
                number = next((m.group(1) for a in self.gh.run_artifacts(ios["id"])
                               for m in [re.match(r"xcodebuild-logs-(\d{12})", a["name"])] if m), None)
                st.update(ios_done=True, ios_run=ios["html_url"], ios_uploaded_at=ios["updated_at"], testflight_build=number)
                self.store.save_state(fid, st)
                self.store.event(fid, "testflight_uploaded", build=number, run=ios["html_url"])
                self.gh.comment(st["issue"], f"iOS build green: {ios['html_url']} — TestFlight build **{number or '?'}** uploaded "
                                f"({minutes_between(self.store.submission(fid)['created'], ios['updated_at'])} min after the screenshot). "
                                "Waiting for Apple to finish processing.")
            else:
                errors = self.gh.run_error_lines(ios["id"])
                if st.get("retries", 0) < 1:
                    st["retries"] = st.get("retries", 0) + 1
                    reason = self.rate_limit_reason()
                    if reason and "launches already today" in reason:
                        self.fail(fid, st, f"iOS build failed and the daily launch cap is reached: {ios['html_url']}")
                        return
                    followup = ("The iOS build on `main` failed after your push "
                                f"({ios['html_url']}). Fix the build with the smallest change, keep the same commit message "
                                f"shape (`fix(feedback): <summary> [feedback {fid}]`), pull --rebase, and push to main again. "
                                "Compiler output:\n\n```\n" + "\n".join(errors or ["(no error lines captured; open the run)"]) + "\n```\n\n"
                                "End with the SUMMARY:/PUSHED: lines again.")
                    self.store.record_launch()
                    try:
                        run = self.cursor.followup(st["agent_id"], followup)
                    except HttpError as e:
                        self.fail(fid, st, f"iOS build failed; follow-up could not start (HTTP {e.status})", extra="\n".join(errors))
                        return
                    st.update(status="running", run_id=run["id"], launched=iso(), ios_failed_run=ios["html_url"])
                    self.store.save_state(fid, st)
                    self.store.event(fid, "retry", run=ios["html_url"])
                    self.gh.comment(st["issue"], f"iOS build failed: {ios['html_url']}. Sent the errors back to the agent for one retry.\n\n"
                                    "```\n" + "\n".join(errors[:20]) + "\n```")
                    return
                self.fail(fid, st, f"iOS build failed twice: {ios['html_url']}", extra="\n".join(errors))
                return

        if st.get("ios_done") and st.get("testflight_build") and not st.get("testflight_ready_at"):
            b = self.asc.build_state(st["testflight_build"])
            if b and b.get("processingState") == "VALID":
                st["testflight_ready_at"] = iso()
                self.store.save_state(fid, st)
                self.store.event(fid, "testflight_ready", build=st["testflight_build"])
            elif b:
                st["testflight_state"] = b.get("processingState")

        # Mac: edge release
        if st.get("expects_ci") and rel and rel["status"] == "completed" and not st.get("release_done"):
            if rel["conclusion"] == "success":
                edge = next((r["tag_name"] for r in self.gh.releases()
                             if r["tag_name"].startswith("edge-") and r.get("target_commitish", "").startswith(sha[:7])), None)
                st.update(release_done=True, release_run=rel["html_url"], edge_release=edge, release_at=rel["updated_at"])
                self.store.event(fid, "edge_released", release=edge, run=rel["html_url"])
                self.gh.comment(st["issue"], f"Mac edge release: **{edge or '?'}** ({rel['html_url']}).")
            else:
                st.update(release_done=True, release_run=rel["html_url"], release_failed=True)
                self.store.event(fid, "release_failed", run=rel["html_url"])
                self.gh.comment(st["issue"], f"Release workflow failed: {rel['html_url']} (the iOS build is unaffected).")
        if st.get("expects_ci") and ci and ci["status"] == "completed" and ci["conclusion"] != "success" and not st.get("ci_failed_noted"):
            st["ci_failed_noted"] = True
            self.store.event(fid, "ci_failed", run=ci["html_url"])
            self.gh.comment(st["issue"], f"CI failed on this commit: {ci['html_url']}. No edge release will be built from it.")

        # done?
        ios_ok = (not st.get("expects_ios")) or bool(st.get("testflight_ready_at"))
        mac_ok = (not st.get("expects_ci")) or bool(st.get("release_done")) or bool(st.get("ci_failed_noted"))
        if ios_ok and mac_ok and (st.get("expects_ios") or st.get("expects_ci")):
            self.ship(fid, st)
            return
        if waited > self.cfg.ship_timeout_min:
            if st.get("ios_done") or st.get("release_done"):
                self.ship(fid, st, partial=True)
            else:
                self.fail(fid, st, f"nothing shipped within {self.cfg.ship_timeout_min} min of the push",
                          extra=json.dumps(st.get("runs", {}), indent=1))
            return
        self.store.save_state(fid, st)

    def ship(self, fid: str, st: dict, partial: bool = False) -> None:
        info = self.store.submission(fid)
        parts = []
        if st.get("testflight_build"):
            parts.append(f"TestFlight build **{st['testflight_build']}**"
                         + (" (processed, installable)" if st.get("testflight_ready_at") else " (uploaded; Apple still processing)"))
        if st.get("edge_release"):
            parts.append(f"Mac edge release **{st['edge_release']}**")
        end = st.get("testflight_ready_at") or st.get("ios_uploaded_at") or st.get("release_at") or iso()
        elapsed = minutes_between(info["created"], end)
        st.update(status="shipped", shipped_at=iso(), elapsed_min=elapsed, partial=partial)
        self.store.save_state(fid, st)
        self.store.event(fid, "shipped", testflight_build=st.get("testflight_build"), edge=st.get("edge_release"),
                         elapsed_min=elapsed, commit=st.get("commit"), issue=st.get("issue_url"))
        self.gh.comment(st["issue"], ("Shipped" if not partial else "Shipped (partially)") + ": " + (", ".join(parts) or "see runs above")
                        + f".\n\nScreenshot → installable build: **{elapsed} min** "
                        f"(submitted {info['created']}, ready {end}).\n\nCommit {st.get('commit_url')} · agent {st.get('agent_url')}")
        self.gh.set_labels(st["issue"], ["shipped"], [])

    def fail(self, fid: str, st: dict, error: str, extra: str = "") -> None:
        st.update(status="failed", error=error, failed_at=iso())
        self.store.save_state(fid, st)
        self.store.event(fid, "failed", error=error)
        if st.get("issue"):
            body = f"Not fixed automatically: {error}."
            if extra:
                body += "\n\n<details><summary>Details</summary>\n\n```\n" + extra[:6000] + "\n```\n</details>"
            if st.get("agent_url"):
                body += f"\n\nAgent: {st['agent_url']}"
            self.gh.comment(st["issue"], body + "\n\nThis item needs a human. Label `needs-human` set.")
            self.gh.set_labels(st["issue"], ["needs-human"], [])

    # ---- entry points -------------------------------------------------------------------------

    def tick(self) -> int:
        if self.cfg.kill_file.exists():
            log(f"kill file {self.cfg.kill_file} present; doing nothing")
            return 0
        self.intake()
        self.advance_all()
        return 0

    def status(self) -> None:
        cur = self.store.cursor()
        print(f"cursor since={cur.get('since')} checked={cur.get('checked')} seen={len(cur.get('seen', []))}")
        print(f"launches today={self.store.launches_today()}/{self.cfg.max_per_day} last={self.store.last_launch()}")
        print(f"kill file: {'PRESENT' if self.cfg.kill_file.exists() else 'absent'} ({self.cfg.kill_file})")
        for fid in self.store.items():
            st = self.store.state(fid)
            bits = [st.get("status", "?"), st.get("target", "")]
            for k in ("issue_url", "agent_url", "commit", "testflight_build", "edge_release", "elapsed_min", "error", "deferred_reason"):
                if st.get(k):
                    bits.append(f"{k}={st[k]}")
            print(f"{fid}\t{st.get('created', '')}\t" + "  ".join(str(b) for b in bits))


def inject(cfg: Config, image: Path, comment: str, target: str | None) -> None:
    inbox = cfg.state_dir / "inbox"
    inbox.mkdir(parents=True, exist_ok=True)
    name = f"manual-{utcnow().strftime('%Y%m%d%H%M%S')}"
    d = inbox / name
    d.mkdir()
    shutil.copy2(image, d / image.name)
    (d / "comment.txt").write_text(comment + "\n")
    if target:
        (d / "target.txt").write_text(target + "\n")
    print(f"placed {d}; the next tick picks it up")


def main(argv: list[str]) -> int:
    cfg = Config.from_env()
    cmd = argv[1] if len(argv) > 1 else "run"
    if cmd == "inject":
        if len(argv) < 4:
            print(__doc__)
            return 2
        inject(cfg, Path(argv[2]), argv[3], argv[4] if len(argv) > 4 else None)
        return 0
    if cmd == "set-cursor":
        store = Store(cfg)
        cur = store.cursor()
        cur["since"] = iso(parse_iso(argv[2]))
        store.save_cursor(cur)
        print(f"cursor since={cur['since']}")
        return 0
    missing = [k for k in ("ASC_KEY_ID", "ASC_ISSUER_ID", "ASC_KEY_P8_B64", "CURSOR_API_KEY", "GITHUB_TOKEN") if not os.environ.get(k)]
    if missing and cmd != "status":
        print(f"missing env: {', '.join(missing)}", file=sys.stderr)
        return 2
    loop = Loop(cfg)
    if cmd == "status":
        loop.status()
        return 0
    if cmd != "run":
        print(__doc__)
        return 2
    cfg.state_dir.mkdir(parents=True, exist_ok=True)
    with (cfg.state_dir / ".lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            log("another tick is still running; skipping")
            return 0
        return loop.tick()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
