"""Real-task stress through the Python SDK: runs examples/real/*.py (local once, then on a secured virtual-agent fleet),
optionally with faults. stdlib only; python3 -I safe.
Env: CK_BIN (ck binary), STRESS_DIR (scratch), CK_PORT base (9860..9899), WL_PY (python with numpy/pandas/torch/..., also put first on the
PCs' PATH), WL_DATA (dir for generated data, default STRESS_DIR/data).
  workloads.py up                              start head + standby + agents, print env, wait (Ctrl-C / kill to stop)
  workloads.py run [--fault none|agent9|agentstop|head9|cancel|multi] [wl ...]   whole cycle: fleet, local ref, cluster, check
Workloads: train sweep etl cv text.  Each examples/real/<wl>.py takes `local|cluster` and prints one JSON line {"digest":..., "secs":...}.
"""
from __future__ import annotations
import json
import os
import signal
import subprocess
import sys
import threading
import time
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from common import DIR, bootstrap, http, join_code, say, spawn, wait_up  # noqa: E402

ROOT = os.environ.get("WL_ROOT") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
PY = os.environ.get("WL_PY", sys.executable)
DATA = os.environ.get("WL_DATA") or os.path.join(DIR, "data-files")
WLS = ["train", "sweep", "etl", "cv", "text"]
PROCS_N, VIRT, THREADS = 4, 2, 1  # 4 processes x 2 virtual agents x 1 thread = 8 PCs


def start_fleet() -> dict[str, Any]:
    """Secured head, a standby on the same ports (takes over when the head dies), PROCS_N agent processes."""
    pe = {"PATH": os.path.dirname(PY) + os.pathsep + os.environ["PATH"]}
    if os.environ.get("WL_DEBUG"):  # per-chunk dispatch/result lines in each process's CK_DATA/logs
        pe.update(CK_TUNE_LOG_LEVEL="4", CK_TUNE_LOG_CHUNKS="1")
    head = spawn(["head", "--no-local-worker"], "head", pe)
    wait_up()
    tok = bootstrap()
    code = join_code(tok, 500)
    sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby",
               dict(pe, CK_PRIMARY_PORT=str(common.BASE), CK_NODE_ID="wl-standby", CK_KEY_DIR=os.path.join(DIR, "keys-standby")), data="standby")
    def mk_agent(i: int) -> subprocess.Popen:
        return spawn(["agent", "--head", "127.0.0.1", "--threads", str(THREADS), "--virtual", str(VIRT), "--name", f"a{i}", "--code", code], f"agent{i}",
                     dict(pe, CK_NODE_ID=f"wl-a{i}", CK_KEY_DIR=os.path.join(DIR, f"keys-a{i}"), CK_HEADS=f"127.0.0.1:{common.BASE}"), data=f"agent{i}")

    ags = [mk_agent(i) for i in range(PROCS_N)]
    t = time.time()
    while time.time() - t < 40:
        n = len([c for c in http("GET", "/api/v1/cluster", token=tok)[1].get("workers", []) if c.get("online", True)])
        if n >= PROCS_N * VIRT:
            break
        time.sleep(1)
    say(f"fleet up: {n} nodes")
    return {"head": head, "standby": sb, "agents": ags, "token": tok, "code": code, "mk_agent": mk_agent}


def wl_env(tok: str, extra: dict | None = None) -> dict:
    e = dict(os.environ, CK_HEAD=common.URL, CK_TOKEN=tok, WL_DATA=DATA, CK_SDK_HOME=os.path.join(DIR, "sdkhome"), PYTHONHASHSEED="0")
    e.update(extra or {})
    return e


def run_wl(wl: str, mode: str, tok: str, extra: dict | None = None, timeout: float = 1500) -> subprocess.Popen:
    log = open(os.path.join(DIR, f"{wl}.{mode}.log"), "w")
    return subprocess.Popen([PY, os.path.join(ROOT, "examples", "real", f"{wl}.py"), mode], env=wl_env(tok, extra), stdout=log, stderr=subprocess.STDOUT, text=True)


def result_of(wl: str, mode: str) -> dict:
    try:
        for ln in reversed(open(os.path.join(DIR, f"{wl}.{mode}.log")).read().splitlines()):
            if ln.startswith("{"):
                return json.loads(ln)
    except Exception:
        pass
    return {}


def rss_thr(pid: int) -> tuple[int, int]:
    r = common.rss_fd_thr(pid)
    return r[0], r[2]


class Mon(threading.Thread):
    """Samples the head's RSS (MiB) and thread count while a workload runs."""

    def __init__(self, pid: int) -> None:
        super().__init__(daemon=True)
        self.pid, self.stop, self.rss, self.thr = pid, False, 0, 0

    def run(self) -> None:
        while not self.stop:
            r, t = rss_thr(self.pid)
            self.rss, self.thr = max(self.rss, r // 1024), max(self.thr, t)
            time.sleep(1)


def fault_thread(kind: str, fl: dict, wl: str, tok: str, delay: float) -> threading.Thread:
    def go() -> None:
        time.sleep(delay)
        ags = fl["agents"]
        say(f"  FAULT {kind} after {delay}s")
        if kind == "agent9":
            os.killpg(ags[1].pid, signal.SIGKILL)  # two virtual PCs vanish at once
            time.sleep(8)
            ags[1] = fl["mk_agent"](1)  # the operator restarts it: it rejoins with its saved key
        elif kind == "agentstop":
            os.killpg(ags[1].pid, signal.SIGSTOP); time.sleep(20); os.killpg(ags[1].pid, signal.SIGCONT)
        elif kind == "head9":
            os.killpg(fl["head"].pid, signal.SIGKILL)
    t = threading.Thread(target=go, daemon=True)
    t.start()
    return t


def cycle(fault: str, wls: list[str]) -> None:
    rows = []
    # local reference first (no fleet load): answer + time
    base = {}
    ref_env = {"CK_HEAD": "http://127.0.0.1:1", "CK_TOKEN": "x"}
    for wl in wls:
        if not os.environ.get("WL_REUSE_LOCAL"):  # WL_REUSE_LOCAL=1: keep the reference from an earlier run (same machine, same data)
            run_wl(wl, "local", "x", ref_env).wait()
        base[wl] = result_of(wl, "local")
        say(f"{wl} local: {base[wl]}")
    fl = start_fleet()
    mon = Mon(fl["head"].pid); mon.start()
    tok = fl["token"]
    if fault == "multi":  # 5 users at once, one workload each (several rounds if fewer than 5 are asked for)
        users = []
        for i, wl in enumerate((wls * 5)[:5]):
            u = f"user{i + 1}"
            http("POST", "/api/v1/users", {"name": u, "password": "stress-user-password-1", "role": "operator"}, fl["token"])
            sc = http("POST", "/api/v1/login", {"name": u, "password": "stress-user-password-1"})[2]
            users.append((u, wl, http("POST", "/api/v1/tokens", {}, cookie=sc.split(";")[0])[1]["token"]))
        t0 = time.time()
        ps = [(u, wl, time.time(), run_wl(wl, f"multi-{u}", t, {"CK_SDK_HOME": os.path.join(DIR, "sdk-" + u)})) for u, wl, t in users]
        for u, wl, ts, p in ps:
            p.wait()
            r = result_of(wl, f"multi-{u}")
            ok = bool(r) and r.get("digest") == base[wl].get("digest") and r.get("metric", 1) >= base[wl].get("metric", 1) - base[wl].get("tol", 0)
            rows.append((wl, f"multi:{u}", base[wl].get("secs"), r.get("secs"), ok, mon.rss, mon.thr, time.time() - ts))
            say(f"{u} {wl}: {r} match={ok}")
        say(f"all 5 finished in {time.time() - t0:.0f}s wall (sum of local times {sum(base[w]['secs'] for _, w, _ in users):.0f}s)")
        wls = []
    for wl in wls:
        mon.rss = mon.thr = 0
        t0 = time.time()
        p = run_wl(wl, "cluster", tok)
        delay = float(os.environ.get("WL_FAULT_AT", "0")) or max(6.0, 0.25 * base[wl].get("secs", 20))
        if fault in ("agent9", "agentstop", "head9"):
            fault_thread(fault, fl, wl, tok, delay)
        if fault == "cancel":
            time.sleep(delay)
            for j in http("GET", "/api/v1/jobs", token=tok)[1].get("jobs", []):
                if j.get("state") in ("running", "queued", "pending"):
                    http("POST", f"/api/v1/jobs/{j['id']}/cancel", {}, tok)
            p.wait()
            say(f"  {wl}: first attempt cancelled, exit code {p.returncode}; resubmitting")
            p = run_wl(wl, "cluster", tok)
        p.wait()
        r = result_of(wl, "cluster")
        ok = bool(r) and r.get("digest") == base[wl].get("digest") and r.get("metric", 1) >= base[wl].get("metric", 1) - base[wl].get("tol", 0)
        rows.append((wl, fault, base[wl].get("secs"), r.get("secs"), ok, mon.rss, mon.thr, time.time() - t0))
        say(f"{wl} cluster[{fault}]: {r} match={ok} headRSS={mon.rss}MiB thr={mon.thr} wall={time.time() - t0:.0f}s")
        if fault == "head9":
            break  # the primary is gone: the standby is the head now
    say("\ncase | local s | cluster s | speedup | match | head RSS MiB | head threads | wall s")
    for wl, f, ls, cs, ok, rs, th, wall in rows:
        say(f"{wl}/{f} | {ls} | {cs} | {round(ls / cs, 2) if ls and cs else '-'} | {'PASS' if ok else 'FAIL'} | {rs} | {th} | {wall:.0f}")
    common.cleanup()


if __name__ == "__main__":
    a = sys.argv[1:]
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))  # still runs the cleanup below
    try:
        if a and a[0] == "up":
            fl = start_fleet()
            print(f"export CK_HEAD={common.URL} CK_TOKEN={fl['token']}", flush=True)
            while True:
                time.sleep(60)
        elif a and a[0] == "run":
            f = "none"
            if "--fault" in a:
                f = a[a.index("--fault") + 1]
            cycle(f, [x for x in a[1:] if x in WLS] or WLS)
        else:
            print(__doc__)
    finally:
        common.cleanup()
