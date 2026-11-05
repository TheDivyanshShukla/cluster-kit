"""Shared helpers for the scripts/stress/ round-2 tests. stdlib only; python3 -I safe.
Env: CK_BIN (ck binary, required), STRESS_DIR (scratch dir), CK_PORT base (default 9660). Never touches ./ck-data.
"""
from __future__ import annotations
import json
import os
import signal
import subprocess
import time
import urllib.error
import urllib.request
from typing import Any

BIN = os.environ["CK_BIN"]
BASE = int(os.environ.get("CK_PORT", "9660"))
DIR = os.environ.get("STRESS_DIR") or os.path.join(os.getcwd(), "stress-out")
os.makedirs(DIR, exist_ok=True)
URL = f"http://127.0.0.1:{BASE + 2}"
PROCS: list[subprocess.Popen] = []


def env(extra: dict | None = None, data: str = "data", fleet: str = "secured") -> dict:
    e = dict(os.environ, CK_PORT=str(BASE), CK_DATA=os.path.join(DIR, data), CK_KEY_DIR=os.path.join(DIR, "keys-" + data),
             CK_FLEET=fleet, CK_PRESENCE="off", CK_POWER_DRY="1")
    e.update(extra or {})
    return e


def spawn(args: list[str], name: str, extra: dict | None = None, data: str = "data", fleet: str = "secured") -> subprocess.Popen:
    p = subprocess.Popen([BIN] + args, env=env(extra, data, fleet), stdout=open(os.path.join(DIR, name + ".log"), "w"), stderr=subprocess.STDOUT, start_new_session=True)
    PROCS.append(p)
    return p


def cleanup() -> None:
    for p in PROCS:
        try:
            os.killpg(p.pid, signal.SIGCONT); os.killpg(p.pid, signal.SIGKILL)  # agents spawn standby heads: kill the whole group
        except Exception:
            pass
    PROCS.clear()


def http(method: str, path: str, body: Any = None, token: str | None = None, cookie: str | None = None, base: str = URL, timeout: float = 15) -> tuple[int, Any, str | None]:
    req = urllib.request.Request(base + path, method=method, data=None if body is None else json.dumps(body).encode())
    req.add_header("Content-Type", "application/json")
    if token: req.add_header("Authorization", "Bearer " + token)
    if cookie: req.add_header("Cookie", cookie)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read(); sc = r.headers.get("Set-Cookie")
            return r.status, (json.loads(raw) if raw[:1] in (b"{", b"[") else raw.decode()), sc
    except urllib.error.HTTPError as e:
        raw = e.read()
        try: return e.code, json.loads(raw), None
        except Exception: return e.code, raw.decode(errors="replace"), None


def wait_up(base: str = URL, secs: float = 20) -> None:
    t = time.time()
    while time.time() - t < secs:
        try:
            if http("GET", "/api/v1/me", base=base, timeout=2)[0] < 500: return
        except Exception: pass
        time.sleep(0.3)
    raise RuntimeError("head did not come up")


def bootstrap(pw: str = "stress-test-password-1") -> str:
    """First admin on a fresh secured head; returns an API token."""
    _, _, sc = http("POST", "/api/v1/bootstrap", {"name": "admin", "password": pw})
    if not sc:
        _, _, sc = http("POST", "/api/v1/login", {"name": "admin", "password": pw})
    cookie = sc.split(";")[0]
    return http("POST", "/api/v1/tokens", {}, cookie=cookie)[1]["token"]


def join_code(tok: str, uses: int = 100) -> str:
    return http("POST", "/api/v1/join-codes", {"uses": uses, "ttl_min": 600, "auto_approve": True}, tok)[1]["code"]


def agent(i: int, code: str, threads: int = 1, extra_args: list[str] | None = None) -> subprocess.Popen:
    return spawn(["agent", "--head", "127.0.0.1", "--threads", str(threads), "--name", f"a{i}", "--code", code] + (extra_args or []),
                 f"agent{i}", {"CK_NODE_ID": f"stress-a{i}", "CK_KEY_DIR": os.path.join(DIR, f"keys-a{i}")}, data=f"agent{i}")


def rss_fd_thr(pid: int) -> tuple[int, int, int]:
    """(RSS KiB, open fds, threads) via ps/lsof (macOS) or /proc (Linux)."""
    try:
        if os.path.exists(f"/proc/{pid}/status"):
            st = open(f"/proc/{pid}/status").read()
            g = lambda k: int([l for l in st.splitlines() if l.startswith(k)][0].split()[1])
            return g("VmRSS:"), len(os.listdir(f"/proc/{pid}/fd")), g("Threads:")
        rss = int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).split()[0])
        fds = max(0, len(subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1)
        thr = len(subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1
        return rss, fds, thr
    except Exception:
        return 0, 0, 0


def say(*a: Any) -> None:
    print(*a, flush=True)
