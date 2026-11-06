#!/usr/bin/env python3
"""Round 4 (rt1): count frames and HTTP requests per flow, on 1 head + 3 agent processes x 4 virtual PCs, and on root + 2 lab heads.

  CK_BIN=... BASE_PORT=14000 HB_DIR=... python3 -I scripts/stress/trips.py flat|hier|sdk|all [--json OUT]

Reads GET /api/v1/wire (frames and bytes per kind, HTTP requests per route) from each head before and after each flow.
Prints one `TRIPS <flow> key=value ...` line per flow and checks every result is exact. Exits 1 on a wrong result.
"""
from __future__ import annotations

import argparse, json, os, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hier_bench as hb  # noqa: E402

OUT: dict[str, dict] = {}
BAD: list[str] = []


def wire(f: hb.Fleet, port: int | None = None) -> dict:
    return f.api("GET", "/wire", port=port)


def reset(f: hb.Fleet, ports: list[int]) -> None:
    for p in ports:
        f.api("POST", "/wire/reset", {}, port=p)


def tally(f: hb.Fleet, ports: list[int]) -> dict:
    t: dict = {"out": {}, "in": {}, "http": {}}
    for p in ports:
        w = wire(f, p)
        for side in ("out", "in", "http"):
            for k, v in (w.get(side) or {}).items():
                t[side][k] = t[side].get(k, 0) + v
    return t


def summarize(name: str, t: dict, wall: float, ok: bool, extra: dict | None = None) -> None:
    quiet = {"HEARTBEAT", "HELLO_W", "CHALLENGE", "PROOF", "KEY"}
    frames = {k: v for k, v in t["out"].items() if k not in quiet}
    http = sum(v for k, v in t["http"].items() if "/wire" not in k and "/stream" not in k)
    rec = {"wall_s": round(wall, 3), "ok": ok, "frames_out": frames, "heartbeats": t["out"].get("HEARTBEAT", 0), "frames_in": {k: v for k, v in t["in"].items() if k not in quiet and k != "HEARTBEAT"}, "http": http, "http_routes": t["http"], **(extra or {})}
    OUT[name] = rec
    print(f"TRIPS {name} wall={wall:.2f}s ok={ok} frames_out={sum(frames.values())} {json.dumps(frames, sort_keys=True)} in={sum(rec['frames_in'].values())} http={http}", flush=True)
    if not ok:
        BAD.append(name)


def synth(f: hb.Fleet, ports: list[int], name: str, n: int, chunk: int, ns: int) -> None:
    reset(f, ports)
    out, wall = f.run("--synth", str(ns), "--n", str(n), "--chunk", str(chunk))
    t = tally(f, ports)
    got = hb.counted(out)
    chunks = -(-n // chunk)
    t_ = {"chunks": chunks}
    summarize(name, t, wall, got == n, t_)
    if chunks:
        print("      per chunk: " + " ".join(f"{k}={v / chunks:.2f}" for k, v in sorted(t["out"].items()) if k not in ("HEARTBEAT", "CTRL")) + f" ctrl={t['out'].get('CTRL', 0)}", flush=True)


def flat(base: int) -> hb.Fleet:
    return hb.Fleet("trips-flat", False, base, labs=3, pcs=4, threads=1)


def run_flat(base: int) -> None:
    f = flat(base)
    try:
        # (a)+(b): 480 chunks of 1 ms work each, and 24 chunks of ~125 ms
        synth(f, [base], "a_small_chunks", 48_000_000, 100_000, 400)
        synth(f, [base], "a_big_chunks", 120_000_000, 1_000_000, 100)
        # (b) tail / latency: one chunk only
        synth(f, [base], "b_one_chunk", 1_000, 1_000, 10)
        sdk(f, base)
    finally:
        f.stop()


def sdk(f: hb.Fleet, base: int) -> None:
    """(e) and (d): SDK map with small items, then with a 3 MB shared argument (blobs)."""
    code = f"""
import sys, json, time, os
sys.path.insert(0, {os.path.join(hb.HERE, '..', '..', 'python')!r}); sys.path.insert(0, {os.path.join(hb.HERE, '..', '..', 'python', 'tests')!r})
import urllib.request, clusterkit as ck, sdkfns
def w(): return json.load(urllib.request.urlopen('http://127.0.0.1:{base + 2}/api/v1/wire'))
def rs(): urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:{base + 2}/api/v1/wire/reset', method='POST', data=b'{{}}'))
ck.connect('http://127.0.0.1:{base + 2}')
ck.config(local='never', history=False)
res = {{}}
for name, items in (('e_sdk_map', list(range(400))), ('e_sdk_map_cached_profile', list(range(400)))):
    rs(); t = time.time()
    out = ck.map(sdkfns.slow, items)
    wall = time.time() - t
    ok = out == [sdkfns.slow(x) for x in items]
    res[name] = dict(wall=wall, ok=ok, wire=w())
import bigfns
ck.config(code_blob_kb=0, blob_auto_kb=64)
for name in ('d_blob_first', 'd_blob_again'):
    rs(); t = time.time()
    out = ck.map(bigfns.lookup, list(range(300)), local='never', batch=False)
    res[name] = dict(wall=time.time() - t, ok=out == [i + 1 for i in range(300)], wire=w())
print('RES' + json.dumps(res))
"""
    p = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=300, env=dict(os.environ, CK_TOKEN=""))
    line = [l for l in p.stdout.splitlines() if l.startswith("RES")]
    if not line:
        print("sdk run failed:", p.stdout[-500:], p.stderr[-1500:], flush=True)
        BAD.append("sdk")
        return
    for name, r in json.loads(line[0][3:]).items():
        w = r["wire"]
        summarize(name, w, r["wall"], r["ok"])


def run_hier(base: int) -> None:
    f = hb.Fleet("trips-hier", True, base, labs=2, pcs=4, threads=1)
    ports = [base, f.lab_port(0), f.lab_port(1)]
    try:
        synth(f, ports, "c_hier_small", 48_000_000, 100_000, 400)
        synth(f, ports, "c_hier_big", 120_000_000, 1_000_000, 100)
    finally:
        f.stop()


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("what", choices=["flat", "hier", "sdk", "all"])
    ap.add_argument("--json")
    a = ap.parse_args()
    base = hb.BASE
    if a.what in ("flat", "all"):
        run_flat(base)
    if a.what in ("hier", "all"):
        run_hier(base + 100)
    if a.json:
        json.dump(OUT, open(a.json, "w"), indent=1)
    print("FAILED: " + ", ".join(BAD) if BAD else "ALL EXACT")
    sys.exit(1 if BAD else 0)
