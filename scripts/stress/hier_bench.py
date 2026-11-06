#!/usr/bin/env python3
"""Lab head (two-level hierarchy) experiments on one machine: docs/HIERARCHY.md section 6.

  hier_bench.py scale    [--pcs 600] [--labs 6] [--chunk 100] [--ns 1000000] [--n 12e6]   E2: root CPU, tasks/s, wall, exact check; hier off vs on
  hier_bench.py labkill  kill -9 a lab head mid-job (extra time, exact)
  hier_bench.py rootstop SIGSTOP the root for --outage s, hier off vs on side by side
  hier_bench.py rootkill kill -9 the root with a standby; labs follow it (exact)
  hier_bench.py switch   hier_mode off -> auto -> off in the middle of a job (exact)
  hier_bench.py auto     auto mode picks lab heads and moves PCs; then pin / drain / unpin
  hier_bench.py standby  kill -9 a lab head with lab_head_standby off vs on (takeover time, extra job time, exact)

Env: CK_BIN (default rust/target/release/ck), BASE_PORT (default 10001; each process uses 4 ports), HB_DIR (work dir).
Every process runs with CK_FLEET=open CK_AUTH=off CK_PRESENCE=off CK_POWER_DRY=1 and its own CK_DATA. Exits 1 on a failed check.
"""
from __future__ import annotations

import argparse, json, os, re, resource, shutil, signal, subprocess, sys, tempfile, threading, time, urllib.request
from typing import Any

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.environ.get("CK_BIN") or os.path.join(HERE, "..", "..", "rust", "target", "release", "ck")
BASE = int(os.environ.get("BASE_PORT", "10001"))
WORK = os.environ.get("HB_DIR") or tempfile.mkdtemp(prefix="hier-bench-")
ENV = dict(os.environ, CK_POWER_DRY="1", CK_PRESENCE="off", CK_FLEET="open", CK_AUTH="off")
FAILS = []
try:
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (min(hard, 65536) if hard > 0 else soft, hard))
except Exception:
    pass


def check(name: str, ok: bool, detail: str = "") -> None:
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
    if not ok:
        FAILS.append(name)


class Fleet:
    def __init__(self, name: str, hier: bool, base: int, labs: int = 3, pcs: int = 40, threads: int = 1, standby: bool = False, tune: dict[str, str] | None = None, lab_parent_extra: str = "") -> None:
        self.dir = f"{WORK}/{name}"
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(self.dir)
        self.procs, self.R, self.labs, self.pcs, self.hier = {}, base, labs, pcs, hier
        env = dict(tune or {})  # hier_mode goes in through the API (the database), so a standby inherits it
        self.start("root", ["head", "--no-local-worker"], base, env)
        time.sleep(1)
        if hier:
            self.api("PUT", "/hierarchy/settings", {"mode": "manual"})
        if standby:
            self.start("standby", ["head", "--standby", "--head", "127.0.0.1", "--no-local-worker"], base, {}, data="standby")
        names = "ABCDEFGHIJKLMNOP"[:labs]
        if hier:
            for i, k in enumerate(names):
                self.start("lab" + k, ["head", "--parent", f"127.0.0.1:{base}" + lab_parent_extra, "--lab", k, "--no-local-worker"], self.lab_port(i))
            time.sleep(1.5)
        for i, k in enumerate(names):
            head = self.lab_port(i) if hier else base
            self.start("pc" + k, ["agent", "--head", f"127.0.0.1:{head}", "--threads", str(threads)] + (["--virtual", str(pcs)] if pcs > 1 else []), head,
                       {"CK_NODE_ID": f"agent-{name}-{k}", "CK_HEADS": f"127.0.0.1:{base}"})
        time.sleep(4 + labs * pcs / 100)

    def lab_port(self, i: int) -> int:
        return self.R + 4 * (i + 1)

    def start(self, key: str, args: list[str], port: int, env: dict[str, str] | None = None, data: str | None = None) -> None:
        e = dict(ENV, CK_PORT=str(port), CK_DATA=f"{self.dir}/data-{data or key}", **(env or {}))
        self.procs[key] = subprocess.Popen([BIN] + args, env=e, stdout=open(f"{self.dir}/{key}.log", "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)

    def sig(self, key: str, s: int) -> None:
        os.kill(self.procs[key].pid, s)

    def cpu(self, key: str = "root") -> float:
        out = subprocess.run(["ps", "-o", "cputime=", "-p", str(self.procs[key].pid)], capture_output=True, text=True).stdout.strip()
        try:
            parts = out.replace("-", ":").split(":")
            sec = 0.0
            for p in parts:
                sec = sec * 60 + float(p)
            return sec
        except Exception:
            return 0.0

    def api(self, method: str, path: str, body: dict[str, Any] | None = None, port: int | None = None) -> Any:
        port = port or self.R
        r = urllib.request.Request(f"http://127.0.0.1:{port + 2}/api/v1{path}", method=method, data=json.dumps(body).encode() if body is not None else None,
                                   headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(r, timeout=10) as x:
                return json.loads(x.read() or b"null")
        except urllib.error.HTTPError as e:
            return {"http": e.code, "body": e.read().decode()[:200]}
        except Exception as e:
            return {"error": str(e)}

    def run(self, *args: str, timeout: int = 900) -> tuple[str, float]:
        t = time.time()
        r = subprocess.run([BIN, "run", "--head", "127.0.0.1"] + list(args), env=dict(ENV, CK_PORT=str(self.R)), capture_output=True, text=True, timeout=timeout)
        return r.stdout + r.stderr, time.time() - t

    def job_async(self, *args: str) -> tuple[threading.Thread, dict[str, Any]]:
        box: dict[str, Any] = {}

        def go() -> None:
            box["out"], box["t"] = self.run(*args)

        th = threading.Thread(target=go, daemon=True)
        th.start()
        return th, box

    def stop(self) -> None:
        import glob
        for pf in glob.glob(f"{self.dir}/data-*/labhead-*/labhead.pid"):  # lab heads the root had agents start
            try:
                os.kill(int(open(pf).read().strip()), signal.SIGKILL)
            except Exception:
                pass
        for p in self.procs.values():
            for s in (signal.SIGCONT, signal.SIGKILL):
                try:
                    os.kill(p.pid, s)
                except Exception:
                    pass
        for p in self.procs.values():
            try:
                p.wait(timeout=5)
            except Exception:
                pass


def counted(out: str) -> int | None:
    m = re.search(r"(\d+) numbers counted", out)
    return int(m.group(1)) if m else None


def tree(f: Fleet) -> list[tuple[Any, ...]]:
    return [(l["lab"], l["state"], (l["head"] or {}).get("pcs"), l["pcs_at_root"]) for l in f.api("GET", "/hierarchy").get("labs", [])]


def scale(a: argparse.Namespace) -> None:
    n, chunk, ns = int(a.n), int(a.chunk), int(a.ns)
    per = a.pcs // a.labs
    print(f"E2: {a.pcs} virtual PCs ({a.labs} labs x {per}), synth {ns} ns/number, N={n}, chunk={chunk} ({chunk * ns / 1e9:.3f} s of work), asks {a.pcs * 1e9 / (chunk * ns):.0f} tasks/s", flush=True)
    rows = {}
    for hier in (False, True):
        f = Fleet("scale-" + ("on" if hier else "off"), hier, BASE, a.labs, per, 1)
        try:
            if hier:
                print("  tree:", tree(f))
            c0, t0 = f.cpu(), time.time()
            out, t = f.run("--synth", str(ns), "--n", str(n), "--chunk", str(chunk))
            cpu = (f.cpu() - c0) / (time.time() - t0) * 100
            ok = counted(out) == n
            tasks = n / chunk / t
            rows[hier] = (cpu, tasks, t)
            check(f"hier {'on ' if hier else 'off'} exact", ok, f"counted {counted(out)}")
            print(f"  hier {'on ' if hier else 'off'}: wall {t:.1f} s  root CPU {cpu:.1f} %  tasks/s {tasks:.0f}", flush=True)
        finally:
            f.stop()
    (c0, k0, t0), (c1, k1, t1) = rows[False], rows[True]
    print(f"RESULT {a.pcs} PCs chunk {chunk}: root CPU {c0:.1f} % -> {c1:.1f} %, tasks/s {k0:.0f} -> {k1:.0f}, wall {t0:.1f} -> {t1:.1f} s")


def labkill(a: argparse.Namespace) -> None:
    f = Fleet("labkill", True, BASE)
    try:
        args = ["--synth", "20000", "--n", str(int(a.n))]
        out, base = f.run(*args)
        check("no-fault exact", counted(out) == int(a.n))
        th, box = f.job_async(*args)
        time.sleep(8)
        f.sig("labB", signal.SIGKILL)
        th.join()
        print("  tree after:", tree(f))
        check("lab head kill: exact", counted(box["out"]) == int(a.n))
        print(f"RESULT lab head kill -9 at 8 s: {box['t']:.1f} s vs {base:.1f} s no fault, extra {box['t'] - base:.1f} s")
    finally:
        f.stop()


def rootstop(a: argparse.Namespace) -> None:
    res: dict[str, Any] = {}

    def one(tag: str, hier: bool, stop: bool, base: int) -> None:
        f = Fleet("rs-" + tag, hier, base)
        try:
            th, box = f.job_async("--synth", "20000", "--n", str(int(a.n)))
            if stop:
                time.sleep(a.at)
                f.sig("root", signal.SIGSTOP)
                time.sleep(a.outage)
                f.sig("root", signal.SIGCONT)
            th.join()
            res[tag] = (counted(box["out"]), box["t"])
        finally:
            f.stop()

    plan = [("off", False, False, BASE), ("off_out", False, True, BASE + 17), ("on", True, False, BASE + 34), ("on_out", True, True, BASE + 59)]
    ts = [threading.Thread(target=one, args=p) for p in plan]
    [t.start() for t in ts]
    [t.join() for t in ts]
    for k, v in res.items():
        check(f"{k} exact", v[0] == int(a.n), f"{v[1]:.1f} s")
    for h in ("off", "on"):
        print(f"RESULT root SIGSTOP {a.outage:.0f} s, hier {h}: extra {res[h + '_out'][1] - res[h][1]:.1f} s")


def rootkill(a: argparse.Namespace) -> None:
    f = Fleet("rootkill", True, BASE, standby=True, lab_parent_extra=f",127.0.0.1:{BASE}")
    try:
        args = ["--synth", "20000", "--n", str(int(a.n))]
        th, box = f.job_async(*args)
        time.sleep(a.at)
        f.sig("root", signal.SIGKILL)
        th.join()
        print("  tree after:", tree(f))
        check("root kill with standby: exact", counted(box["out"]) == int(a.n), f"{box['t']:.1f} s, ideal ~{a.n * 20000 / 1e9 / (3 * 40):.0f} s")
        print(f"RESULT root kill -9 at {a.at:.0f} s with a standby: job finished in {box['t']:.1f} s")
    finally:
        f.stop()


def switch(a: argparse.Namespace) -> None:
    """hier_mode lives in the database (no env override): start manual by API, then walk off -> auto -> off during one job."""
    f = Fleet("switch", False, BASE, labs=3, pcs=40)  # PCs on the root, no lab heads yet
    try:
        for i, k in enumerate("ABC"):
            f.start("lab" + k, ["head", "--parent", f"127.0.0.1:{BASE}", "--lab", k, "--no-local-worker"], f.lab_port(i))
        time.sleep(2)
        # seat the PCs in labs so the root knows who belongs where
        for i, k in enumerate("ABC"):
            f.api("POST", "/labs", {"name": k, "w": 8, "h": 8})
        labs = {l["name"]: l["id"] for l in f.api("GET", "/labs").get("labs", [])}
        nodes = [n["node_id"] for n in f.api("GET", "/nodes")["nodes"] if "/v" in n["node_id"]]
        by_proc = {}
        for n in nodes:
            by_proc.setdefault(n.split("/")[0], []).append(n)
        for (base, ids), k in zip(sorted(by_proc.items()), "ABC"):
            for j, n in enumerate(ids):
                f.api("PUT", f"/nodes/{n.replace('/', '%2F')}/place", {"lab_id": labs[k], "x": j % 8, "y": j // 8})
        args = ["--synth", "20000", "--n", str(int(a.n))]
        th, box = f.job_async(*args)
        steps = [(5, "manual"), (20, "off"), (35, "manual"), (50, "off")]
        t0 = time.time()
        for at, mode in steps:
            time.sleep(max(0, at - (time.time() - t0)))
            f.api("PUT", "/hierarchy/settings", {"mode": mode})
            time.sleep(3)
            print(f"  t={time.time() - t0:.0f}s mode {mode}:", tree(f), flush=True)
        th.join()
        check("live off -> manual -> off -> manual -> off: exact", counted(box["out"]) == int(a.n), f"{box['t']:.1f} s")
    finally:
        f.stop()


def auto(a: argparse.Namespace) -> None:
    # lab A: 3 real agent processes (can host a lab head) + virtual PCs; lab head port base far from the fleet
    f = Fleet("auto", False, BASE, labs=1, pcs=1)
    f.stop()
    f = Fleet.__new__(Fleet)
    f.dir = f"{WORK}/auto"
    shutil.rmtree(f.dir, ignore_errors=True)
    os.makedirs(f.dir)
    f.procs, f.R = {}, BASE
    f.start("root", ["head", "--no-local-worker"], BASE, {"CK_HIER_ALLOW_VIRTUAL": "1", "CK_TUNE_LAB_HEAD_PORT": str(BASE + 40), "CK_TUNE_LAB_HEAD_MIN_PCS": "4"})
    time.sleep(1)
    try:
        for k in "xyz":
            f.start("pc" + k, ["agent", "--head", f"127.0.0.1:{BASE}", "--threads", "1", "--virtual", "6"], BASE + 4 * ("xyz".index(k) + 1), {"CK_NODE_ID": f"auto-{k}", "CK_HEADS": f"127.0.0.1:{BASE}"})
        time.sleep(6)
        f.api("POST", "/labs", {"name": "A", "w": 8, "h": 8})
        lid = f.api("GET", "/labs")["labs"][0]["id"]
        nodes = [n["node_id"] for n in f.api("GET", "/nodes")["nodes"]]
        for j, n in enumerate(nodes):
            f.api("PUT", f"/nodes/{n.replace('/', '%2F')}/place", {"lab_id": lid, "x": j % 8, "y": j // 8})
        print(f"  {len(nodes)} PCs seated in lab A; hier_mode auto")
        f.api("PUT", "/hierarchy/settings", {"mode": "auto", "lab_head_min_pcs": 4})
        ok = None
        for _ in range(60):
            time.sleep(1)
            t = f.api("GET", "/hierarchy")
            lab = next((l for l in t.get("labs", []) if l["lab"] == "A"), None)
            if lab and lab["head"] and lab["head"]["pcs"] >= len(nodes) - 1:
                ok = lab
                break
        check("auto: a lab head was started and the lab's PCs moved under it", ok is not None, str(tree(f)))
        out, t = f.run("--synth", "10000", "--n", "2000000")
        check("job through the auto lab head is exact", counted(out) == 2000000, f"{t:.1f} s")
        n = f.api("GET", "/nodes")["nodes"]
        check("PCs under the lab head are in /nodes with via_lab", sum(1 for x in n if x.get("via_lab") == "A" and x["online"]) >= len(nodes) - 1)
        # drain: held leases finish, PCs go back to the root
        f.api("POST", "/hierarchy/labs/A/drain", {"on": True})
        time.sleep(6)
        t = f.api("GET", "/hierarchy")
        lab = next(l for l in t["labs"] if l["lab"] == "A")
        check("drain: PCs back at the root", lab["pcs_at_root"] >= len(nodes) - 1, str(tree(f)))
        out, _ = f.run("--synth", "10000", "--n", "1000000")
        check("job while drained is exact", counted(out) == 1000000)
        f.api("POST", "/hierarchy/labs/A/drain", {"on": False})
        time.sleep(8)
        check("undrain: PCs move under the lab head again", (next(l for l in f.api("GET", "/hierarchy")["labs"] if l["lab"] == "A")["head"] or {}).get("pcs", 0) >= len(nodes) - 1, str(tree(f)))
        # pin to another PC: the old lab head leaves, a new one starts there
        old = next(l for l in f.api("GET", "/hierarchy")["labs"] if l["lab"] == "A")["head"]["node_id"].split("/")[0]
        other = next(n for n in nodes if n.split("/")[0] != old)
        print("  pin lab A to", other, f.api("POST", "/hierarchy/labs/A/head", {"node": other}))
        moved = None
        for _ in range(60):
            time.sleep(1)
            h = next(l for l in f.api("GET", "/hierarchy")["labs"] if l["lab"] == "A")["head"]
            if h and h["node_id"].split("/")[0] != old and h["pcs"] >= len(nodes) - 1:
                moved = h
                break
        check("pin: the lab head moved to the pinned PC", moved is not None, str(tree(f)))
        print("  unpin:", f.api("DELETE", "/hierarchy/labs/A/head"))
        out, _ = f.run("--synth", "10000", "--n", "1000000")
        check("job after the move is exact", counted(out) == 1000000)
    finally:
        f.stop()


def standby_case(a: argparse.Namespace, on: bool, base: int, freeze: bool = False) -> dict[str, Any]:
    """Auto mode, 3 agent processes x 6 virtual PCs in lab A. kill -9 the lab head mid-job; with standby on a second lab head waits on another process.
    freeze: SIGSTOP the lab head instead, let the standby take over, then SIGCONT it: the old lab head must leave (one lab head per lab)."""
    fnv = 0xcbf29ce484222325
    for ch in b"A":
        fnv = ((fnv ^ ch) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    f = Fleet.__new__(Fleet)
    f.dir = f"{WORK}/standby-{'on' if on else 'off'}{'-freeze' if freeze else ''}"
    shutil.rmtree(f.dir, ignore_errors=True)
    os.makedirs(f.dir)
    f.procs, f.R = {}, base
    f.start("root", ["head", "--no-local-worker"], base, {"CK_HIER_ALLOW_VIRTUAL": "1", "CK_TUNE_LAB_HEAD_PORT": str(base + 40 - 10 * (fnv % 50)), "CK_TUNE_LAB_HEAD_MIN_PCS": "4"})
    time.sleep(1)
    res: dict[str, Any] = {}
    try:
        for i, k in enumerate("xyz"):
            f.start("pc" + k, ["agent", "--head", f"127.0.0.1:{base}", "--threads", "1", "--virtual", "6"], base + 4 * (i + 1), {"CK_NODE_ID": f"sb-{k}", "CK_HEADS": f"127.0.0.1:{base}"})
        time.sleep(6)
        f.api("POST", "/labs", {"name": "A", "w": 8, "h": 8})
        lid = f.api("GET", "/labs")["labs"][0]["id"]
        nodes = [n["node_id"] for n in f.api("GET", "/nodes")["nodes"]]
        for j, n in enumerate(nodes):
            f.api("PUT", f"/nodes/{n.replace('/', '%2F')}/place", {"lab_id": lid, "x": j % 8, "y": j // 8})
        f.api("PUT", "/hierarchy/settings", {"mode": "auto", "lab_head_min_pcs": 4, "lab_head_standby": on})
        lab: dict[str, Any] = {}
        for _ in range(90):
            time.sleep(1)
            lab = next((l for l in f.api("GET", "/hierarchy").get("labs", []) if l["lab"] == "A"), {})
            if lab.get("head") and lab["head"]["pcs"] >= len(nodes) - 1 and (not on or (lab.get("standby") or {}).get("ready")):
                break
        check(f"standby {'on' if on else 'off'}: lab head up with all PCs" + (" and a standby ready" if on else ""), bool(lab.get("head") and (not on or (lab.get("standby") or {}).get("ready"))), str(tree(f)))
        print("  standby:", lab.get("standby"))
        old = lab["head"]["node_id"].split("/")[0]
        pid = int(subprocess.run(["lsof", "-ti", f"tcp:{lab['head']['port']}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout.split()[0])  # the lab head that listens on its port
        args = ["--synth", "20000", "--n", str(int(a.n))]
        out, base_t = f.run(*args)
        check("no-fault exact", counted(out) == int(a.n), f"{base_t:.1f} s")
        th, box = f.job_async(*args)
        time.sleep(a.at)
        t0 = time.time()
        os.kill(pid, signal.SIGSTOP if freeze else signal.SIGKILL)
        seen: dict[str, float] = {}
        while th.is_alive() and len(seen) < 2 and time.time() - t0 < 60:
            time.sleep(0.05)
            l = next((x for x in f.api("GET", "/hierarchy").get("labs", []) if x["lab"] == "A"), None)
            if not l:
                continue
            hd = l["head"] or {}
            under_new = hd.get("pcs", 0) if hd.get("node_id", "").split("/")[0] != old else 0
            if "attached" not in seen and under_new + l["pcs_at_root"] >= len(nodes) - 1:
                seen["attached"] = time.time() - t0
            if "under_lab_head" not in seen and under_new >= len(nodes) - 1:
                seen["under_lab_head"] = time.time() - t0
        if freeze:
            os.kill(pid, signal.SIGCONT)  # the lab head runs inside the agent process: it gives up the role, the process stays
            heads = []
            for _ in range(20):
                time.sleep(0.5)
                lab_now = next((x for x in f.api("GET", "/hierarchy").get("labs", []) if x["lab"] == "A"), {})
                heads.append(((lab_now.get("head") or {}).get("node_id", "").split("/")[0], (lab_now.get("head") or {}).get("pcs", 0)))
            check("frozen lab head comes back and does not take the lab back", all(h[0] and h[0] != old for h in heads[4:]), f"head after SIGCONT: {heads[::4]}; standby took over after {seen.get('under_lab_head')}")
        th.join(timeout=3 * base_t + 60)
        if th.is_alive():
            check("lab head " + ("frozen" if freeze else "kill -9") + ": job finished in time", False, json.dumps(f.api("GET", "/hierarchy"))[:3000])
            return res
        check("lab head " + ("frozen" if freeze else "kill -9") + ": exact", counted(box["out"]) == int(a.n))
        res = dict(seen, extra=box["t"] - base_t, wall=box["t"], base=base_t)
        print(f"RESULT standby {'on ' if on else 'off'}: kill -9 lab head at {a.at:.0f} s: PCs served again after {seen.get('attached', float('nan')):.2f} s, back under a lab head after {seen.get('under_lab_head', float('nan')):.2f} s, job {box['t']:.1f} s vs {base_t:.1f} s no fault, extra {box['t'] - base_t:.1f} s", flush=True)
    finally:
        f.stop()
    return res


def standby(a: argparse.Namespace) -> None:
    for i, on in enumerate((False, True)):
        standby_case(a, on, BASE + 100 * i)


def comeback(a: argparse.Namespace) -> None:
    standby_case(a, True, BASE + 200, freeze=True)


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("what", choices=["scale", "labkill", "rootstop", "rootkill", "switch", "auto", "standby", "comeback"])
    p.add_argument("--pcs", type=int, default=600)
    p.add_argument("--labs", type=int, default=6)
    p.add_argument("--chunk", type=float, default=100)
    p.add_argument("--ns", type=float, default=1_000_000)
    p.add_argument("--n", type=float, default=None)
    p.add_argument("--outage", type=float, default=30)
    p.add_argument("--at", type=float, default=None)
    a = p.parse_args()
    if a.n is None:
        a.n = 12e6 if a.what == "scale" else 20e6 if a.what in ("standby", "comeback") else 60e6
    if a.at is None:
        a.at = 20 if a.what == "rootstop" else 8
    {"scale": scale, "labkill": labkill, "rootstop": rootstop, "rootkill": rootkill, "switch": switch, "auto": auto, "standby": standby, "comeback": comeback}[a.what](a)
    print("FAILED: " + ", ".join(FAILS) if FAILS else "ALL PASSED")
    sys.exit(1 if FAILS else 0)
