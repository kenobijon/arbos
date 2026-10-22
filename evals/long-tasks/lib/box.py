"""Run scripts on the test agent's sandbox. Two backends: a D89 host sandbox (ssh to the relay
host, `docker exec` into `arbos-box-<user>`) and a Daytona box (toolbox API). The eval only ever
touches the dedicated test user's sandbox."""

import base64
import json
import os
import shlex
import subprocess

from . import relay as rl

SSH_HOST = os.environ.get("LT_HOST_SSH", "const@204.12.171.6")
SSH_KEY = os.environ.get("LT_HOST_SSH_KEY", os.path.expanduser("~/.ssh/arbos_cloud"))
DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY", "")


class Box:
    def __init__(self):
        m = rl.machine()
        self.provider = m.get("provider") or "gvisor"
        self.sandbox_id = m.get("sandbox_id")
        self.image = ((m.get("image") or {}).get("current")) or ((m.get("image") or {}).get("default"))
        self.user = rl.user_id()
        self.container = f"arbos-box-{self.user.lower()}"

    # ---- raw exec ----------------------------------------------------------------------------

    def _exec_root(self, script, timeout):
        b64 = base64.b64encode(script.encode()).decode()
        if self.provider == "daytona":
            st, v = rl.http(
                "POST",
                f"https://proxy.app.daytona.io/toolbox/{self.sandbox_id}/process/execute",
                token=DAYTONA_KEY,
                body={"command": f"echo {b64} | base64 -d | sudo -n bash", "timeout": timeout},
                timeout=timeout + 30,
            )
            if st != 200:
                return 255, f"toolbox exec {st}: {v}"
            return v.get("exitCode"), v.get("result", "")
        # the script travels on stdin (base64, one line) so its size never hits the argv limit
        cmd = f"docker exec -i {shlex.quote(self.container)} bash -c 'read -r B; echo \"$B\" | base64 -d | bash'"
        p = subprocess.run(
            ["ssh", "-i", SSH_KEY, "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=15", SSH_HOST, cmd],
            input=b64 + "\n",
            capture_output=True,
            text=True,
            timeout=timeout + 60,
        )
        return p.returncode, p.stdout + (("\n" + p.stderr) if p.stderr.strip() else "")

    def root(self, script, timeout=120):
        try:
            return self._exec_root(script, timeout)
        except subprocess.TimeoutExpired:
            return 124, "box exec timed out"

    def ws_user(self, project):
        code, out = self.root(f"stat -c %U /work/{project}")
        return out.strip().splitlines()[0] if out.strip() else "root"

    def run_as_ws(self, project, script, env=None, timeout=120):
        """Run a bash script as the workspace user in the project directory."""
        pre = f"cd /work/{project} && "
        for k, v in (env or {}).items():
            pre += f"export {k}={shlex.quote(str(v))}; "
        user = self.ws_user(project)
        b64 = base64.b64encode((pre + script).encode()).decode()
        return self.root(f"echo {b64} | base64 -d | sudo -n -u {shlex.quote(user)} bash", timeout)

    def put_files(self, project, base_dir, files, timeout=180):
        """Write {relative path: bytes} under /work/<project>/<base_dir> as the workspace user."""
        parts = []
        for rel, data in files.items():
            b64 = base64.b64encode(data).decode()
            parts.append(f"mkdir -p \"$(dirname {shlex.quote(rel)})\" && echo {b64} | base64 -d > {shlex.quote(rel)}")
        script = f"mkdir -p {shlex.quote(base_dir)} && cd {shlex.quote(base_dir)} && " + " && ".join(parts) + " && echo PUT_OK"
        code, out = self.run_as_ws(project, script, timeout=timeout)
        return "PUT_OK" in out, out[-300:]

    def read_file(self, project, path, limit=6000):
        code, out = self.root(f"head -c {limit} /work/{project}/{shlex.quote(path)} 2>/dev/null")
        return out

    # ---- evidence ----------------------------------------------------------------------------

    def session_entries(self, project, chat_id):
        code, out = self.root(f"cat /work/{project}/.arbos/pi/sessions/*_{chat_id}.jsonl 2>/dev/null")
        entries = []
        for line in out.splitlines():
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError:
                pass
        return entries

    def audit_since(self, project, ts_ms):
        code, out = self.root(f"cat /work/{project}/.arbos/audit.jsonl 2>/dev/null")
        rows = []
        for line in out.splitlines():
            try:
                v = json.loads(line)
            except json.JSONDecodeError:
                continue
            if v.get("ts", 0) >= ts_ms:
                rows.append(v)
        return rows

    def agentd_sha(self):
        code, out = self.root("cat /opt/arbos/agentd.sha 2>/dev/null || arbos-agentd --version 2>/dev/null || cat /etc/arbos-image 2>/dev/null; true")
        return out.strip()[:80]
