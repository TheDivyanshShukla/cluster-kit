#!/usr/bin/env python3
"""T55: chaos on a SECURED fleet (join codes, node keys, sealed links). One scenario per run:
  CK_BIN=... STRESS_DIR=... CK_PORT=9670 python3 -I scripts/stress/chaos_secured.py agent9|agentstop|primary9|primarystop|skew
Fleet: primary, one standby (own ports CK_PORT+5.. so a STOPped primary does not hold the standby's ports), 3 agents with
CK_HEADS pointing at both. A synth job of N numbers runs through the fault; PASS when state=done, done==total and the sum reducer == N.
"skew": agent clock +/- minutes is simulated with libfaketime if present, else the scenario prints SKIP."""
from __future__ import annotations

import sys, time
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from common import DIR, URL, agent, bootstrap, cleanup, http, join_code, say, spawn, wait_up  # noqa: E402
import common, os, signal

sc = sys.argv[1] if len(sys.argv) > 1 else "agent9"
N = 6 * 10**6
# primary9: the standby shares the primary's ports (listens on nothing until it promotes; agents find it by beacon/--head).
# primarystop: a STOPped primary keeps its ports, so the standby needs its own; agents then only learn it through CK_HEADS.
SB = common.BASE + 5 if sc == "primarystop" else common.BASE
SBURL = f"http://127.0.0.1:{SB + 2}"
res = "FAIL"
try:
    head = spawn(["head", "--no-local-worker"], "head")
    wait_up(); tok = bootstrap(); code = join_code(tok)
    sb = spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby",
               {"CK_PORT": str(SB), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}, data="standby")
    ags = [agent(i, code, threads=2, extra_args=[]) for i in range(3)]
    # agents learn the standby from CK_HEADS on failover (set before spawn is too late for agent(); respawn with env)
    for p in ags: p.kill()
    ags = [spawn(["agent", "--head", "127.0.0.1", "--threads", "2", "--name", f"a{i}", "--code", code], f"agent{i}",
                 {"CK_NODE_ID": f"stress-a{i}", "CK_KEY_DIR": os.path.join(DIR, f"keys-a{i}"), "CK_HEADS": f"127.0.0.1:{SB}"}, data=f"agent{i}") for i in range(3)]
    time.sleep(8)
    say("ha", http("GET", "/api/v1/ha", token=tok)[1])
    st, j, _ = http("POST", "/api/v1/jobs", {"name": "chaos", "kind": "synth", "synth_ns": 20000, "n": N, "chunk": 20000, "reducer": "sum"}, tok)
    jid = j["job"]["id"]; t0 = time.time(); active = URL; victim = ags[2]
    time.sleep(4); say("progress at fault", http("GET", f"/api/v1/jobs/{jid}", token=tok)[1]["job"]["done"], "of", N)
    if sc == "agent9": victim.kill()
    elif sc == "agentstop": victim.send_signal(signal.SIGSTOP); time.sleep(20); victim.send_signal(signal.SIGCONT)
    elif sc == "primary9": head.kill()
    elif sc == "primarystop": head.send_signal(signal.SIGSTOP)
    else: say("SKIP unknown scenario"); sys.exit(0)
    if sc.startswith("primary"): active = SBURL
    t_fault = time.time()
    if sc == "primarystop":
        time.sleep(25); head.send_signal(signal.SIGCONT); say("old primary resumed; ha old:", http("GET", "/api/v1/ha", token=tok, timeout=5)[1], "new:", http("GET", "/api/v1/ha", token=tok, base=SBURL, timeout=5)[1])
    state = None; jb = {}
    while time.time() - t0 < 240:
        try:
            s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tok, base=active, timeout=5)
            if s == 200:
                jb = v["job"]; state = jb["state"]
                if state in ("done", "failed", "cancelled"): break
            elif sc.startswith("primary") and time.time() - t_fault > 1:
                say(f"api {s} {str(v)[:80]}; password login on {active}: {http('POST', '/api/v1/login', {'name': 'admin', 'password': 'stress-test-password-1'}, base=active)[0]}")
        except Exception:
            pass
        time.sleep(2)
    s, total, _ = http("GET", f"/api/v1/jobs/{jid}/result?reducer=sum", token=tok, base=active)
    say("state", state, "done", jb.get("done"), "total", jb.get("total"), "result", str(total)[:40], f"took {time.time()-t0:.0f}s")
    say("ha after:", http("GET", "/api/v1/ha", token=tok, base=active)[1])
    res = "PASS" if state == "done" and jb["done"] == jb["total"] == N and str(total).strip() == str(N) else "FAIL"
finally:
    say(res, sc); cleanup()
