#!/usr/bin/env python3
"""T55: standby catch-up after ~100k changes. A job of N chunks of 1000 numbers (chunk min is 1000) makes ~100k replicated part entries.
  CK_BIN=... STRESS_DIR=... CK_PORT=9680 python3 -I scripts/stress/repl_100k.py stop|fresh [changes=100000]
stop : standby is SIGSTOPped for the whole job, then SIGCONTed (replay from the ring, or snapshot if the ring overflowed).
fresh: a standby that never ran joins after the job (snapshot). Prints seconds until acked_seq == last_seq, peak RSS of both heads."""
from __future__ import annotations

import os
import signal
import subprocess
import sys
import threading
import time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import DIR, agent, bootstrap, cleanup, http, join_code, rss_fd_thr, say, spawn, wait_up  # noqa: E402
import common
mode = sys.argv[1] if len(sys.argv) > 1 else "stop"; N = int(sys.argv[2]) if len(sys.argv) > 2 else 100000
SB = common.BASE + 5
peak = {"head": 0, "sb": 0}; stop = False
def sample(p: subprocess.Popen, k: str) -> None:
    while not stop:
        peak[k] = max(peak[k], rss_fd_thr(p.pid)[0]); time.sleep(0.2)
try:
    head = spawn(["head", "--no-local-worker"], "head"); wait_up(); tok = bootstrap(); code = join_code(tok)
    mk = lambda: spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby",
          {"CK_PORT": str(SB), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}, data="standby")
    sb = mk() if mode == "stop" else None
    for i in range(3): agent(i, code, threads=2, extra_args=["--virtual", "0"] if False else None)
    time.sleep(8)
    if sb: sb.send_signal(signal.SIGSTOP)
    threading.Thread(target=sample, args=(head, "head"), daemon=True).start()
    t0 = time.time()
    jid = http("POST", "/api/v1/jobs", {"name": "r100k", "kind": "synth", "synth_ns": 10, "n": N * 1000, "chunk": 1000, "policy": "fixed"}, tok)[1]["job"]["id"]
    while True:
        jb = http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]
        if jb["state"] in ("done", "failed"): break
        time.sleep(2)
    ha = http("GET", "/api/v1/ha", token=tok)[1]
    say(f"job {jb['state']} {jb['done']//1000}/{jb['total']//1000} chunks in {time.time()-t0:.0f}s; primary last_seq {ha['last_seq']} head RSS {rss_fd_thr(head.pid)[0]//1024} MiB")
    sb = sb or mk()
    threading.Thread(target=sample, args=(sb, "sb"), daemon=True).start()
    t1 = time.time()
    if mode == "stop": sb.send_signal(signal.SIGCONT)
    while time.time() - t1 < 600:
        ha = http("GET", "/api/v1/ha", token=tok)[1]
        mine = [s for s in ha["standbys"] if s["node_id"] == "8f67810af1a31b28"]  # node id of CK_NODE_ID=stress-standby (agents start their own standbys too)
        if mine and mine[0]["acked_seq"] >= ha["last_seq"]:
            say(f"standby caught up in {time.time()-t1:.1f}s (last_seq {ha['last_seq']}); peak RSS head {peak['head']//1024} MiB, standby {peak['sb']//1024} MiB")
            break
        time.sleep(0.5)
    else: say("FAIL standby did not catch up in 600s", ha)
    say("db size MiB", os.path.getsize(os.path.join(DIR, "data", "head.db")) // 2**20)
finally:
    stop = True; cleanup()
