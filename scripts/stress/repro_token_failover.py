#!/usr/bin/env python3
"""T55 repro: does an API token / login made on the primary work on the standby after kill -9 of the primary (secured fleet)?
  CK_BIN=... STRESS_DIR=... CK_PORT=9690 python3 -I scripts/stress/repro_token_failover.py [wait_s_before_kill=15]"""
from __future__ import annotations

import sys, time, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import DIR, bootstrap, cleanup, http, join_code, say, spawn, wait_up  # noqa: E402
import common
w = float(sys.argv[1]) if len(sys.argv) > 1 else 15
SB = common.BASE + 5; SBURL = f"http://127.0.0.1:{SB+2}"
try:
    head = spawn(["head", "--no-local-worker"], "head"); wait_up(); tok = bootstrap(); code = join_code(tok)
    spawn(["head", "--standby", "--head", "127.0.0.1", "--code", code, "--no-local-worker"], "standby",
          {"CK_PORT": str(SB), "CK_PRIMARY_PORT": str(common.BASE), "CK_NODE_ID": "stress-standby", "CK_KEY_DIR": os.path.join(DIR, "keys-standby")}, data="standby")
    time.sleep(w); say("ha", http("GET", "/api/v1/ha", token=tok)[1]["standbys"])
    head.kill(); time.sleep(12)
    say("token on promoted standby:", http("GET", "/api/v1/nodes", token=tok, base=SBURL)[0])
    s, _, sc = http("POST", "/api/v1/login", {"name": "admin", "password": "stress-test-password-1"}, base=SBURL)
    say("password login on promoted standby:", s)
finally:
    cleanup()
