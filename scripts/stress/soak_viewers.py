#!/usr/bin/env python3
"""T55 soak: secured head + 3 agents + a long synth job + 25 SSE viewers; samples head RSS/fds/threads and tasks/s.
  CK_BIN=... STRESS_DIR=... CK_PORT=9660 python3 -I scripts/stress/soak_viewers.py [seconds=1200] [viewers=25]
Prints a CSV line every 20 s and a first-vs-last drift summary. Finding if RSS/fds/threads grow without bound."""
from __future__ import annotations

import sys, threading, time, urllib.request
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from common import URL, agent, bootstrap, cleanup, http, join_code, rss_fd_thr, say, spawn, wait_up  # noqa: E402

secs = int(sys.argv[1]) if len(sys.argv) > 1 else 1200
nv = int(sys.argv[2]) if len(sys.argv) > 2 else 25
head = spawn(["head", "--no-local-worker"], "head")
try:
    wait_up(); tok = bootstrap(); code = join_code(tok)
    for i in range(3): agent(i, code, threads=2)
    time.sleep(6)
    st, j, _ = http("POST", "/api/v1/jobs", {"name": "soak", "kind": "synth", "synth_ns": 200000, "n": 10**9, "chunk": 2000}, tok)
    jid = j["job"]["id"]; say("job", jid, "nodes", len(http("GET", "/api/v1/nodes", token=tok)[1]["nodes"]))
    stop = False; got = [0] * nv; errs = [0] * nv

    def viewer(k: int) -> None:
        while not stop:
            try:
                rq = urllib.request.Request(URL + "/api/v1/stream", headers={"Authorization": "Bearer " + tok})
                with urllib.request.urlopen(rq, timeout=30) as r:
                    while not stop:
                        b = r.read1(65536)
                        if not b: break
                        got[k] += len(b)
            except Exception: errs[k] += 1; time.sleep(1)
    ths = [threading.Thread(target=viewer, args=(k,), daemon=True) for k in range(nv)]
    [t.start() for t in ths]
    rows = []; t0 = time.time(); prev = None
    say("t_s,rss_kib,fds,threads,tasks_done,tasks_per_s,viewer_MB,viewer_errs,api_ms")
    while time.time() - t0 < secs:
        time.sleep(20)
        a = time.time(); jj = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]; api_ms = (time.time() - a) * 1000
        jb = jj.get("job", jj); done = jb.get("tasks_done", jb.get("done", 0)) or 0
        r, f, t = rss_fd_thr(head.pid); now = time.time() - t0
        rate = 0 if prev is None else (done - prev[1]) / (now - prev[0]); prev = (now, done)
        rows.append((now, r, f, t, rate))
        say(f"{now:.0f},{r},{f},{t},{done},{rate:.1f},{sum(got)/1e6:.1f},{sum(errs)},{api_ms:.0f}")
    stop = True
    a, b = rows[1], rows[-1]
    say(f"DRIFT rss {a[1]}->{b[1]} KiB ({(b[1]-a[1])/1024:+.1f} MiB)  fds {a[2]}->{b[2]}  threads {a[3]}->{b[3]}  tasks/s {a[4]:.1f}->{b[4]:.1f}")
finally:
    cleanup()
