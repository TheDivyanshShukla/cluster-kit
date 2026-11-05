#!/usr/bin/env python3
"""E7 / M9: the full-capacity day. Submits one SHA-256 proof-of-work job at background priority, samples the head
every minute into a CSV, survives head failover (tries every --head in turn), cancels the job at the end and writes a
Markdown report: throughput, utilisation, PCs, failures per hour, failovers.

  python3 scripts/soak.py --head http://10.0.0.5:7702,http://10.0.0.6:7702 --hours 24 --out soak-2026-12-01
  env CK_TOKEN: an operator API token (`ck token`), unless the head runs with CK_AUTH=off.

Energy: CPU package joules from Linux RAPL when the agents can read it (OBS-03); add wall-meter readings by hand.
"""
import argparse, csv, json, os, statistics, sys, time, urllib.request
from datetime import datetime

p = argparse.ArgumentParser()
p.add_argument("--head", required=True, help="comma-separated head URLs (primary and standbys)")
p.add_argument("--hours", type=float, default=24)
p.add_argument("--every", type=float, default=60, help="seconds between samples")
p.add_argument("--prefix", default="000000", help="hex prefix the PoW job searches for")
p.add_argument("--priority", default="background", help="urgent|high|normal|low|background; background = any other job takes the PCs first")
p.add_argument("--out", default="soak-" + datetime.now().strftime("%Y%m%d-%H%M"))
a = p.parse_args()
heads = [h.strip().rstrip("/") for h in a.head.split(",") if h.strip()]
token = os.environ.get("CK_TOKEN")


def call(method, path, body=None):
    """Try each head; a standby answers 503, a dead head refuses: move on. Returns the JSON reply."""
    last = None
    for h in list(heads):
        req = urllib.request.Request(h + path, method=method, data=None if body is None else json.dumps(body).encode())
        req.add_header("Content-Type", "application/json")
        if token:
            req.add_header("Authorization", "Bearer " + token)
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                if heads[0] != h:  # remember who answered
                    heads.remove(h)
                    heads.insert(0, h)
                raw = r.read()
                return json.loads(raw) if raw[:1] in (b"{", b"[") else raw.decode()
        except Exception as e:  # noqa: BLE001 - any failure means "try the next head"
            last = e
    raise RuntimeError(f"no head answered {path}: {last}")


os.makedirs(a.out, exist_ok=True)
job = call("POST", "/api/v1/jobs", {"name": "soak", "kind": "sha", "from": 1, "n": 2**53 - 2, "sha": a.prefix, "priority": a.priority})["job"]
jid = job["id"]
print(f"soak job {jid} submitted; samples every {a.every:.0f} s for {a.hours} h into {a.out}/")
t0 = time.time()
end = t0 + a.hours * 3600
first_event = max((e["id"] for e in call("GET", "/api/v1/events").get("events", [])), default=0)
rows, prev, misses = [], None, 0
last_j, joules = {}, 0.0  # per node: last energy_j seen; total joules (RAPL, OBS-03)
f = open(os.path.join(a.out, "samples.csv"), "w", newline="")
w = csv.writer(f)
w.writerow(["t_s", "pcs", "busy", "threads", "rate_hps", "job_done", "epoch", "role", "joules"])
try:
    while time.time() < end:
        try:
            c, ha = call("GET", "/api/v1/cluster"), call("GET", "/api/v1/ha")
            j = call("GET", f"/api/v1/jobs/{jid}")["job"]
        except RuntimeError as e:
            misses += 1
            print(f"{time.time() - t0:8.0f}s  {e}", file=sys.stderr)
            time.sleep(a.every)
            continue
        ws = [x for x in c["workers"] if x.get("threads")]
        for x in ws:
            n, e = x.get("node_id"), x.get("energy_j") or 0
            if n and e:
                d = e - last_j.get(n, e)
                joules += d if d > 0 else 0  # an agent restart resets its counter: skip that step
                last_j[n] = e
        now = time.time()
        rate = (j["done"] - prev[1]) / (now - prev[0]) if prev and now > prev[0] else 0.0
        prev = (now, j["done"])
        row = [round(now - t0), len(ws), sum(1 for x in ws if x.get("busy")), sum(x["threads"] for x in ws), round(rate), j["done"], ha.get("epoch"), ha.get("role"), round(joules)]
        w.writerow(row)
        f.flush()
        rows.append(row)
        print(f"{row[0]:8d}s  {row[1]} PCs ({row[2]} busy)  {rate / 1e6:9.1f} MH/s  epoch {row[6]}")
        if j["state"] != "running" and j["state"] != "queued":
            print(f"job ended early: {j['state']} {j.get('error') or ''}")
            break
        time.sleep(max(0, a.every - (time.time() - now)))
except KeyboardInterrupt:
    print("stopped by hand")
finally:
    f.close()

try:
    call("POST", f"/api/v1/jobs/{jid}/cancel")
except RuntimeError:
    pass
hours = (time.time() - t0) / 3600
events = [e for e in call("GET", "/api/v1/events").get("events", []) if e["id"] > first_event]  # ponytail: first page only
kinds = {}
for e in events:
    kinds[e["kind"]] = kinds.get(e["kind"], 0) + 1
rates = [r[4] for r in rows[1:]] or [0]
pcs = [r[1] for r in rows] or [0]
util = [r[2] / r[1] for r in rows if r[1]] or [0]
epochs = sorted({r[6] for r in rows if r[6] is not None})
done = rows[-1][5] if rows else 0
energy = (f"{joules / 3.6e6:.2f} kWh from {len(last_j)} PCs, {done / joules / 1e6:.2f} MH per joule (CPU packages only; add the wall meter)"
          if joules else "not readable on these PCs (RAPL needs Linux and root): add wall-meter kWh here")
q = lambda xs, k: sorted(xs)[int(k * (len(xs) - 1))]
report = f"""# Soak report: {datetime.fromtimestamp(t0):%Y-%m-%d %H:%M} for {hours:.2f} h

| Measure | Value |
| --- | --- |
| Job | {jid}, SHA-256 prefix `{a.prefix}`, priority {a.priority} |
| Hashes done | {done:,} |
| Throughput (MH/s) | mean {statistics.mean(rates) / 1e6:.1f}, p5 {q(rates, 0.05) / 1e6:.1f}, max {max(rates) / 1e6:.1f} |
| PCs connected | min {min(pcs)}, median {q(pcs, 0.5)}, max {max(pcs)} |
| Busy share of connected PCs | mean {100 * statistics.mean(util):.1f} % |
| PCs lost (node.lost) | {kinds.get('node.lost', 0)} ({kinds.get('node.lost', 0) / max(hours, 1e-9):.2f} per hour) |
| Task timeouts | {kinds.get('task.timeout', 0)} |
| Head failovers | {max(0, len(epochs) - 1)} (epochs {', '.join(map(str, epochs)) or '-'}) |
| Samples missed (no head answered) | {misses} of {len(rows) + misses} |
| CPU package energy (RAPL) | {energy} |

Samples: `samples.csv` (one row per {a.every:.0f} s).
"""
open(os.path.join(a.out, "report.md"), "w").write(report)
print(report)
