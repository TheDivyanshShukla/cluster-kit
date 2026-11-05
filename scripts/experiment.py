#!/usr/bin/env python3
"""EXP-01..03, EXP-05: run an experiment matrix unattended against a head and write CSV for docs/paper/figs.py.

Matrix = PC counts x policies x chunk sizes, each run `--repeats` times, interleaved (repeat r runs every config once,
starting at a different config each round), after one warm-up run per config that is not recorded.

  python3 scripts/experiment.py --head http://10.0.0.5:7702 --pcs 1,2,4,8,16,32,64,128,200 --repeats 7 \
      --n-per-pc 2e9 --policy adaptive,fixed --out exp-e1
  env CK_TOKEN: an API token with the researcher role, unless the head runs with CK_AUTH=off.
  --upload [NAME]: at the end also store the two files on the head (Experiments page, `ck admin experiment ls`).

Out: raw.csv (config,repeat,n,seconds,hashes_per_s,hits + pcs,policy,chunk,virtual: the paper's columns first),
runs.jsonl (provenance per run: git commit, head, the exact PCs and their classes, times), summary on stdout.
PCs are chosen in a fixed order (node id), so "8 PCs" is the same 8 every repeat. EXP-05: virtual and real PCs are
never mixed; a run on virtual nodes says so in every row and in the file name.
"""
from __future__ import annotations

import argparse, csv, json, os, statistics, subprocess, sys, time, urllib.parse, urllib.request
from typing import Any

p = argparse.ArgumentParser()
p.add_argument("--head", required=True)
p.add_argument("--pcs", default="1,2,4,all", help="comma list of PC counts; 'all' = every matching PC")
p.add_argument("--repeats", type=int, default=7)
p.add_argument("--warmup", type=int, default=1)
p.add_argument("--n-per-pc", type=float, default=0, help="weak scaling: numbers per PC (n = this x PCs)")
p.add_argument("--n", type=float, default=0, help="strong scaling: the same n for every config")
p.add_argument("--kind", default="sha", choices=["sha", "synth"])
p.add_argument("--prefix", default="00000")
p.add_argument("--synth-ns", type=int, default=1000)
p.add_argument("--policy", default="adaptive", help="comma list: adaptive,guided,fixed")
p.add_argument("--chunk", default="0", help="comma list of chunk sizes; 0 = the policy decides")
p.add_argument("--selector", default="state=active", help="which PCs take part at all (ORG-04)")
p.add_argument("--upload", nargs="?", const="", metavar="NAME", help="at the end send raw.csv and runs.jsonl to the head (POST /api/v1/experiments, needs the researcher role)")
p.add_argument("--out", default="exp-" + time.strftime("%Y%m%d-%H%M"))
a = p.parse_args()
if not (a.n or a.n_per_pc):
    sys.exit("give --n (strong scaling) or --n-per-pc (weak scaling)")
head, token = a.head.rstrip("/"), os.environ.get("CK_TOKEN")


def call(method: str, path: str, body: dict[str, Any] | None = None) -> Any:
    req = urllib.request.Request(head + path, method=method, data=None if body is None else json.dumps(body).encode())
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read()
        return json.loads(raw) if raw[:1] == b"{" else raw.decode()


# the PCs: connected now and matching --selector, real ones only when there are any (EXP-05)
match = set(call("GET", "/api/v1/select?q=" + urllib.parse.quote(a.selector))["node_ids"])
workers = {w["node_id"]: w for w in call("GET", "/api/v1/cluster")["workers"] if w.get("node_id") in match and w.get("threads")}
real = sorted(n for n, w in workers.items() if not w.get("virtual"))
pool = real or sorted(workers)
virtual = not real
if not pool:
    sys.exit(f"no connected PC matches '{a.selector}'")
counts = sorted({len(pool) if c == "all" else int(c) for c in a.pcs.split(",")})
if counts[-1] > len(pool):
    sys.exit(f"asked for {counts[-1]} PCs, only {len(pool)} connected and matching")
configs = [(k, pol, int(float(ch))) for k in counts for pol in a.policy.split(",") for ch in a.chunk.split(",")]
out = a.out + ("-virtual" if virtual else "")
os.makedirs(out, exist_ok=True)
try:
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "status", "--porcelain"], capture_output=True, text=True).stdout.strip())
except OSError:
    commit, dirty = "", False
print(f"{len(configs)} configs x {a.repeats} repeats (+{a.warmup} warm-up) on {len(pool)} {'VIRTUAL ' if virtual else ''}PCs -> {out}/")


def run(cfg: tuple[int, str, int]) -> tuple[dict[str, Any], list[str], int, float, int]:
    k, pol, ch = cfg
    ids = pool[:k]
    n = int(a.n or a.n_per_pc * k)
    body = {"name": f"exp {k}pc {pol} {ch}", "kind": a.kind, "from": 1, "n": n, "chunk": ch, "policy": pol,
            "selector": "node=" + ",".join(ids), "priority": "high"}
    body.update({"sha": a.prefix} if a.kind == "sha" else {"synth_ns": a.synth_ns})
    j = call("POST", "/api/v1/jobs", body)["job"]
    while j["state"] in ("queued", "running"):
        time.sleep(0.5)
        j = call("GET", f"/api/v1/jobs/{j['id']}")["job"]
    if j["state"] != "done":
        raise RuntimeError(f"job {j['id']} {j['state']}: {j.get('error')}")
    secs = (j["finished_ms"] - j["started_ms"]) / 1000
    res = call("GET", f"/api/v1/jobs/{j['id']}/result")
    hits = len(res.splitlines()) if a.kind == "sha" else 0
    return j, ids, n, secs, hits


f = open(os.path.join(out, "raw.csv"), "w", newline="")
w = csv.writer(f)
w.writerow(["config", "repeat", "n", "seconds", "hashes_per_s", "hits", "pcs", "policy", "chunk", "virtual"])
prov = open(os.path.join(out, "runs.jsonl"), "w")
rates = {}
for cfg in configs:
    for _ in range(a.warmup):
        run(cfg)
for r in range(1, a.repeats + 1):
    for cfg in configs[r % len(configs):] + configs[: r % len(configs)]:  # interleaved, rotated start
        t0 = time.time()
        j, ids, n, secs, hits = run(cfg)
        k, pol, ch = cfg
        name = f"{k} PCs" + ("" if len(a.policy.split(",")) == 1 else f" {pol}") + ("" if ch == 0 else f" c{ch}")
        w.writerow([name, r, n, f"{secs:.3f}", round(n / secs), hits, k, pol, ch, int(virtual)])
        f.flush()
        rates.setdefault(name, []).append(n / secs)
        prov.write(json.dumps({"config": name, "repeat": r, "job": j["id"], "commit": commit, "dirty": dirty, "head": head,
                               "kind": a.kind, "n": n, "policy": pol, "chunk": ch, "virtual": virtual, "seconds": secs,
                               "started_ms": j["started_ms"], "finished_ms": j["finished_ms"], "wall_start": t0,
                               "pcs": [{"node_id": i, "host": workers[i].get("host"), "threads": workers[i].get("threads"),
                                        "class": (workers[i].get("inv") or {}).get("cpu")} for i in ids]}) + "\n")
        prov.flush()
        print(f"r{r} {name:<24} {secs:8.3f} s  {n / secs / 1e6:9.1f} M/s  hits {hits}")
f.close()
prov.close()
print("\nconfig                     median M/s   min    max")
for name, v in rates.items():
    print(f"{name:<24} {statistics.median(v) / 1e6:10.1f} {min(v) / 1e6:6.1f} {max(v) / 1e6:6.1f}")

if a.upload is not None:
    try:
        body = {"name": a.upload or os.path.basename(os.path.abspath(out)),
                "raw_csv": open(os.path.join(out, "raw.csv")).read(), "runs_jsonl": open(os.path.join(out, "runs.jsonl")).read()}
        e = call("POST", "/api/v1/experiments", body)["experiment"]
        print(f"\nuploaded as experiment {e['id']} ({e['configs']} configs x {e['repeats']} repeats)")
    except Exception as ex:  # an old head without the route, or no researcher role: the files are on disk anyway
        print(f"\nupload failed ({ex}); results are in {out}/", file=sys.stderr)
