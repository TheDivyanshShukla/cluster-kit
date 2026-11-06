#!/usr/bin/env python3
"""Round 3 (t4): job and process lifecycle bad cases on a secured fleet. One case per run:
  CK_BIN=... STRESS_DIR=... CK_PORT=9820 python3 -I scripts/stress/lifecycle.py CASE
Job faults : segv exit1 exit75 hang orphan mem bigout garbage nondet poison sdk
HA         : double switch sbsnap restart1 stop1 cancelfo clientkill dup2 flap many nopcs
Each case prints one line `RESULT <case> PASS|FAIL <detail>` (exact job results are checked)."""
from __future__ import annotations

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
import urllib.request
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from common import BIN, DIR, URL, agent, bootstrap, cleanup, http, join_code, rss_fd_thr, say, spawn, wait_up  # noqa: E402

CASE = sys.argv[1] if len(sys.argv) > 1 else "segv"
MARK = f"ckl{common.BASE}"  # unique marker for processes the cases start
SL = 30000 + (common.BASE % 1000) * 10  # unique sleep durations per port base so parallel runs do not reap each other
VERDICT: list[str] = []


def ok(cond: bool, what: str) -> None:
    VERDICT.append(("ok  " if cond else "BAD ") + what)
    say(VERDICT[-1])


def submit(tok: str, body: dict, base: str = URL) -> int:
    s, j, _ = http("POST", "/api/v1/jobs", body, tok, base=base)
    if s != 200:
        raise RuntimeError(f"submit {s} {j}")
    return j["job"]["id"]


def job(tok: str, jid: int, base: str = URL) -> dict:
    try:
        s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tok, base=base, timeout=5)
        return v["job"] if s == 200 else {}
    except Exception:
        return {}


def wait_job(tok: str, jid: int, secs: float = 120, base: str = URL) -> dict:
    t = time.time()
    jb: dict = {}
    while time.time() - t < secs:
        jb = job(tok, jid, base) or jb
        if jb.get("state") in ("done", "failed", "cancelled"):
            break
        time.sleep(0.5)
    return jb


def raw_result(tok: str, jid: int, base: str = URL, q: str = "") -> bytes:
    req = urllib.request.Request(f"{base}/api/v1/jobs/{jid}/result{q}", headers={"Authorization": "Bearer " + tok})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def procs(pat: str) -> list[str]:
    out = subprocess.run(["ps", "-axo", "pid,ppid,command"], capture_output=True, text=True).stdout.splitlines()
    return [ln for ln in out if pat in ln and "lifecycle.py" not in ln and "grep" not in ln]


def reap(pat: str) -> None:
    """Kill leftover helper processes this script started (matched by the unique marker)."""
    for ln in procs(pat):
        try:
            os.kill(int(ln.split()[0]), signal.SIGKILL)
        except Exception:
            pass


def cmd_job(tok: str, cmd: str, n: int = 4, chunk: int = 4, **kw: Any) -> int:
    return submit(tok, dict({"name": CASE, "kind": "cmd", "cmd": cmd, "from": 0, "n": n, "chunk": chunk, "policy": "fixed"}, **kw))


def basic() -> tuple[str, str]:
    spawn(["head", "--no-local-worker"], "head")
    wait_up()
    tok = bootstrap()
    return tok, join_code(tok)


def job_faults() -> None:
    tok, code = basic()
    agent(0, code, 2)
    agent(1, code, 2)
    time.sleep(6)
    if CASE == "segv":
        jb = wait_job(tok, cmd_job(tok, "sh -c 'kill -SEGV $$'"), 60)
        ok(jb["state"] == "failed" and "signal" in str(jb["error"]), f"segv -> {jb['state']} {jb['error']}")
    elif CASE == "exit1":
        jb = wait_job(tok, cmd_job(tok, "sh -c 'echo boom >&2; exit 1'"), 60)
        ok(jb["state"] == "failed" and "boom" in str(jb["error"]), f"exit1 -> {jb['state']} {jb['error']}")
    elif CASE == "exit75":
        t = time.time()
        jb = wait_job(tok, cmd_job(tok, "sh -c 'echo flaky >&2; exit 75'"), 60)
        ok(jb["state"] == "failed" and "flaky" in str(jb["error"]), f"exit75 -> {jb['state']} {jb['error']} in {time.time() - t:.0f}s")
        # a flaky program that succeeds on its 3rd run must finish (attempt file in a shared dir)
        d = os.path.join(DIR, "flaky"); os.makedirs(d, exist_ok=True)
        jb = wait_job(tok, cmd_job(tok, f"sh -c 'f={d}/c$CK_START; echo x >> $f; [ $(wc -l < $f) -ge 3 ] && echo ok || exit 75'", 1, 1), 60)
        ok(jb["state"] == "done" and raw_result(tok, jb["id"]).strip() == b"ok", f"flaky-then-ok -> {jb['state']} {jb['error']}")
    elif CASE == "hang":
        jid = cmd_job(tok, f"sh -c 'sleep {SL}'", 4, 2)
        time.sleep(8)
        n0 = len(procs(f"sleep {SL}"))
        s, v, _ = http("POST", f"/api/v1/jobs/{jid}/cancel", {}, tok)
        time.sleep(5)
        n1 = len(procs(f"sleep {SL}"))
        say(f"cancel -> {s}; hung sleeps before {n0} after cancel(5s) {n1}")
        ok(n1 == 0, "cancel kills the hung programs (no orphan sleep)")
        # PC freed: a fresh quick job must run on the same fleet
        t = time.time()
        jb = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 30)
        ok(jb["state"] == "done", f"PCs free after cancel: next job {jb['state']} in {time.time() - t:.1f}s")
        # no cancel: does a hang ever time out? (head limit 60 s min)
        jid = cmd_job(tok, f"sh -c 'sleep {SL}'", 1, 1)
        jb = wait_job(tok, jid, 200)
        say(f"uncancelled hang: {jb['state']} {jb['error']} after {time.time() - t:.0f}s; sleeps still alive {len(procs(f'sleep {SL}'))}")
        ok(jb["state"] == "failed", "hang with no cancel eventually fails the job")
        jb2 = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 30)
        ok(jb2["state"] == "done", f"PCs free after timeout: next job {jb2['state']}")
        ok(len(procs(f"sleep {SL}")) == 0, "no hung process left after the timeout")
    elif CASE == "orphan":
        jid = cmd_job(tok, f"sh -c '(sleep {SL+1} ) & sleep {SL+2}'", 2, 1)
        time.sleep(8)
        say("before cancel:", len(procs(f"sleep {SL+1}")) + len(procs(f"sleep {SL+2}")))
        http("POST", f"/api/v1/jobs/{jid}/cancel", {}, tok)
        time.sleep(5)
        left = procs(f"sleep {SL+1}") + procs(f"sleep {SL+2}")
        ok(not left, f"no orphans after cancel (left {len(left)})")
        # parent exits normally, child survives
        jb = wait_job(tok, cmd_job(tok, f"sh -c '(sleep {SL+3} ) >/dev/null 2>&1 & echo done'", 2, 1), 30)
        ok(jb["state"] == "done", f"{jb['state']} {jb['error']}: job with background child finishes")
        time.sleep(2)
        left = procs(f"sleep {SL+3}")
        ok(not left, f"background child of a finished chunk is reaped (left {len(left)})")
    elif CASE == "mem":
        py = "python3 -c \"import sys;b=[bytearray(1<<20) for _ in range(1800)];[x.__setitem__(slice(None),b'1'*(1<<20)) for x in b];print('big')\""
        t0 = rss_fd_thr(os.getpid())
        jb = wait_job(tok, cmd_job(tok, py, 2, 1), 90)
        say("mem job:", jb["state"], jb["error"])
        jb2 = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 30)
        ok(jb2["state"] == "done", f"cluster alive after 1.8 GB hog (hog job {jb['state']}): next {jb2['state']}")
        _ = t0
    elif CASE == "bigout":
        sc = "sh -c 'truncate -s 10G $CK_OUT/big_$CK_START; echo ok'"
        jb = wait_job(tok, cmd_job(tok, sc, 2, 1), 90)
        ok(jb["state"] == "failed" and "files" in str(jb["error"]).lower(), f"sparse 10G CK_OUT -> {jb['state']} {jb['error']}")
        sc = "sh -c 'dd if=/dev/zero of=$CK_OUT/big_$CK_START bs=1m count=1500 2>/dev/null; echo ok'"
        t = time.time()
        jb = wait_job(tok, cmd_job(tok, sc, 2, 1), 180)
        ok(jb["state"] == "failed", f"real 1.5 GB per chunk CK_OUT -> {jb['state']} {jb['error']} in {time.time() - t:.0f}s")
        import glob
        left = glob.glob("/var/folders/*/*/T/ck-scratch-*") + glob.glob("/tmp/ck-scratch-*")
        say("scratch dirs left:", len(left))
        jb2 = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 30)
        ok(jb2["state"] == "done", "next job fine")
    elif CASE == "garbage":
        sc = "sh -c 'head -c 3000000 /dev/urandom' x"
        jid = cmd_job(tok, sc, 1, 1)
        jb = wait_job(tok, jid, 60)
        body = raw_result(tok, jid) if jb["state"] == "done" else b""
        ok(jb["state"] == "done" and len(body) == 3000000, f"binary garbage 3 MB -> {jb['state']} {jb['error']} got {len(body)} bytes")
        # known binary payload: byte-exact via a deterministic generator
        sc = "python3 -c \"import sys;sys.stdout.buffer.write(bytes(range(256))*2000+b'\\xff\\xfe\\x00\\x00')\""
        jid = cmd_job(tok, sc, 1, 1)
        jb = wait_job(tok, jid, 60)
        want = bytes(range(256)) * 2000 + b"\xff\xfe\x00\x00"
        got = raw_result(tok, jid)
        ok(got == want, f"binary result byte-exact: {len(got)} vs {len(want)} sha {hashlib.sha256(got).hexdigest()[:8]}/{hashlib.sha256(want).hexdigest()[:8]}")
        jid = submit(tok, {"name": "g", "kind": "list", "cmd": "cat", "items": ["a\x00b", "é中", "x" * 4000], "reducer": "lines"})
        jb = wait_job(tok, jid, 60)
        ok(jb["state"] == "done", f"list with odd items -> {jb['state']} {jb['error']}")
    elif CASE == "nondet":
        jid = cmd_job(tok, "sh -c 'echo $RANDOM$RANDOM' sh", 40, 1)
        jb = wait_job(tok, jid, 60)
        ok(jb["state"] == "done" and len(raw_result(tok, jid).split()) >= 1, "nondeterministic output job completes")
        # random stragglers and random 75s (retries) must not double-count or lose a range: sum reducer is exact
        body = "a=$1; b=$2; s=0; [ $RANDOM -lt 5000 ] && sleep 4; [ $RANDOM -lt 6000 ] && [ $CK_ATTEMPT = 1 ] && [ -n \"$FLAKY\" ] && exit 75; while [ $a -lt $b ]; do s=$((s+a+1)); a=$((a+1)); done; echo $s"
        jid = cmd_job(tok, f"sh -c '{body}' sh", 3000, 50, reducer="sum")
        jb = wait_job(tok, jid, 180)
        got = raw_result(tok, jid).strip()
        ok(jb["state"] == "done" and got == str(3000 * 3001 // 2).encode(), f"sum with random stragglers {got!r} want {3000 * 3001 // 2} state {jb['state']} {jb['error']}")
    elif CASE == "poison":
        items = [f"i{i}" for i in range(2000)]
        items[1234] = "POISON"
        cmd = "sh -c 'while read l; do [ \"$l\" = POISON ] && { echo \"bad item POISON\" >&2; exit 1; }; echo $l; done'"
        t = time.time()
        jid = submit(tok, {"name": "poison", "kind": "list", "cmd": cmd, "items": items, "reducer": "lines"})
        jb = wait_job(tok, jid, 120)
        ok(jb["state"] == "failed" and "POISON" in str(jb["error"]), f"poison item -> {jb['state']} {jb['error']} in {time.time() - t:.0f}s")
        # poison that exits 75 forever
        cmd = "sh -c 'while read l; do [ \"$l\" = POISON ] && { echo busy >&2; exit 75; }; echo $l; done'"
        jid = submit(tok, {"name": "poison75", "kind": "list", "cmd": cmd, "items": items, "reducer": "lines"})
        jb = wait_job(tok, jid, 120)
        ok(jb["state"] == "failed", f"poison75 -> {jb['state']} {jb['error']}")
        jb2 = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 30)
        ok(jb2["state"] == "done", "fleet fine afterwards")
    elif CASE == "sdk":
        sdk_case(tok)


def sdk_case(tok: str) -> None:
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "python"))
    import clusterkit as ck  # type: ignore
    ck.connect(URL, token=tok)
    os.makedirs(DIR, exist_ok=True)
    src = os.path.join(DIR, "sdkfns.py")
    open(src, "w").write(
        "import os, time\n"
        "def sq(x):\n    return x * x\n"
        "def boom(x):\n    if x == 7: raise ValueError('poison 7')\n    return x\n"
        "def die(x):\n    if x == 3: os._exit(1)\n    return x\n"
        "def segv(x):\n    import ctypes\n    if x == 3: ctypes.string_at(0)\n    return x\n"
        "def hang(x):\n    if x == 2: time.sleep(10**6)\n    return x\n")
    sys.path.insert(0, DIR)
    import sdkfns  # type: ignore
    items = list(range(200))
    try:
        r = ck.map(sdkfns.sq, items, local="never")
        ok(r == [x * x for x in items], "sdk map exact (200 items)")
    except Exception as e:
        ok(False, f"sdk map exact: {type(e).__name__} {str(e)[:200]}")
    for name in ("boom", "die", "segv"):
        t = time.time()
        try:
            r = ck.map(getattr(sdkfns, name), items, local="never")
            ok(False, f"sdk {name}: returned {len(r)} results without error")
        except Exception as e:
            ok(time.time() - t < 150, f"sdk {name} -> {type(e).__name__}: {str(e).splitlines()[0][:120]} in {time.time() - t:.0f}s")
    t = time.time()
    try:
        ck.map(sdkfns.hang, items[:10], local="never", timeout_s=20)
        ok(False, "sdk hang returned")
    except Exception as e:
        ok(time.time() - t < 150, f"sdk hang timeout_s=20 -> {type(e).__name__}: {str(e)[:100]} in {time.time() - t:.0f}s")
    time.sleep(3)
    jb = wait_job(tok, cmd_job(tok, "echo hi", 2, 1), 40)
    ok(jb["state"] == "done", f"fleet fine after sdk faults ({jb['state']})")


# ---------------------------------------------------------------- HA / fleet cases

SB_N = 3  # heads: primary + 2 standbys, each its own port block


def head_port(i: int) -> int:
    return common.BASE  # standbys share the primary's ports (they listen on nothing until they promote): one Mac, one beacon port


def hurl(i: int) -> str:
    return f"http://127.0.0.1:{head_port(i) + 2}"


def start_head(i: int, code: str | None = None, standby: bool = False, extra: dict | None = None) -> subprocess.Popen:
    e = {"CK_PORT": str(head_port(i)), "CK_NODE_ID": f"lc-head{i}", "CK_KEY_DIR": os.path.join(DIR, f"keys-head{i}")}
    e.update(extra or {})
    args = ["head", "--no-local-worker"]
    if standby:
        e["CK_PRIMARY_PORT"] = str(head_port(0))
        args = ["head", "--standby", "--head", "127.0.0.1", "--code", code or "", "--no-local-worker"]
    return spawn(args, f"head{i}", e, data=f"head{i}")


def heads_env() -> str:
    return ",".join(f"127.0.0.1:{head_port(i)}" for i in range(SB_N))


def start_agent(i: int, code: str, threads: int = 2, extra_args: list[str] | None = None, extra: dict | None = None) -> subprocess.Popen:
    e = {"CK_NODE_ID": f"lc-a{i}", "CK_KEY_DIR": os.path.join(DIR, f"keys-a{i}"), "CK_HEADS": heads_env()}
    e.update(extra or {})
    return spawn(["agent", "--head", "127.0.0.1", "--threads", str(threads), "--name", f"a{i}", "--code", code] + (extra_args or []),
                 f"agent{i}", e, data=f"agent{i}")


def ha(base: str, tok: str) -> dict:
    try:
        return http("GET", "/api/v1/ha", token=tok, base=base, timeout=3)[1]
    except Exception:
        return {}


HS: dict[int, subprocess.Popen] = {}


def primary_idx(tok: str, among: list[int]) -> int | None:
    h = ha(URL, tok)
    if not (isinstance(h, dict) and h.get("role") == "primary"):
        return None
    out = subprocess.run(["lsof", "-nP", f"-iTCP:{common.BASE}", "-sTCP:LISTEN", "-t"], capture_output=True, text=True).stdout.split()
    for i in among:
        if i in HS and str(HS[i].pid) in out:
            return i
    return None


def wait_primary(tok: str, among: list[int], secs: float = 60) -> int | None:
    t = time.time()
    while time.time() - t < secs:
        p = primary_idx(tok, among)
        if p is not None:
            return p
        time.sleep(0.5)
    return None


def ha_fleet(nsb: int = 2, nag: int = 3) -> tuple[str, str, dict[int, subprocess.Popen], list[subprocess.Popen]]:
    hs = HS
    hs[0] = start_head(0)
    wait_up(hurl(0))
    tok = bootstrap()
    code = join_code(tok, 500)
    for i in range(1, nsb + 1):
        hs[i] = start_head(i, code, True)
    ags = [start_agent(i, code) for i in range(nag)]
    t = time.time()
    while time.time() - t < 40:
        h = ha(hurl(0), tok)
        if isinstance(h, dict) and len(h.get("standbys", h.get("peers", []))) >= nsb:
            break
        time.sleep(1)
    time.sleep(6)
    say("ha:", json.dumps(ha(hurl(0), tok))[:300])
    return tok, code, hs, ags


def synth(tok: str, n: int, base: str = URL, ns: int = 20000, chunk: int = 20000, **kw: Any) -> int:
    return submit(tok, dict({"name": CASE, "kind": "synth", "synth_ns": ns, "n": n, "chunk": chunk, "reducer": "sum"}, **kw), base)


def finish_exact(tok: str, jid: int, n: int, among: list[int], secs: float = 240) -> None:
    t = time.time()
    jb: dict = {}
    base = hurl(0)
    while time.time() - t < secs:
        for i in among:
            jb2 = job(tok, jid, hurl(i))
            if jb2:
                jb, base = jb2, hurl(i)
                break
        if jb.get("state") in ("done", "failed", "cancelled"):
            break
        time.sleep(1)
    try:
        res = raw_result(tok, jid, base).strip()
    except Exception as e:
        res = repr(e).encode()
    ok(jb.get("state") == "done" and jb.get("done") == jb.get("total") == n and res == str(n).encode(),
       f"job {jb.get('state')} done {jb.get('done')}/{jb.get('total')} result {res[:40]!r} want {n} in {time.time() - t:.0f}s")


def ha_cases() -> None:
    N = 6 * 10**6
    nsb = 0 if CASE in ("restart1", "stop1", "clientkill_nosb") else 2
    if CASE in ("flap", "many", "nopcs", "dup2"):
        nsb = 0
    tok, code, hs, ags = ha_fleet(nsb, 0 if CASE in ("nopcs", "flap", "many") else 3)
    if CASE == "double":
        jid = synth(tok, N)
        time.sleep(4)
        hs[0].kill(); t = time.time()
        p1 = wait_primary(tok, [1, 2], 40)
        say(f"first failover -> head{p1} in {time.time() - t:.1f}s")
        time.sleep(5)
        other = [i for i in (1, 2) if i != p1][0]
        hs[p1].kill(); t = time.time()
        p2 = wait_primary(tok, [other], 60)
        say(f"second failover -> head{p2} in {time.time() - t:.1f}s")
        ok(p1 is not None and p2 == other, "two failovers elected")
        finish_exact(tok, jid, N, [other])
    elif CASE == "switch":
        jid = synth(tok, N)
        time.sleep(4)
        # find standby node id
        h = ha(hurl(0), tok)
        say("ha", json.dumps(h)[:300])
        s, v, _ = http("POST", "/api/v1/ha/switchover", {"to": h["standbys"][0]["node_id"]}, tok)
        say("switchover", s, str(v)[:150])
        time.sleep(10)
        say("primary now", primary_idx(tok, [0, 1, 2]), ha(URL, tok).get("epoch"))
        finish_exact(tok, jid, N, [0])
    elif CASE == "sbsnap":
        # fat db so the snapshot takes a while, then kill a standby mid-snapshot
        for k in range(3):
            submit(tok, {"name": "fat", "kind": "list", "cmd": "cat", "items": [f"x{i}" * 40 for i in range(100000)], "reducer": "lines"})
        jid = synth(tok, N)
        st3 = start_head(3, code, True, {})
        time.sleep(1.5)
        st3.kill()
        time.sleep(1)
        st3 = start_head(3, code, True, {})
        time.sleep(15)
        say("ha:", json.dumps(ha(hurl(0), tok))[:300])
        ok(hs[0].poll() is None, "primary alive after standby killed mid-snapshot")
        finish_exact(tok, jid, N, [0])
    elif CASE in ("restart1", "stop1"):
        for i in ags:
            i.kill()
        ags = [start_agent(i, code, extra={"CK_HEADS": ""}) for i in range(3)]
        time.sleep(6)
        jid = synth(tok, N)
        time.sleep(4)
        say("progress", job(tok, jid).get("done"))
        if CASE == "restart1":
            hs[0].kill(); time.sleep(1)
            hs[0] = start_head(0)
        else:
            hs[0].send_signal(signal.SIGSTOP); time.sleep(20); hs[0].send_signal(signal.SIGCONT)
        finish_exact(tok, jid, N, [0])
    elif CASE == "cancelfo":
        jid = synth(tok, 10**8, ns=50000, chunk=20000)
        time.sleep(3)
        hs[0].kill()
        s = None
        p1 = None
        t = time.time()
        while time.time() - t < 30 and s != 200:
            p1 = wait_primary(tok, [1, 2], 5)
            if p1 is not None:
                try:
                    s = http("POST", f"/api/v1/jobs/{jid}/cancel", {}, tok, base=hurl(p1), timeout=5)[0]
                except Exception:
                    s = None
            time.sleep(0.5)
        say("cancel on new primary:", s)
        time.sleep(5)
        jb = job(tok, jid, hurl(p1 or 1))
        ok(jb.get("state") == "cancelled", f"cancel during failover -> {jb.get('state')}")
        jb2 = wait_job(tok, synth(tok, 10**6, hurl(p1 or 1)), 60, hurl(p1 or 1))
        ok(jb2.get("state") == "done", "new job runs afterwards")
    elif CASE == "clientkill":
        p = subprocess.Popen([BIN, "run", "--head", "127.0.0.1", "--n", str(N), "--chunk", "20000", "--synth", "20000"], env=common.env({"CK_TOKEN": tok}), start_new_session=True,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        time.sleep(4)
        os.killpg(p.pid, signal.SIGKILL)
        time.sleep(1)
        jobs = http("GET", "/api/v1/jobs", token=tok)[1]["jobs"]
        jid = jobs[0]["id"]
        say("job after client kill:", jobs[0]["state"], jobs[0]["done"])
        finish_exact(tok, jid, N, [0])
    elif CASE == "dup2":
        e = common.env(data="head0")
        p = subprocess.run([BIN, "head", "--no-local-worker"], env=e, capture_output=True, text=True, timeout=30) if False else None
        _ = p
        second = spawn(["head", "--no-local-worker"], "head-dup", {}, data="head0")
        time.sleep(5)
        rc = second.poll()
        say("second head rc", rc)
        ok(rc is not None and rc != 0, "second head on same CK_DATA refuses")
        ok(hs[0].poll() is None and http("GET", "/api/v1/me", token=tok)[0] == 200, "first head unaffected")
        # different port, same data
        third = spawn(["head", "--no-local-worker"], "head-dup2", {"CK_PORT": str(common.BASE + 10)}, data="head0")
        time.sleep(5)
        rc = third.poll()
        ok(rc is not None and rc != 0, f"second head on same CK_DATA, other port, refuses (rc {rc})")
    elif CASE == "nopcs":
        jid = synth(tok, N)
        time.sleep(6)
        jb = job(tok, jid)
        ok(jb["state"] in ("queued", "running") and jb["done"] == 0, f"no agents: job waits ({jb['state']} done {jb['done']})")
        for i in range(3):
            start_agent(i, code)
        finish_exact(tok, jid, N, [0], 120)
    elif CASE == "many":
        many(tok, code)
    elif CASE == "flap":
        flap(tok, code, hs[0])


def many(tok: str, code: str) -> None:
    users = [f"u{i}" for i in range(5)]
    toks = {}
    for u in users:
        s, v, _ = http("POST", "/api/v1/users", {"name": u, "password": "stress-test-password-1", "role": "operator"}, tok)
        _, _, sc = http("POST", "/api/v1/login", {"name": u, "password": "stress-test-password-1"})
        toks[u] = http("POST", "/api/v1/tokens", {}, cookie=sc.split(";")[0])[1]["token"] if sc else tok
    say("users", {u: toks[u][:6] for u in users})
    http("PUT", "/api/v1/users/u4/quota", {"max_pcs": 1, "max_running": 2}, tok)
    for i in range(3):
        start_agent(i, code)
    time.sleep(6)
    jids = []
    t0 = time.time()
    for k in range(200):
        u = users[k % 5]
        jids.append((k, u, submit(toks[u], {"name": f"j{k}", "kind": "synth", "synth_ns": 200000, "n": 20000 + k, "chunk": 5000, "reducer": "sum"})))
    say(f"submitted 200 in {time.time() - t0:.1f}s")
    bad = 0
    fin: dict[int, float] = {}
    while time.time() - t0 < 400 and len(fin) < 200:
        for k, u, jid in jids:
            if k in fin:
                continue
            jb = job(tok, jid)
            if jb.get("state") in ("done", "failed", "cancelled"):
                fin[k] = time.time() - t0
                if jb["state"] != "done" or raw_result(tok, jid).strip() != str(20000 + k).encode():
                    bad += 1
        time.sleep(2)
    per_user = {u: sorted(v for k, v in fin.items() if k % 5 == int(u[1:])) for u in users}
    say("finish time of last job per user:", {u: round(v[-1]) if v else None for u, v in per_user.items()}, "median", {u: round(v[len(v) // 2]) if v else None for u, v in per_user.items()})
    ok(len(fin) == 200 and bad == 0, f"200 jobs finished {len(fin)}, wrong/failed {bad}, total {time.time() - t0:.0f}s")


def flap(tok: str, code: str, head: subprocess.Popen) -> None:
    N = 20 * 10**6
    NV = 200
    ag = start_agent(0, code, 1, ["--virtual", str(NV)], {"CK_HEADS": ""})
    time.sleep(25)
    nodes = len(http("GET", "/api/v1/nodes", token=tok)[1]["nodes"])
    say("virtual nodes joined:", nodes)
    jid = synth(tok, N, ns=2000000, chunk=500, policy="fixed")
    base = rss_fd_thr(head.pid)
    samples = [base]
    t0 = time.time()
    n_flap = 0
    DUR = float(os.environ.get("FLAP_SECS", "180"))
    while time.time() - t0 < DUR:
        time.sleep(3)
        ag.kill(); n_flap += 1
        time.sleep(0.5)
        ag = start_agent(0, code, 1, ["--virtual", str(NV)], {"CK_HEADS": ""})
        samples.append(rss_fd_thr(head.pid))
        if job(tok, jid).get("state") in ("done", "failed"):
            break
    say(f"flaps {n_flap}; head (rss KiB, fds, threads) start {samples[0]} mid {samples[len(samples) // 2]} end {samples[-1]}")
    finish_exact(tok, jid, N, [0], 600)
    time.sleep(10)
    end = rss_fd_thr(head.pid)
    ok(end[1] <= base[1] * 1.3 + 50 and end[2] <= base[2] * 1.3 + 20, f"head fds/threads flat: {base} -> {end}")
    ok(end[0] <= base[0] * 1.5 + 100000, f"head RSS flat-ish: {base[0] // 1024} -> {end[0] // 1024} MiB")


JOB_CASES = {"segv", "exit1", "exit75", "hang", "orphan", "mem", "bigout", "garbage", "nondet", "poison", "sdk"}
try:
    if CASE in JOB_CASES:
        job_faults()
    else:
        ha_cases()
except Exception as ex:
    import traceback
    traceback.print_exc()
    VERDICT.append(f"BAD exception {ex!r}")
finally:
    cleanup()
    reap(MARK)
    for k in range(4):
        reap(f"sleep {SL + k}")
    say("RESULT", CASE, "FAIL" if any(v.startswith("BAD") for v in VERDICT) or not VERDICT else "PASS", "; ".join(VERDICT)[:600])
