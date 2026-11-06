#!/usr/bin/env python3
"""Round 3 / t1: network bad cases through a userspace TCP fault proxy (stdlib only; no tc/netem on macOS).
  CK_BIN=... STRESS_DIR=... CK_PORT=9700 python3 -I scripts/stress/netfault.py all|mode_<m>|oneblack|allblack|sbblack|clientflap|clientblack|virt200
  <m> = slow latency jitter reset flap halfopen truncate blackhole.   `all` runs every case in its own STRESS_DIR/<case>.
Proxy modes (switch at runtime): pass blackhole reset slow latency flap halfopen truncate. Agents, the standby and `ck run` take
--head host:port, so the proxy sits between them and the head. Open fleet; exact job result (synth sum == N) is the check.
Ports used: CK_PORT base (head), +10 standby, +20.. proxies, +30 agents. One RESULT line per case."""
from __future__ import annotations

import os
import queue
import random
import socket
import struct
import subprocess
import sys
import threading
import time
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
from common import BASE, DIR, URL, bootstrap, cleanup, http, rss_fd_thr, say, spawn, wait_up  # noqa: E402

PPORT = BASE + 20  # first proxy port
AGP = str(BASE + 30)  # CK_PORT of agent processes (keeps their discovery/blob ports off the head's)
LOG: list[str] = []


def result(case: str, verdict: str, detail: str) -> None:
    say(f"RESULT {case} | {verdict} | {detail}")


# ------------------------------------------------------------------ proxy --
def rst(s: socket.socket | None) -> None:
    if s is None:
        return
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    except OSError:
        pass
    try:
        s.close()
    except OSError:
        pass


class Conn:
    def __init__(self, c: socket.socket, u: socket.socket | None) -> None:
        self.c, self.u, self.dead, self.dirty = c, u, False, False
        self.sent = [0, 0]
        self.cut = random.randint(20_000, 300_000)  # truncate mode: close after this many head->client bytes
        self.open = 2

    def kill(self, hard: bool = True) -> None:
        self.dead = True
        for s in (self.c, self.u):
            if hard:
                rst(s)
            elif s is not None:
                try:
                    s.close()
                except OSError:
                    pass


class Proxy:
    """Listens on 127.0.0.1:lport, forwards to target. Modes: pass blackhole reset slow latency flap halfopen truncate."""

    def __init__(self, lport: int, target: tuple[str, int]) -> None:
        self.target, self.mode, self.arg = target, "pass", {}
        self.conns: set[Conn] = set()
        self.bytes = [0, 0]  # forwarded bytes: agent->head, head->agent
        self.lock = threading.Lock()
        self.flap_id = 0
        self.ls = socket.socket()
        self.ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.ls.bind(("127.0.0.1", lport))
        self.ls.listen(256)
        threading.Thread(target=self._accept, daemon=True).start()

    def set(self, mode: str, **arg: Any) -> None:
        with self.lock:
            old, self.mode, self.arg = self.mode, mode, arg
            self.flap_id += 1
            victims = list(self.conns)
        if mode == "reset":
            for c in victims:
                c.kill()
        elif mode == "truncate":  # existing streams get cut too, a few KB from now
            for c in victims:
                c.cut = c.sent[1] + random.randint(*arg.get("cut", (20_000, 300_000)))
        elif old in ("blackhole", "halfopen") and mode != old:
            for c in victims:
                if c.dirty and c.u is not None:  # a stream with swallowed bytes cannot resume: RST (what a NAT/middlebox does)
                    c.kill()
        if mode == "flap":
            threading.Thread(target=self._flap, args=(self.flap_id, arg.get("up", 2.0), arg.get("down", 2.0)), daemon=True).start()

    def _flap(self, fid: int, up: float, down: float) -> None:
        while self.flap_id == fid:
            self.mode = "pass"
            time.sleep(up)
            if self.flap_id != fid:
                return
            self.mode = "reset"
            for c in list(self.conns):
                c.kill()
            time.sleep(down)

    def _accept(self) -> None:
        while True:
            try:
                c, _ = self.ls.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(c,), daemon=True).start()

    def _serve(self, c: socket.socket) -> None:
        if self.mode == "reset":
            return rst(c)
        if self.mode == "blackhole":  # accept, swallow, never reply, never FIN
            cn = Conn(c, None)
            cn.dirty = True
            c.settimeout(1.0)
            while not cn.dead:
                try:
                    if not c.recv(65536):
                        break
                except socket.timeout:
                    continue
                except OSError:
                    break
            return
        try:
            u = socket.create_connection(self.target, timeout=5)
        except OSError:
            return rst(c)
        cn = Conn(c, u)
        if self.mode == "truncate":
            cn.cut = random.randint(*self.arg.get("cut", (20_000, 300_000)))
        with self.lock:
            self.conns.add(cn)
        for d, (a, b) in enumerate(((c, u), (u, c))):
            q: queue.Queue = queue.Queue()
            threading.Thread(target=self._read, args=(cn, d, a, q), daemon=True).start()
            threading.Thread(target=self._write, args=(cn, d, b, q), daemon=True).start()

    def _read(self, cn: Conn, d: int, a: socket.socket, q: queue.Queue) -> None:
        a.settimeout(0.5)
        last = 0.0
        while not cn.dead:
            try:
                data = a.recv(65536)
            except socket.timeout:
                continue
            except OSError:
                break
            if not data:
                break
            m, arg = self.mode, self.arg
            if m == "blackhole" or (m == "halfopen" and d == arg.get("drop", 1)):
                cn.dirty = True
                continue
            if m == "reset":
                cn.kill()
                return
            if m == "truncate" and d == 1 and cn.sent[1] + len(data) > cn.cut:
                q.put((0.0, data[: max(0, cn.cut - cn.sent[1])], True))
                return
            cn.sent[d] += len(data)
            lat = 0.0
            if m == "latency":
                lat = random.uniform(*arg["ms"]) / 1000 if "ms" in arg else 0.0
            last = max(last, time.time() + lat)
            q.put((last, data, False))
        q.put(None)

    def _write(self, cn: Conn, d: int, b: socket.socket, q: queue.Queue) -> None:
        try:
            while not cn.dead:
                try:
                    it = q.get(timeout=0.5)
                except queue.Empty:
                    continue
                if it is None:
                    b.shutdown(socket.SHUT_WR)
                    break
                due, data, cut = it
                while time.time() < due and not cn.dead:
                    time.sleep(min(0.05, due - time.time()))
                b.sendall(data)
                self.bytes[d] += len(data)
                if cut:
                    cn.kill(hard=False)  # FIN in the middle of a frame
                    return
                if self.mode == "slow":
                    time.sleep(len(data) / float(self.arg.get("bps", 65536)))
        except OSError:
            pass
        with self.lock:
            cn.open -= 1
            if cn.open == 0:
                cn.kill(hard=False)
                self.conns.discard(cn)


# ---------------------------------------------------------------- cluster --
def start_head(extra: dict | None = None, base: int = BASE, name: str = "head", args: list[str] | None = None, data: str = "data") -> subprocess.Popen:
    return spawn(["head", "--no-local-worker"] + (args or []), name, dict({"CK_PORT": str(base)}, **(extra or {})), data=data, fleet="open")


def start_agent(i: int, port: int, virtual: int = 0, extra: dict | None = None) -> subprocess.Popen:
    """port 0 = no --head: discover heads by beacon (follows a promoted standby), CK_PORT = head's."""
    a = ["agent", "--threads", "2", "--name", f"a{i}" if port else "lab"] + (["--head", f"127.0.0.1:{port}"] if port else []) + (["--virtual", str(virtual)] if virtual else [])
    return spawn(a, f"agent{i}", dict({"CK_NODE_ID": f"nf-a{i}", "CK_PORT": AGP if port else str(BASE + 24 + 4 * i)}, **(extra or {})), data=f"agent{i}", fleet="open")


def online(tok: str, base: str = URL) -> dict[str, bool]:
    try:
        s, v, _ = http("GET", "/api/v1/nodes", token=tok, base=base, timeout=3)
        return {n["node_id"]: bool(n.get("online")) for n in v["nodes"]} if s == 200 else {}
    except Exception:
        return {}


def n_online(tok: str) -> int:
    return sum(1 for v in online(tok).values() if v)  # fresh data dir per case: every node in the table is one of ours


def wait_for(fn: Any, secs: float, step: float = 0.25) -> float | None:
    t = time.time()
    while time.time() - t < secs:
        if fn():
            return time.time() - t
        time.sleep(step)
    return None


def submit(tok: str, n: int, chunk: int, ns: int, base: str = URL) -> int:
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "nf", "kind": "synth", "synth_ns": ns, "n": n, "chunk": chunk}, tok, base=base)
    assert s in (200, 201), (s, j)
    return j["job"]["id"]


def wait_job(tok: str, jid: int, secs: float, base: str = URL) -> dict:
    t, jb = time.time(), {}
    while time.time() - t < secs:
        try:
            s, v, _ = http("GET", f"/api/v1/jobs/{jid}", token=tok, base=base, timeout=5)
            if s == 200:
                jb = v["job"]
                if jb["state"] in ("done", "failed", "cancelled"):
                    return jb
        except Exception:
            pass
        time.sleep(0.5)
    return jb


def total(tok: str, jid: int, base: str = URL) -> str:
    return str(http("GET", f"/api/v1/jobs/{jid}/result", token=tok, base=base)[1]).strip()


def judge(case: str, tok: str, jid: int, n: int, t0: float, secs: float, extra: str = "", base: str = URL) -> bool:
    jb = wait_job(tok, jid, secs, base)
    got = total(tok, jid, base) if jb.get("state") == "done" else "-"
    ok = jb.get("state") == "done" and got == str(n) and jb.get("done") == n
    result(case, "PASS" if ok else "BUG", f"state={jb.get('state')} result={got} want={n} done={jb.get('done')} {time.time() - t0:.1f}s {extra}")
    return ok


NS, CH = 10000, 50000  # synth: 10 us/number, 50k per chunk = 0.5 s CPU per chunk


def lost_lines(name: str = "head") -> int:
    try:
        return sum(1 for ln in open(os.path.join(DIR, name + ".log")) if ln.startswith("- worker"))
    except OSError:
        return -1


# ------------------------------------------------------------------ cases --
MODES: dict[str, dict] = {
    "slow": {"bps": 4096}, "latency": {"ms": (1000, 1000)}, "jitter": {"ms": (200, 2000)}, "reset": {}, "flap": {"up": 2.0, "down": 2.0},
    "halfopen": {"drop": 1}, "truncate": {"cut": (2000, 20000)}, "blackhole": {},
}


def case_mode(m: str) -> None:
    start_head(); wait_up(); tok = bootstrap()
    px = Proxy(PPORT, ("127.0.0.1", BASE))
    for i in range(3):
        start_agent(i, PPORT)
    up = wait_for(lambda: n_online(tok) == 3, 20)
    n = 12_000_000
    t0 = time.time(); jid = submit(tok, n, CH, NS)
    time.sleep(3)
    lost0 = lost_lines()
    px.set("latency" if m in ("latency", "jitter") else m, **MODES[m])
    dur = 12.0 if m == "flap" else 10.0
    low = 3
    end = time.time() + dur
    while time.time() < end:
        low = min(low, n_online(tok))
        time.sleep(0.5)
    px.set("pass")
    back = wait_for(lambda: n_online(tok) == 3, 30)
    judge(f"mode_{m}", tok, jid, n, t0, 150, f"fault={dur:.0f}s min_online={low} lost_lines+={lost_lines() - lost0} rejoin_after_restore={back and round(back, 1)}s joined_at={up and round(up, 1)}s")


def case_oneblack() -> None:
    start_head(); wait_up(); tok = bootstrap()
    pxs = [Proxy(PPORT + i, ("127.0.0.1", BASE)) for i in range(6)]
    for i in range(6):
        start_agent(i, PPORT + i)
    wait_for(lambda: n_online(tok) == 6, 25)
    n = 24_000_000
    t0 = time.time(); jid = submit(tok, n, CH, NS)
    time.sleep(3)
    tb = time.time(); pxs[0].set("blackhole")
    det = wait_for(lambda: n_online(tok) < 6, 30, 0.1)
    ok = judge("oneblack", tok, jid, n, t0, 150, f"detected_lost_after={det and round(det, 1)}s (docs: ~5 s) others_online={n_online(tok) - 0}")
    pxs[0].set("pass")
    back = wait_for(lambda: n_online(tok) == 6, 30)
    result("oneblack_rejoin", "PASS" if back is not None else "BUG", f"agent0 online again {back and round(back, 1)}s after restore (stalled handshake conns left open) ok={ok} t={time.time() - tb:.0f}s")


def case_allblack() -> None:
    start_head(); wait_up(); tok = bootstrap()
    px = Proxy(PPORT, ("127.0.0.1", BASE))
    for i in range(3):
        start_agent(i, PPORT)
    wait_for(lambda: n_online(tok) == 3, 20)
    n = 12_000_000
    t0 = time.time(); jid = submit(tok, n, CH, NS)
    time.sleep(3)
    d0 = wait_job(tok, jid, 0.1).get("done")
    px.set("blackhole"); time.sleep(30)
    d1 = wait_job(tok, jid, 0.1).get("done")
    px.set("pass")
    back = wait_for(lambda: n_online(tok) == 3, 40)
    judge("allblack30", tok, jid, n, t0, 150, f"done_at_cut={d0} done_after_30s={d1} rejoin={back and round(back, 1)}s lost_lines={lost_lines()}")


def ha(base: str, tok: str) -> str:
    try:
        s, v, _ = http("GET", "/api/v1/ha", token=tok, base=base, timeout=2)
        return f"{v['role']}/e{v['epoch']}/disp={v.get('dispatching')}/conn={v.get('connected')}" if s == 200 else f"http{s}"
    except Exception:
        return "down"


def case_sbblack() -> None:
    SB = BASE + 10
    SBU = f"http://127.0.0.1:{SB + 2}"
    ag = ",".join(str(BASE + 25 + 4 * i) for i in range(3))  # agents own a CK_PORT each (one machine): beacons are sent to their discovery ports
    start_head({"CK_BEACON_PORTS": f"{SB + 1},{ag}", "CK_NODE_ID": "nf-head"}); wait_up(); tok = bootstrap()
    http("PUT", "/api/v1/ha/settings", {"target_standbys": 0, "selector": "!virtual"}, tok)  # no agent-recruited standbys: only ours
    px = Proxy(PPORT, ("127.0.0.1", BASE))
    spawn(["head", "--standby", "--head", f"127.0.0.1:{PPORT}", "--no-local-worker"], "standby",
          {"CK_PORT": str(SB), "CK_PRIMARY_PORT": str(PPORT), "CK_BEACON_PORTS": f"{BASE + 1},{ag}", "CK_NODE_ID": "nf-standby"}, data="standby", fleet="open")
    for i in range(3):
        start_agent(i, 0)
    wait_for(lambda: n_online(tok) == 3, 20)
    time.sleep(4)
    say("before:", ha(URL, tok), "|", ha(SBU, tok))
    n = 18_000_000
    t0 = time.time(); jid = submit(tok, n, CH, NS)
    time.sleep(3)
    px.set("blackhole"); tb = time.time()
    tl, last = [], ""
    while time.time() - tb < 45:
        cur = f"P[{ha(URL, tok)}] S[{ha(SBU, tok)}]"
        if cur != last:
            tl.append(f"+{time.time() - tb:.0f}s {cur}"); last = cur
        time.sleep(1)
    px.set("pass")
    for _ in range(15):
        time.sleep(1)
        cur = f"P[{ha(URL, tok)}] S[{ha(SBU, tok)}]"
        if cur != last:
            tl.append(f"+{time.time() - tb:.0f}s {cur}"); last = cur
    for ln in tl:
        say("  timeline", ln)
    # the job lives on whichever head ended up primary
    for base in (URL, SBU):
        if ha(base, tok).startswith("primary") or ha(base, tok).startswith("candidate"):
            jb = wait_job(tok, jid, 0.1, base)
            say("  job on", base, jb.get("state"), jb.get("done"))
    heads = [b for b in (URL, SBU) if ha(b, tok).split("/")[0] in ("primary", "candidate")]
    jb = {}
    for b in heads:
        jb = wait_job(tok, jid, 150, b)
        if jb.get("state") == "done":
            judge("sbblack", tok, jid, n, t0, 5, f"final_heads={heads} timeline={len(tl)} entries", b)
            return
    result("sbblack", "BUG", f"job not done on {heads}: {jb.get('state')} {jb.get('done')}")


def run_client(port: int, n: int, tok: str = "") -> tuple[int, str, float]:
    t = time.time()
    p = subprocess.run([common.BIN, "run", "--head", "127.0.0.1", "--synth", str(NS), "--n", str(n), "--chunk", str(CH)],
                       capture_output=True, text=True, timeout=240, env=common.env({"CK_FAILOVER_SECS": "60", "CK_PORT": str(port), "CK_TOKEN": tok}, fleet="open"))  # CK_TOKEN: needed on a secured fleet
    return p.returncode, p.stdout + p.stderr, time.time() - t


def case_client(kind: str) -> None:
    start_head(); wait_up(); tok = bootstrap()
    px = Proxy(PPORT, ("127.0.0.1", BASE))
    for i in range(3):
        start_agent(i, BASE)
    time.sleep(3)
    n = 12_000_000
    box: list = []
    cl = threading.Thread(target=lambda: box.append(run_client(PPORT, n, tok)))
    cl.start(); time.sleep(2)
    if kind == "clientflap":
        px.set("flap", up=2.0, down=2.0); time.sleep(16)
    else:
        px.set("blackhole"); time.sleep(12)
    px.set("pass")
    cl.join(250)
    if not box:
        return result(kind, "BUG", "ck run never returned")
    rc, out, dt = box[0]
    ok = rc == 0 and f"{n} numbers counted" in out
    result(kind, "PASS" if ok else "BUG", f"rc={rc} {dt:.1f}s tail={out.strip().splitlines()[-2:] if out.strip() else out!r}")


def case_virt200(flap: bool = True) -> None:
    hp = start_head(); wait_up(); tok = bootstrap()
    px = Proxy(PPORT, ("127.0.0.1", BASE))
    ap = start_agent(0, PPORT, virtual=200)
    up = wait_for(lambda: n_online(tok) >= 190, 60)
    say("virtual agents online:", n_online(tok), "after", up)
    n = 40_000_000
    t0 = time.time(); jid = submit(tok, n, 20000, NS * 2)
    base = rss_fd_thr(hp.pid); say("head rss/fds/threads at start", base)
    if flap:
        px.set("flap", up=2.0, down=2.0)
    samples = []
    while time.time() - t0 < 120:
        samples.append(rss_fd_thr(hp.pid)); say(f"  t={time.time() - t0:.0f}s head={samples[-1]} agentproc={rss_fd_thr(ap.pid)} online={n_online(tok)}")
        time.sleep(10)
    px.set("pass")
    end = rss_fd_thr(hp.pid)
    tr = time.time()
    back = wait_for(lambda: n_online(tok) >= 190, 120)
    d_at = wait_job(tok, jid, 0.1).get("done")
    mx = [max(s[k] for s in samples) for k in range(3)]
    judge("virt200_flap2min" if flap else "virt200_nofault", tok, jid, n, t0, 400, f"restored_at={tr - t0:.0f}s rejoin_190={back and round(back, 1)}s done_at_restore={d_at} head start={base} max={tuple(mx)} end={end} agent_proc={rss_fd_thr(ap.pid)} lost_lines={lost_lines()}")


def lan_ip() -> str:
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.connect(("8.8.8.8", 80))  # no packet is sent
    return u.getsockname()[0]


def case_blob(kind: str) -> None:
    """50 MB blob through a proxy bound to 127.0.0.1:<head blob port> (the head binds 0.0.0.0 there, agents dial <head ip>:<blob port>)."""
    import hashlib
    import urllib.request
    start_head(); wait_up(); tok = bootstrap()
    bp = Proxy(BASE + 3, (lan_ip(), BASE + 3))
    Proxy(PPORT, ("127.0.0.1", BASE))
    data = os.urandom(50 << 20); sha = hashlib.sha256(data).hexdigest()
    rq = urllib.request.Request(URL + "/api/v1/blobs", data=data, method="PUT", headers={"Authorization": "Bearer " + tok})
    say("blob put:", urllib.request.urlopen(rq, timeout=60).read()[:80])
    if kind == "blobslow":
        bp.set("slow", bps=1_000_000)
    else:
        bp.set("truncate", cut=(3 << 20, 15 << 20))
    for i in range(2):
        start_agent(i, PPORT)
    wait_for(lambda: n_online(tok) == 2, 20)
    t0 = time.time()
    s, j, _ = http("POST", "/api/v1/jobs", {"name": "blob", "kind": "cmd", "cmd": 'shasum -a 256 "$CK_BLOB_0" | cut -c1-64 #', "n": 8, "chunk": 1, "blobs": [sha]}, tok)
    assert s in (200, 201), (s, j)
    jid = j["job"]["id"]
    jb = wait_job(tok, jid, 240)
    lines = [ln.strip() for ln in total(tok, jid).splitlines() if ln.strip()] if jb.get("state") == "done" else []
    cached = []
    for i in range(2):
        f = os.path.join(DIR, f"agent{i}", "cache", sha)
        if os.path.exists(f):  # an agent that was given no chunk never fetches
            cached.append(hashlib.sha256(open(f, "rb").read()).hexdigest() == sha)
    ok = jb.get("state") == "done" and len(lines) >= 1 and set(lines) == {sha} and bool(cached) and all(cached)
    result(kind, "PASS" if ok else "BUG", f"state={jb.get('state')} lines={len(lines)} all_match={set(lines) == {sha}} cache_hash_ok={cached} {time.time() - t0:.0f}s head->agents={bp.bytes[1] >> 20}MB (ideal 100)")


CASES: dict[str, Any] = {f"mode_{m}": (lambda m=m: case_mode(m)) for m in MODES}
CASES.update(oneblack=case_oneblack, allblack=case_allblack, sbblack=case_sbblack, clientflap=lambda: case_client("clientflap"),
             clientblack=lambda: case_client("clientblack"), virt200=case_virt200, virt200base=lambda: case_virt200(False),
             blobslow=lambda: case_blob("blobslow"), blobtrunc=lambda: case_blob("blobtrunc"))


def main() -> None:
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which == "all":
        for c in CASES:
            d = os.path.join(DIR, c)
            subprocess.run([sys.executable, "-I", os.path.abspath(__file__), c], env=dict(os.environ, STRESS_DIR=d), check=False)
        return
    try:
        CASES[which]()
    except Exception as e:  # noqa: BLE001
        result(which, "BUG", f"harness error {type(e).__name__}: {e}")
    finally:
        cleanup()


if __name__ == "__main__":
    main()
