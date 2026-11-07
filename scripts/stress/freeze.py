#!/usr/bin/env python3
"""Freeze hunter: pause (SIGSTOP/SIGCONT) or kill -9 every role of a fleet at random moments and check that every job still ends.

  freeze.py list                         print the case matrix
  freeze.py baseline [fleet/kind]        no-fault time of every (fleet, job kind), saved in FZ_DIR/baseline.json (a name re-measures that one)
  freeze.py run [--workers 3] [--only ID,TEXT,..] [--limit N] [--skip N] [--seed 1]   run the matrix (results in FZ_DIR/results.jsonl;
                                         another --seed moves every fault to another moment)
  freeze.py case ID                      run one case
  freeze.py up plain|hier                start a fleet and leave it running (debugging; Ctrl-C stops it)

Fleets: plain = head + 1 standby + 3 agent processes x 4 virtual PCs; hier = root + root standby + 2 labs (lab heads in auto mode,
lab_head_standby on), 3 agent processes x 4 virtual PCs per lab.
Faults: pause 2 / 8 / 40 s, or kill -9, on head, standby, lab head, lab-head standby, agent, `ck run` client, SDK caller; two roles
paused at once; a pause that starts during a failover; cancel during a fault.
Checks: the job (or all jobs of a client) reaches done with an exact result, or a clean cancelled/failed state, within
3 x no-fault time + fault time + 30 s; afterwards no lease, no in-flight chunk, no orphan process. Progress is sampled every second
so a stall shows as "no progress for N s while k PCs idle".

Env: CK_BIN (default rust/target/release/ck), FZ_DIR (work dir), FZ_BASE (first port, default 13000; 100 ports per fleet slot).
Every process runs with CK_FLEET=open CK_AUTH=off CK_PRESENCE=off CK_POWER_DRY=1 and its own CK_DATA. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from typing import Any

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
BIN = os.environ.get("CK_BIN") or os.path.join(REPO, "rust", "target", "release", "ck")
WORK = os.environ.get("FZ_DIR") or tempfile.mkdtemp(prefix="freeze-")
BASE = int(os.environ.get("FZ_BASE", "13000"))
ENV = dict(os.environ, CK_POWER_DRY="1", CK_PRESENCE="off", CK_FLEET="open", CK_AUTH="off")
ENV.pop("CK_HEAD", None)
ENV.pop("CK_TOKEN", None)
N_SYNTH, NS_SYNTH = 16_000_000, 20_000
N_CMD, CHUNK_CMD, SLEEP_CMD = 60_000, 1000, 4.0
N_LIST, SLEEP_LIST = 180, 1.5
SDK_PARTS, SDK_SLEEP = 72, 1.5
STALL_S = 15.0  # no progress for this long while a PC is idle is recorded as a stall
ROLES_PLAIN = ["head", "standby", "agent", "run", "sdk"]
ROLES_HIER = ROLES_PLAIN + ["labhead", "labstandby"]
KINDS = ["synth", "cmd", "list", "sdk"]


def now() -> float:
    return time.time()


def fnv(s: str) -> int:
    h = 0xcbf29ce484222325
    for ch in s.encode():
        h = ((h ^ ch) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def port_free(p: int) -> bool:
    for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
        s = socket.socket(socket.AF_INET, kind)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)  # connections in TIME_WAIT do not make a port busy (the heads use it too)
        try:
            s.bind(("0.0.0.0", p))
        except OSError:
            return False
        finally:
            s.close()
    return True


def http(method: str, port: int, path: str, body: Any = None, timeout: float = 4.0) -> Any:
    r = urllib.request.Request(f"http://127.0.0.1:{port + 2}/api/v1{path}", method=method, data=json.dumps(body).encode() if body is not None else None,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as x:
            raw = x.read()
            return json.loads(raw) if raw[:1] in (b"{", b"[") else raw.decode()
    except urllib.error.HTTPError as e:
        return {"http": e.code, "body": e.read().decode()[:300]}
    except Exception as e:  # unreachable / timeout
        return {"error": type(e).__name__}


def ps_lines() -> list[tuple[int, int, str]]:
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,command="], capture_output=True, text=True).stdout
    rows = []
    for ln in out.splitlines():
        m = re.match(r"\s*(\d+)\s+(\d+)\s+(.*)", ln)
        if m:
            rows.append((int(m.group(1)), int(m.group(2)), m.group(3)))
    return rows


FLEETS: list[Any] = []  # every live fleet, so a signal can stop them all


def die(*_: Any) -> None:
    for f in list(FLEETS):
        f.stop()
    os._exit(143)


class Fleet:
    """One fleet in its own directory, ports base..base+99. Never touches other fleets' processes: the binary is a private hard link."""

    def __init__(self, name: str, kind: str, slot: int, block_blob: bool = False) -> None:
        self.name, self.kind, self.slot, self.block_blob = name, kind, slot, block_blob
        self.squat: list[socket.socket] = []  # listeners on the blob ports of the heads (blob port = CK_PORT + 3): "port taken"
        self.R = BASE + 100 * slot
        for _ in range(40):  # the previous fleet of this slot may still be closing its sockets
            if all(port_free(self.R + d) for d in (0, 1, 2, 10)):
                break
            time.sleep(0.5)
        else:
            raise RuntimeError(f"ports {self.R}.. are in use by someone else")
        self.dir = f"{WORK}/fleets/{name}"
        subprocess.run(["rm", "-rf", self.dir])
        os.makedirs(self.dir)
        self.bin = f"{self.dir}/ck"
        os.link(BIN, self.bin) if os.stat(BIN).st_dev == os.stat(self.dir).st_dev else subprocess.run(["cp", BIN, self.bin])
        self.procs: dict[str, subprocess.Popen[bytes]] = {}
        # plain: the standby has its own port, so it can take over while a paused head still holds the head's. hier: the same port (lab heads are
        # told "the standby is at <its ip>:<root port>"): a takeover works after kill -9 only; a paused root cannot be replaced on one machine
        self.sp = self.R if kind == "hier" else self.R + 10
        self.heads = f"127.0.0.1:{self.R}" + (f",127.0.0.1:{self.sp}" if self.sp != self.R else "")
        self.labs = ["A", "B"] if kind == "hier" else []
        self.nodes: dict[str, list[str]] = {}
        FLEETS.append(self)

    def start(self, key: str, args: list[str], port: int, env: dict[str, str] | None = None, data: str | None = None) -> None:
        e = dict(ENV, CK_PORT=str(port), CK_DATA=f"{self.dir}/data-{data or key}", **(env or {}))
        self.procs[key] = subprocess.Popen([self.bin] + args, env=e, stdout=open(f"{self.dir}/{key}.log", "w"), stderr=subprocess.STDOUT,
                                           stdin=subprocess.DEVNULL, start_new_session=True)

    def up(self) -> None:
        hier_env: dict[str, str] = {}
        lo = 0
        if self.kind == "hier":
            # lab heads the root starts listen on lab_head_port + 10 x (fnv(lab) % 50): pick two lab names whose slots are close
            names = sorted(("ABCDEFGHIJKLMNOP"), key=lambda c: fnv(c) % 50)
            self.labs = sorted(names[:2], key=lambda c: fnv(c) % 50)
            lo = fnv(self.labs[0]) % 50
            hier_env = {"CK_HIER_ALLOW_VIRTUAL": "1", "CK_TUNE_LAB_HEAD_PORT": str(self.R + 40 - 10 * lo), "CK_TUNE_LAB_HEAD_MIN_PCS": "4"}
        if self.block_blob:
            ports = [self.R + 3, self.sp + 3] + ([self.R + 40 - 10 * lo + 10 * (fnv(lb) % 50 - lo) + d + 3 for lb in self.labs for d in (0, 5)] if self.kind == "hier" else [])
            for bp in ports:
                sq = socket.socket()
                try:
                    sq.bind(("0.0.0.0", bp))
                    sq.listen(5)
                    self.squat.append(sq)
                except OSError:
                    sq.close()  # taken already: that is what we want
        self.start("root", ["head", "--no-local-worker"], self.R, hier_env)
        time.sleep(1)
        self.start("standby", ["head", "--standby", "--head", "127.0.0.1", "--no-local-worker"], self.sp, {"CK_PRIMARY_PORT": str(self.R), **hier_env}, data="standby")
        # 12 virtual PCs per fleet: plain 3 x 4, hier 6 x 2 (3 agent processes per lab)
        n_agents = 6 if self.kind == "hier" else 3
        for i in range(n_agents):
            self.start(f"pc{i}", ["agent", "--head", f"127.0.0.1:{self.R}", "--threads", "1", "--virtual", "2" if self.kind == "hier" else "4"],
                       self.R + 60 + 4 * i,
                       {"CK_NODE_ID": f"fz-{self.name}-{i}", "CK_HEADS": self.heads})
        time.sleep(6)
        if self.kind == "hier":
            lids = []
            for lab in self.labs:
                http("POST", self.R, "/labs", {"name": lab, "w": 8, "h": 8})
            labs = http("GET", self.R, "/labs").get("labs", [])
            nodes = [n["node_id"] for n in http("GET", self.R, "/nodes").get("nodes", []) if "/v" in n["node_id"]]
            for lab in labs:
                lids.append(lab["id"])
            hosts = sorted({n.split("/")[0] for n in nodes})  # one entry per agent process
            for j, n in enumerate(sorted(nodes)):
                half = 0 if hosts.index(n.split("/")[0]) < len(hosts) // 2 else 1
                http("PUT", self.R, f"/nodes/{n.replace('/', '%2F')}/place", {"lab_id": labs[half]["id"], "x": j % 8, "y": j // 8})
            http("PUT", self.R, "/hierarchy/settings", {"mode": "auto", "lab_head_min_pcs": 4, "lab_head_standby": True})
            for _ in range(200):
                time.sleep(1)
                t = http("GET", self.R, "/hierarchy")
                ok = [lb for lb in t.get("labs", []) if (lb.get("head") or {}).get("pcs", 0) >= 5 and (lb.get("standby") or {}).get("ready")]
                if len(ok) == 2:
                    break
            else:
                raise RuntimeError("hierarchy did not come up: " + json.dumps(self.tree())[:600])
        else:
            for _ in range(60):
                time.sleep(1)
                if len(http("GET", self.R, "/cluster").get("workers", [])) >= 12 and http("GET", self.R, "/ha").get("standbys", ["x"]):
                    break
            time.sleep(3)

    def tree(self) -> list[dict[str, Any]]:
        return http("GET", self.api_port(), "/hierarchy").get("labs", [])

    def api_port(self) -> int:
        """The head that is primary with the highest epoch (a resumed old head still answers, with a stale job table), else any that answers."""
        best, seen = (-1, self.R), []
        for p in (self.R, self.sp):
            ha = http("GET", p, "/ha", timeout=2)
            if "epoch" in ha:
                seen.append(p)
                if ha.get("role") == "primary" and ha["epoch"] > best[0]:
                    best = (ha["epoch"], p)
        return best[1] if best[0] >= 0 else (seen[0] if seen else self.R)

    def pid_on(self, port: int) -> int | None:
        out = subprocess.run(["lsof", "-ti", f"tcp:{port}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout.split()
        return int(out[0]) if out else None

    def pid_of(self, role: str, rng: random.Random) -> int | None:
        if role == "head":
            return self.procs["root"].pid
        if role == "standby":
            return self.procs["standby"].pid
        if role == "agent":
            return self.procs[rng.choice([k for k in self.procs if k.startswith("pc")])].pid
        lab = rng.choice(self.labs or ["A"])
        l = next((x for x in self.tree() if x["lab"] == lab), {})
        if role == "labhead":
            return self.pid_on(int((l.get("head") or {}).get("port") or 0)) if l.get("head") else None
        if role == "labstandby":
            addr = (l.get("standby") or {}).get("addr") or ""
            return self.pid_on(int(addr.rsplit(":", 1)[-1])) if ":" in addr else None
        return None

    def ours(self) -> list[tuple[int, int, str]]:
        return [r for r in ps_lines() if r[2].startswith(self.bin + " ")]

    def stop(self) -> list[str]:
        """SIGCONT then SIGKILL everything of this fleet; return the leftovers (ck processes that were still alive after the fleet was killed)."""
        for sq in self.squat:
            sq.close()
        if self in FLEETS:
            FLEETS.remove(self)
        for p in self.procs.values():
            for s in (signal.SIGCONT, signal.SIGKILL):
                try:
                    os.killpg(p.pid, s)
                except Exception:
                    pass
        for _ in range(3):  # lab heads that setsid'd away: found by their binary path
            for pid, _pp, _cmd in self.ours():
                for s in (signal.SIGCONT, signal.SIGKILL):
                    try:
                        os.kill(pid, s)
                    except Exception:
                        pass
            time.sleep(0.4)
        for p in self.procs.values():
            try:
                p.wait(timeout=5)
            except Exception:
                pass
        return [f"{pid} {cmd[:100]}" for pid, _pp, cmd in self.ours()]


# ---------------------------------------------------------------- jobs
SDK_SCRIPT = '''
import hashlib, json, os, sys, time
sys.path.insert(0, %(py)r)
import clusterkit as ck


def work(path):
    time.sleep(%(sleep)s)
    with open(path, "rb") as f:
        d = f.read()
    return [len(d), d.count(b"\\n"), hashlib.sha256(d).hexdigest()[:12]]


if __name__ == "__main__":
    ck.connect(%(url)r)
    ck.config(local="never", name=%(tag)r)
    data = %(data)r
    shards = ck.shard(data, parts=%(parts)d)
    out = ck.map(work, shards)
    print("RESULT", json.dumps([[a, b] for a, b, _ in out]), flush=True)
'''


class Job:
    """One job of a case: how it is submitted, what the exact answer is, how to read it back."""

    def __init__(self, fleet: Fleet, kind: str, mode: str, tag: str) -> None:
        self.f, self.kind, self.mode, self.tag = fleet, kind, mode, tag
        self.expect: Any = None
        self.client: subprocess.Popen[bytes] | None = None
        self.ids: list[int] = []
        self.spec: dict[str, Any] = {}
        self.out = f"{fleet.dir}/{tag}.client.out"
        if kind == "synth":
            self.spec = {"kind": "synth", "from": 1, "n": N_SYNTH, "chunk": 20_000, "synth_ns": NS_SYNTH, "name": tag}
            self.expect = N_SYNTH
        elif kind == "cmd":
            self.spec = {"kind": "cmd", "from": 0, "n": N_CMD, "chunk": CHUNK_CMD, "policy": "fixed", "name": tag, "reducer": "sum",
                         "cmd": f": fzm_{tag}; sleep {SLEEP_CMD}; echo $(( (CK_END-1+CK_START)*(CK_END-CK_START)/2 )) #"}
            self.expect = (N_CMD - 1) * N_CMD // 2
        elif kind == "list":
            self.spec = {"kind": "list", "items": [str(i) for i in range(1, N_LIST + 1)], "chunk": 1, "policy": "fixed", "name": tag, "reducer": "sum",
                         "cmd": f": fzm_{tag}; sleep {SLEEP_LIST}; awk '{{s+=$1}} END{{print s}}'"}
            self.expect = N_LIST * (N_LIST + 1) // 2

    def max_id(self) -> int:
        js = http("GET", self.f.api_port(), "/jobs?limit=5").get("jobs", [])
        return max([j["id"] for j in js] or [0])

    def submit(self) -> None:
        self.base_id = self.max_id()
        R = self.f.R
        if self.mode == "api":
            r = http("POST", R, "/jobs", self.spec, timeout=15)
            if "job" not in r:
                raise RuntimeError(f"submit failed: {r}")
            self.ids = [r["job"]["id"]]
            return
        env = dict(ENV, CK_PORT=str(R), CK_HEADS=self.f.heads, CK_FAILOVER_SECS="90")
        if self.mode == "run":
            args = [self.f.bin, "run", "--head", "127.0.0.1"]
            if self.kind == "synth":
                args += ["--synth", str(NS_SYNTH), "--n", str(N_SYNTH), "--chunk", "20000"]
            elif self.kind == "cmd":
                args += ["--cmd", self.spec["cmd"], "--n", str(N_CMD), "--chunk", str(CHUNK_CMD), "--reducer", "sum", "--policy", "fixed"]
            else:
                lf = f"{self.f.dir}/{self.tag}.items"
                open(lf, "w").write("\n".join(self.spec["items"]) + "\n")
                args += ["--cmd", self.spec["cmd"], "--list", lf, "--reducer", "sum", "--chunk", "1", "--policy", "fixed"]
            if self.kind == "cmd":
                args += ["--from", "0"]
        else:  # sdk
            data = f"{self.f.dir}/{self.tag}.data"
            rng = random.Random(7)
            with open(data, "w") as fh:
                fh.writelines(f"row {i} {rng.random():.9f} " + "x" * 400 + "\n" for i in range(SDK_PARTS * 600))
            size = os.path.getsize(data)
            self.expect = [size, SDK_PARTS * 600]
            script = f"{self.f.dir}/{self.tag}.sdk.py"
            open(script, "w").write(SDK_SCRIPT % {"py": os.path.join(REPO, "python"), "sleep": SDK_SLEEP, "url": f"http://127.0.0.1:{R + 2}",
                                                  "tag": self.tag, "data": data, "parts": SDK_PARTS})
            args = [sys.executable, "-I", script]
            env["CK_HEAD"] = f"http://127.0.0.1:{R + 2}"
        self.client = subprocess.Popen(args, env=env, stdout=open(self.out, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)

    def find_ids(self) -> list[int]:
        """Client modes: the jobs this client created (ids above the one seen before it started)."""
        if self.mode != "api":
            js = http("GET", self.f.api_port(), "/jobs?limit=20").get("jobs", [])
            self.ids = sorted({j["id"] for j in js if j["id"] > self.base_id} | set(self.ids))
        return self.ids

    def rows(self) -> list[dict[str, Any]]:
        rows = []
        for i in self.find_ids():
            r = http("GET", self.f.api_port(), f"/jobs/{i}")
            if "job" in r:
                rows.append(r["job"])
        return rows

    def exact(self, rows: list[dict[str, Any]]) -> tuple[bool, str]:
        """Compare the result with the known answer. Client modes: the client's own output for sdk, the job result otherwise."""
        if self.mode == "sdk":
            txt = open(self.out).read() if os.path.exists(self.out) else ""
            m = re.search(r"^RESULT (.*)$", txt, re.M)
            if not m:
                return False, "sdk printed no RESULT: " + txt[-200:].replace("\n", " | ")
            res = json.loads(m.group(1))
            got = [sum(a for a, _ in res), sum(b for _, b in res)]
            return got == self.expect, f"sdk sums {got} expect {self.expect}"
        main = [r for r in rows if r["state"] == "done" and r.get("total", 0) > 0] or rows
        if not main:
            return False, "no job found"
        j = main[-1]
        txt = http("GET", self.f.api_port(), f"/jobs/{j['id']}/result", timeout=30)
        txt = txt if isinstance(txt, str) else json.dumps(txt)
        nums = re.findall(r"\d+", txt)
        if self.kind == "synth":
            got = int(nums[0]) if nums else None
        else:
            got = sum(int(x) for x in nums) if len(nums) > 1 else (int(nums[0]) if nums else None)
        return got == self.expect, f"result {got} expect {self.expect}"


def settle_checks(f: Fleet, job: Job, errs: list[str]) -> None:
    """After the end: no lease, no chunk in flight on a PC, no job program still running."""
    time.sleep(6)
    port = f.api_port()
    t = http("GET", port, "/hierarchy")
    for lab in t.get("labs", []):
        h = lab.get("head") or {}
        for k in ("root_inflight", "granted_in_flight", "chunks_inflight"):
            v = h.get(k)
            if v and (len(v) if isinstance(v, (list, dict)) else v) > 0:
                errs.append(f"lab {lab['lab']} {k}={json.dumps(v)[:120]} after the job ended")
        if h.get("leases"):
            errs.append(f"lab {lab['lab']} leases left: {json.dumps(h['leases'])[:160]}")
    if t.get("orphans"):
        errs.append(f"orphan leases {json.dumps(t['orphans'])[:160]}")
    c = http("GET", port, "/cluster")
    busy = [w.get("node_id") or w.get("id") for w in c.get("workers", []) if w.get("busy")]
    if busy:
        time.sleep(6)
        c = http("GET", f.api_port(), "/cluster")
        busy = [w.get("node_id") or w.get("id") for w in c.get("workers", []) if w.get("busy")]
        if busy:
            errs.append(f"{len(busy)} PCs still busy after the job ended: {busy[:3]}")
    if c.get("job"):
        errs.append(f"cluster still shows a running job: {json.dumps(c['job'])[:120]}")
    mark = f"fzm_{job.tag}"
    left = [f"{pid} {cmd[:80]}" for pid, _pp, cmd in ps_lines() if mark in cmd and "ps -axo" not in cmd and "/ck run " not in cmd]  # the `ck run` client carries the command in its argv
    if left:
        errs.append(f"orphan job programs: {left[:3]}")


# ---------------------------------------------------------------- one case
class Case:
    def __init__(self, cid: str, fleet: str, kind: str, mode: str, faults: list[tuple[str, str, float, float]], cancel: bool = False, block_blob: bool = False) -> None:
        """faults: (role, 'pause'|'kill', seconds, delay after the previous fault or after the random first moment)."""
        self.id, self.fleet, self.kind, self.mode, self.faults, self.cancel, self.block_blob = cid, fleet, kind, mode, faults, cancel, block_blob

    def describe(self) -> str:
        fs = " + ".join(f"{r} {k}{('/' + format(d, 'g') + 's') if k == 'pause' else ''}@+{dl:g}" for r, k, d, dl in self.faults)
        return f"{self.fleet}/{self.kind}/{self.mode}: {fs or 'no fault'}{' + cancel' if self.cancel else ''}{' + blob ports taken' if self.block_blob else ''}"


def run_case(c: Case, slot: int, seed: int, base: dict[str, float]) -> dict[str, Any]:
    rng = random.Random(f"{seed}:{c.id}")
    res: dict[str, Any] = {"id": c.id, "case": c.describe(), "seed": f"{seed}:{c.id}", "errs": [], "notes": [], "verdict": "PASS"}
    f = Fleet(f"c{c.id}", c.fleet, slot, c.block_blob)
    timers: list[threading.Timer] = []
    paused: list[int] = []
    t_start = now()
    try:
        f.up()
        res["up_s"] = round(now() - t_start, 1)
        job = Job(f, c.kind, c.mode, f"c{c.id}")
        bkey = f"{c.fleet}/{c.kind}"
        b = base.get(bkey, 40.0)
        fault_s = sum(d for _r, k, d, _dl in c.faults if k == "pause")
        bound = 3 * b + fault_s + 30
        job.submit()
        t0 = now()
        t_first = rng.uniform(3.0, max(4.0, 0.7 * b))
        log: list[tuple[float, int, int]] = []  # (t, done, idle PCs)
        stalls: list[str] = []
        stop = threading.Event()

        def fire(role: str, kind: str, dur: float) -> None:
            if role in ("run", "sdk"):
                pid = job.client.pid if job.client else None
            else:
                pid = f.pid_of(role, rng)
            if not pid:
                res["notes"].append(f"{role}: no process to {kind} at {now() - t0:.0f}s")
                return
            res["notes"].append(f"{now() - t0:.1f}s {kind} {role} pid {pid}" + (f" for {dur:g}s" if kind == "pause" else ""))
            try:
                if kind == "pause":
                    os.kill(pid, signal.SIGSTOP)
                    paused.append(pid)
                    tm = threading.Timer(dur, lambda: (os.kill(pid, signal.SIGCONT), res["notes"].append(f"{now() - t0:.1f}s resumed {role}")))
                    tm.daemon = True
                    tm.start()
                    timers.append(tm)
                else:
                    os.kill(pid, signal.SIGCONT)
                    os.kill(pid, signal.SIGKILL)
                    if role in ("run", "sdk") and job.client:
                        job.client.poll()
            except ProcessLookupError:
                res["notes"].append(f"{role} pid {pid} already gone")

        t_at = t_first
        for role, kind, dur, dl in c.faults:
            t_at += dl if t_at != t_first else 0
            tm = threading.Timer(max(0.0, t_at - (now() - t0)), fire, (role, kind, dur))
            tm.daemon = True
            tm.start()
            timers.append(tm)
        cancel_at = None
        if c.cancel:
            cancel_at = t_first + rng.uniform(0.5, 6.0)
            res["notes"].append(f"cancel planned at {cancel_at:.1f}s")

        def sampler() -> None:
            last_done, last_t = -1, now()
            while not stop.is_set():
                time.sleep(1.0)
                rows = job.rows()
                done = sum(r.get("done", 0) for r in rows)
                cl = http("GET", f.api_port(), "/cluster", timeout=2)
                idle = sum(1 for w in cl.get("workers", []) if not w.get("busy"))
                log.append((round(now() - t0, 1), done, idle))
                if done != last_done:
                    if last_done >= 0 and now() - last_t >= STALL_S and rows and rows[-1]["state"] == "running":
                        stalls.append(f"no progress for {now() - last_t:.0f} s (ended at {now() - t0:.0f} s, {log[-2][2] if len(log) > 1 else 0} PCs idle)")
                    last_done, last_t = done, now()

        th = threading.Thread(target=sampler, daemon=True)
        th.start()
        cancelled = False
        rows: list[dict[str, Any]] = []
        limit, extended = bound, False
        while now() - t0 < limit:
            time.sleep(1.0)
            if now() - t0 >= limit - 1 and not extended and len(log) > 30 and log[-1][1] != log[-30][1]:
                limit, extended = bound + max(60.0, bound / 2), True  # over the bound but still moving: wait once more, then call it SLOW, not frozen
                res["notes"].append(f"{now() - t0:.0f}s over the bound but still progressing: waiting until {limit:.0f}s")
            if cancel_at is not None and not cancelled and now() - t0 >= cancel_at:
                for i in job.find_ids():
                    http("POST", f.api_port(), f"/jobs/{i}/cancel", {}, timeout=10)
                cancelled = True
                res["notes"].append(f"{now() - t0:.1f}s cancel sent")
            rows = job.rows()
            if rows and all(r["state"] in ("done", "failed", "cancelled") for r in rows):
                client_done = job.client is None or job.client.poll() is not None  # a paused client prints its result only after it wakes
                if client_done or now() - t0 > limit - 20:
                    break
        stop.set()
        th.join(timeout=3)
        el = now() - t0
        res.update(elapsed=round(el, 1), bound=round(bound, 1), baseline=round(b, 1), stalls=stalls)
        states = [r["state"] for r in rows]
        res["states"] = states
        if not rows or not all(s in ("done", "failed", "cancelled") for s in states):
            res["verdict"] = "FREEZE"
            res["errs"].append(f"job not finished after {el:.0f} s (bound {bound:.0f}); states {states}; last progress {log[-1] if log else None}")
        else:
            if el > bound:
                res["verdict"] = "SLOW"
                res["errs"].append(f"finished after the bound ({el:.0f} s > {bound:.0f} s), progress never stopped for good")
            if cancelled:
                if any(s == "failed" for s in states):
                    res["errs"].append(f"cancel ended failed: {[r.get('error') for r in rows]}")
            elif c.mode == "api" or not any(r in ("run", "sdk") for r, k, *_ in c.faults if k == "kill"):
                ok, why = job.exact(rows)
                res["exact"] = why
                if not all(s == "done" for s in states):
                    res["errs"].append(f"job {states}: {[r.get('error') for r in rows if r['state'] != 'done']}")
                elif not ok:
                    res["errs"].append("WRONG RESULT: " + why)
            elif not all(s in ("done", "cancelled", "failed") for s in states):
                res["errs"].append(f"client killed, job {states}")
            settle_checks(f, job, res["errs"])
        if job.client and job.client.poll() is None:
            if any(k == "pause" and r in ("run", "sdk") for r, k, *_ in c.faults) or res["verdict"] == "FREEZE":
                res["notes"].append("client still running at the end")
            os.killpg(job.client.pid, signal.SIGKILL)
        res["log_tail"] = log[-3:]
        slow = [s for s in stalls]
        if slow and fault_s and not res["errs"]:
            res["notes"].extend(slow)
        elif slow:
            res["errs"].extend(["STALL " + s for s in slow])
        if res["errs"] and res["verdict"] == "PASS":
            res["verdict"] = "FAIL"
    except Exception as e:
        res["verdict"] = "ERROR"
        res["errs"].append(f"harness: {type(e).__name__}: {e}")
    finally:
        for p in paused:
            try:
                os.kill(p, signal.SIGCONT)
            except Exception:
                pass
        for tm in timers:
            tm.cancel()
        left = f.stop()
        if left:
            res["errs"].append(f"processes left after teardown: {left[:3]}")
            if res["verdict"] == "PASS":
                res["verdict"] = "FAIL"
        res["wall_s"] = round(now() - t_start, 1)
    return res


# ---------------------------------------------------------------- matrix
def matrix(seed: int) -> list[Case]:
    rng = random.Random(seed)
    cases: list[Case] = []
    n = [0]

    def add(fleet: str, kind: str, faults: list[tuple[str, str, float, float]], cancel: bool = False, block_blob: bool = False) -> None:
        roles = [r for r, *_ in faults]
        mode = "sdk" if "sdk" in roles else "run" if "run" in roles else "api"
        if mode == "sdk":
            kind = "sdk"
        elif kind == "sdk" and not faults:
            mode = "sdk"
        elif kind == "sdk":
            kind, mode = rng.choice(["synth", "cmd", "list"]), "api"
        n[0] += 1
        cases.append(Case(f"{n[0]:03d}", fleet, kind, mode, faults, cancel, block_blob))

    singles = [("pause", 2.0), ("pause", 8.0), ("pause", 40.0), ("kill", 0.0)]
    kinds = ["synth", "cmd", "list", "sdk"]
    i = 0
    for fleet, roles in (("plain", ROLES_PLAIN), ("hier", ROLES_HIER)):
        for role in roles:
            for k, d in singles:
                add(fleet, kinds[i % 4], [(role, k, d, 0.0)])
                i += 1
    # the blob port of a head is taken by another program: the PCs must still get their blobs
    add("plain", "sdk", [], block_blob=True)
    add("hier", "sdk", [], block_blob=True)
    # pairs paused at once (both 8 s) and pause during a failover (kill a head-like role, then pause the next one 0.5-3 s later)
    pairs = [("head", "standby"), ("head", "agent"), ("standby", "agent"), ("agent", "run"), ("head", "run"), ("agent", "sdk")]
    for a, b in pairs:
        add("plain", kinds[i % 4], [(a, "pause", 8.0, 0.0), (b, "pause", 8.0, 0.2)])
        i += 1
    for a, b in [("head", "standby"), ("labhead", "labstandby"), ("labhead", "head"), ("head", "labhead"), ("labhead", "agent"), ("labstandby", "agent"),
                 ("head", "agent"), ("standby", "labhead")]:
        add("hier", kinds[i % 3], [(a, "pause", 8.0, 0.0), (b, "pause", 8.0, 0.2)])
        i += 1
    for fleet, roles in (("plain", ["head", "standby", "agent"]), ("hier", ["head", "labhead", "labstandby", "agent"])):
        for role in roles:
            for other in ([r for r in roles if r != role][:2] + (["run"] if fleet == "plain" else [])):
                add(fleet, kinds[i % 3], [(role, "kill", 0.0, 0.0), (other, "pause", 8.0, round(rng.uniform(0.3, 3.0), 1))])
                i += 1
    # cancel during a fault
    for fleet, roles in (("plain", ROLES_PLAIN[:3]), ("hier", ROLES_HIER[:3] + ROLES_HIER[5:])):
        for role in roles:
            for k, d in (("pause", 8.0), ("kill", 0.0), ("pause", 40.0)):
                add(fleet, kinds[i % 3], [(role, k, d, 0.0)], cancel=True)
                i += 1
    # fill to 100+ with random single and double faults
    while len(cases) < 112:
        fleet = rng.choice(["plain", "hier"])
        roles = ROLES_PLAIN if fleet == "plain" else ROLES_HIER
        fs = [(rng.choice(roles), rng.choice(["pause", "kill"]), rng.choice([2.0, 8.0, 40.0]), 0.0)]
        if rng.random() < 0.4:
            fs.append((rng.choice(roles), rng.choice(["pause", "kill"]), rng.choice([2.0, 8.0]), round(rng.uniform(0.2, 5.0), 1)))
        add(fleet, rng.choice(kinds), [(r, k, d if k == "pause" else 0.0, dl) for r, k, d, dl in fs], cancel=rng.random() < 0.15)
    return cases


def baseline(force: str | None = None) -> dict[str, float]:
    out: dict[str, float] = {}
    path = f"{WORK}/baseline.json"
    todo = [(fl, k) for fl in ("plain", "hier") for k in KINDS]
    try:
        out.update(json.load(open(path)))  # keep what an earlier run measured
    except Exception:
        pass
    todo = [t for t in todo if f"{t[0]}/{t[1]}" not in out or f"{t[0]}/{t[1]}" == force]
    lock = threading.Lock()
    slots = [0, 1, 2, 3]

    def one(fl: str, k: str, slot: int) -> None:
        f = Fleet(f"base-{fl}-{k}", fl, slot)
        try:
            f.up()
            job = Job(f, k, "sdk" if k == "sdk" else "api", f"b{fl}{k}")
            job.submit()
            t0 = now()
            while now() - t0 < 600:
                time.sleep(1)
                rows = job.rows()
                if rows and all(r["state"] in ("done", "failed", "cancelled") for r in rows):
                    break
            ok, why = job.exact(job.rows())
            with lock:
                out[f"{fl}/{k}"] = round(now() - t0, 1)
                json.dump(out, open(path, "w"), indent=1)
            print(f"baseline {fl}/{k}: {now() - t0:.1f} s exact={ok} ({why})", flush=True)
        except Exception as e:
            print(f"baseline {fl}/{k}: FAILED {type(e).__name__}: {e}", flush=True)
        finally:
            f.stop()

    for i in range(0, len(todo), 2):  # hier fleets are 12 processes each: two at a time
        ths = [threading.Thread(target=one, args=(fl, k, s)) for (fl, k), s in zip(todo[i:i + 2], slots)]
        [t.start() for t in ths]
        [t.join() for t in ths]
    json.dump(out, open(path, "w"), indent=1)
    return out


def load_base() -> dict[str, float]:
    try:
        return json.load(open(f"{WORK}/baseline.json"))
    except Exception:
        return {}


def run_all(a: argparse.Namespace) -> None:
    keys = a.only.split(",") if a.only else []  # ids or text found in the description, comma separated
    cases = [c for c in matrix(a.seed) if not keys or any(k == c.id or k in c.describe() for k in keys)]
    done_ids = set()
    rp = f"{WORK}/results.jsonl"
    if os.path.exists(rp) and not a.again:
        done_ids = {json.loads(ln)["id"] for ln in open(rp) if ln.strip()}
    cases = [c for c in cases if c.id not in done_ids][a.skip:][: a.limit or None]
    base = load_base()
    print(f"seed {a.seed}; {len(cases)} cases; {a.workers} workers; baseline {base}", flush=True)
    q = list(cases)
    lock = threading.Lock()
    free_slots = [s for s in range(a.slot0, a.slot0 + 10) if all(port_free(BASE + 100 * s + d) for d in range(0, 100, 1) if d in (0, 1, 2, 3, 10, 13, 40, 41, 42, 43, 50, 51, 52, 53, 60, 61, 62, 63))][: a.workers]

    def worker(slot: int) -> None:
        while True:
            with lock:
                if not q:
                    return
                c = q.pop(0)
            try:
                r = run_case(c, slot, a.seed, base)
            except Exception as e:  # e.g. the ports of the slot are taken: report it, keep the worker alive
                r = {"id": c.id, "case": c.describe(), "verdict": "ERROR", "errs": [f"harness: {e}"], "notes": []}
                time.sleep(5)
            with lock:
                open(rp, "a").write(json.dumps(r) + "\n")
                print(f"[{r['verdict']:6}] {c.id} {c.describe()}  {r.get('elapsed', '-')}s/{r.get('bound', '-')}s " + "; ".join(r["errs"])[:300], flush=True)

    ths = [threading.Thread(target=worker, args=(s,)) for s in free_slots]
    [t.start() for t in ths]
    [t.join() for t in ths]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("what", choices=["list", "baseline", "run", "case", "up"])
    p.add_argument("arg", nargs="?")
    p.add_argument("--workers", type=int, default=3)
    p.add_argument("--only", default=None)
    p.add_argument("--limit", type=int, default=0)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--slot0", type=int, default=0)
    p.add_argument("--again", action="store_true")
    p.add_argument("--skip", type=int, default=0, help="skip the first N cases (run the rest of the matrix with another --seed)")
    a = p.parse_args()
    signal.signal(signal.SIGTERM, die)  # stop every fleet, then exit
    signal.signal(signal.SIGINT, die)
    print(f"work dir {WORK}  seed {a.seed}", flush=True)
    os.makedirs(WORK, exist_ok=True)
    if a.what == "list":
        for c in matrix(a.seed):
            print(c.id, c.describe())
    elif a.what == "baseline":
        print(baseline(a.arg))
    elif a.what == "run":
        run_all(a)
    elif a.what == "case":
        a.only, a.limit = a.arg, 1
        a.workers = 1
        a.again = True
        run_all(a)
    else:
        f = Fleet("dbg-" + (a.arg or "plain"), a.arg or "plain", a.slot0)
        try:
            f.up()
            print("up; ports", f.R, "Ctrl-C to stop", flush=True)
            time.sleep(3600)
        finally:
            f.stop()


if __name__ == "__main__":
    main()
