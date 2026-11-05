#!/usr/bin/env python3
"""T55 limits on a secured head: list10k | stdout | alerts.
  CK_BIN=... STRESS_DIR=... CK_PORT=9680 python3 -I scripts/stress/limits.py list10k|stdout|alerts
list10k: 10,000-item list job, `cat` per item, lines reducer: result must hold every item once.
stdout : cmd job printing ~1 GB: head/agent RSS must stay bounded and the job end with a clear error (output cap).
alerts : 1,000 virtual PCs vanish (kill -9 agent): how many alerts open, does GET /alerts show all, do they close on return."""
from __future__ import annotations

import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from common import DIR, URL, agent, bootstrap, cleanup, http, join_code, rss_fd_thr, say, spawn, wait_up  # noqa: E402

mode = sys.argv[1] if len(sys.argv) > 1 else "list10k"


def wait_job(jid: int, tok: str, secs: int = 300) -> dict:
    t = time.time()
    jb: dict = {}
    while time.time() - t < secs:
        jb = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]
        if jb["state"] in ("done", "failed", "cancelled"):
            break
        time.sleep(1)
    return jb


try:
    head = spawn(["head", "--no-local-worker"], "head")
    wait_up()
    tok = bootstrap()
    code = join_code(tok, 500)
    if mode == "alerts":
        ag = agent(0, code, 1, ["--virtual", "1000"])
        time.sleep(25)
        n = len(http("GET", "/api/v1/nodes", token=tok)[1]["nodes"])
        say("virtual nodes joined:", n)
        t0 = time.time()
        ag.kill()
        time.sleep(40)
        a = http("GET", "/api/v1/alerts?active=1", token=tok)[1]["alerts"]
        say(f"after kill: active alerts listed {len(a)} (nodes {n}); head RSS {rss_fd_thr(head.pid)[0] // 1024} MiB; {time.time() - t0:.0f}s")
        t1 = time.time()
        s, _, _ = http("GET", "/api/v1/alerts", token=tok)
        say(f"GET /alerts status {s} in {time.time() - t1:.2f}s")
        agent(0, code, 1, ["--virtual", "1000"])
        time.sleep(40)
        a2 = http("GET", "/api/v1/alerts?active=1", token=tok)[1]["alerts"]
        say(f"after return: active alerts {len(a2)}")
    else:
        for i in range(2):
            agent(i, code, 2)
        time.sleep(8)
        if mode == "list10k":
            items = [f"item-{i}" for i in range(10000)]
            jid = http("POST", "/api/v1/jobs", {"name": "l10k", "kind": "list", "cmd": "cat", "items": items, "reducer": "lines"}, tok)[1]["job"]["id"]
            t0 = time.time()
            jb = wait_job(jid, tok)
            out = http("GET", f"/api/v1/jobs/{jid}/result", token=tok)[1]
            lines = str(out).split("\n")
            ok = jb["state"] == "done" and sorted(x for x in lines if x) == sorted(items)
            say(f"list10k {jb['state']} in {time.time() - t0:.0f}s, result lines {len(lines)}, exact={ok}; head RSS {rss_fd_thr(head.pid)[0] // 1024} MiB")
        else:
            jid = http("POST", "/api/v1/jobs", {"name": "big", "kind": "cmd", "cmd": "head -c 1073741824 /dev/zero", "n": 1}, tok)[1]["job"]["id"]
            peak = 0
            t0 = time.time()
            jb: dict = {}
            while time.time() - t0 < 120:
                peak = max(peak, rss_fd_thr(head.pid)[0])
                jb = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]
                if jb["state"] in ("done", "failed", "cancelled"):
                    break
                time.sleep(0.5)
            say(f"stdout 1GB: {jb.get('state')} in {time.time() - t0:.0f}s error={jb.get('error')!r} head peak RSS {peak // 1024} MiB")
finally:
    cleanup()
    say("dir", DIR, URL, common.BASE)
