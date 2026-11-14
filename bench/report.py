"""Charts and a Markdown report from an experiment.py run.

  python3 bench/report.py bench/<run-dir> [--head http://127.0.0.1:7702]

Three efficiency figures, all from the same runs:
  vs N x first PC   measured / (N x the 1-PC throughput). Fair only when every PC is the same model.
  hardware-group    measured / sum over the PCs in the run of their hardware group's speed. A group is one CPU model
                    with one thread count; its speed is the median, over the group's PCs, of each PC's own compute
                    rate (numbers / busy seconds) in the largest runs. The headline on mixed hardware. Needs --head.
  head-measured     compute time / (PCs x wall time), from the head's per-job breakdown. Shows where time goes
                    (tail idle, dispatch); generous as a headline because a PC that computes slowly still counts as busy.
With --head the last two are computed and saved to heff.json for bench/pdf.py.
"""
import csv, json, statistics, sys, pathlib, urllib.request
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

d = pathlib.Path(sys.argv[1])
head = sys.argv[sys.argv.index("--head") + 1] if "--head" in sys.argv else None
rows = list(csv.DictReader(open(d / "raw.csv")))
by, secs = {}, {}
for r in rows:
    by.setdefault(int(r["pcs"]), []).append(float(r["hashes_per_s"]) / 1e6)
    secs.setdefault(int(r["pcs"]), []).append(float(r["seconds"]))
pcs = sorted(by)
med = [statistics.median(by[p]) for p in pcs]
lo = [min(by[p]) for p in pcs]
hi = [max(by[p]) for p in pcs]
base = med[0] / pcs[0]  # throughput of one PC
ideal = [base * p for p in pcs]
eff = [100 * m / i for m, i in zip(med, ideal)]


def group_of(inv: dict) -> str:
    """Hardware group of a PC: CPU model and thread count."""
    model = (inv.get("cpu_model") or "unknown").replace("Intel(R) Core(TM) ", "").replace(" CPU", "").strip()
    return f"{model}, {inv.get('cores_logical', '?')} threads"


heff, tail, geff, gideal, groups = {}, {}, {}, {}, {}
if head:
    get = lambda p: json.load(urllib.request.urlopen(head + p, timeout=10))
    runs = [json.loads(l) for l in open(d / "runs.jsonl")]
    own: dict[str, list[float]] = {}  # node -> its compute rate in each run of the largest size, M/s
    for r in runs:
        try:  # breakdowns are not replicated: a job run under an earlier head has none here
            b = get(f"/api/v1/jobs/{r['job']}/breakdown")["breakdown"]
        except OSError:
            continue
        k = int(r["config"].split()[0])
        heff.setdefault(k, []).append(100 * b["efficiency"])
        tail.setdefault(k, []).append(100 * b["pc_seconds"]["tail_idle"] / max(sum(b["pc_seconds"].values()), 1e-9))
        if k == pcs[-1]:
            for p in b["per_pc"]:
                if p["busy_s"] > 0:
                    own.setdefault(p["node_id"], []).append(p["numbers"] / p["busy_s"] / 1e6)
    heff = {k: statistics.median(v) for k, v in heff.items()}
    tail = {k: statistics.median(v) for k, v in tail.items()}
    if set(heff) != set(pcs):
        print(f"head has breakdowns for {sorted(heff)} only; head-measured and hardware-group efficiency left out", file=sys.stderr)
        heff, tail = {}, {}
    else:
        grp = {n["node_id"]: group_of(n.get("inventory") or {}) for n in get("/api/v1/nodes")["nodes"]}
        speeds: dict[str, list[float]] = {}
        for node, v in own.items():
            speeds.setdefault(grp.get(node, "unknown"), []).append(statistics.median(v))
        gspeed = {g: statistics.median(v) for g, v in speeds.items()}
        groups = {g: [len(v), gspeed[g]] for g, v in sorted(speeds.items(), key=lambda x: -len(x[1]))}
        for r in runs:
            k = int(r["config"].split()[0])
            gideal[k] = sum(gspeed.get(grp.get(p["node_id"], "unknown"), base) for p in r["pcs"])
        geff = {k: 100 * m / gideal[k] for k, m in zip(pcs, med)}
        (d / "heff.json").write_text(json.dumps({"eff": heff, "tail": tail, "group": geff, "group_ideal": gideal, "groups": groups}))

fig, ax = plt.subplots(1, 3, figsize=(15, 4.2))
ax[0].plot(pcs, ideal, "--", color="#bbb", label="N x first PC")
if geff:
    ax[0].plot(pcs, [gideal[p] for p in pcs], "--", color="#16a34a", label="ideal for this hardware (by group)")
ax[0].errorbar(pcs, med, yerr=[[m - l for m, l in zip(med, lo)], [h - m for m, h in zip(med, hi)]], fmt="o-", color="#2563eb", capsize=3, label="measured (median, min-max)")
ax[0].set(title="Throughput", xlabel="PCs", ylabel="M hashes/s"); ax[0].legend()
if geff:
    ax[1].plot(pcs, [geff[p] for p in pcs], "o-", color="#16a34a", label="hardware-group (each CPU model at its own speed)")
    ax[1].plot(pcs, [heff[p] for p in pcs], "^:", color="#2563eb", label="head-measured (share of time computing)")
    ax[1].plot(pcs, eff, "s--", color="#9ca3af", label="vs N x first PC")
    ax[1].legend(loc="lower left")
else:
    ax[1].plot(pcs, eff, "o-", color="#16a34a")
ax[1].axhline(100, ls="--", color="#999")
ax[1].set(title="Scaling efficiency", xlabel="PCs", ylabel="%", ylim=(0, 110))
ax[2].boxplot([secs[p] for p in pcs], tick_labels=[str(p) for p in pcs])
ax[2].set(title="Run time (weak scaling: flat = ideal)", xlabel="PCs", ylabel="seconds")
for a in ax:
    a.grid(alpha=0.3)
fig.tight_layout()
fig.savefig(d / "scaling.png", dpi=130)

hcol = " group ideal M/s | hardware-group eff | head-measured eff | tail idle |" if geff else ""
lines = [f"# Benchmark report: {d.name}", "", f"{len(rows)} recorded runs, SHA-256 search, {rows[0]['policy']} policy.", "",
         f"| PCs | median M/s | min | max | N x first PC | eff vs N x first PC |{hcol} median s |", "|---" * (7 + 4 * bool(geff)) + "|"]
for p, m, l, h, i, e in zip(pcs, med, lo, hi, ideal, eff):
    hv = f" {gideal[p]:.1f} | {geff[p]:.1f}% | {heff[p]:.1f}% | {tail[p]:.1f}% |" if geff else ""
    lines.append(f"| {p} | {m:.1f} | {l:.1f} | {h:.1f} | {i:.1f} | {e:.0f}% |{hv} {statistics.median(secs[p]):.2f} |")
if groups:
    lines += ["", "Hardware groups (PCs, M hashes/s each): " + "; ".join(f"{g}: {n} x {s:.1f}" for g, (n, s) in groups.items())]
lines += ["", f"Speed-up at {pcs[-1]} PCs: {med[-1] / med[0] * pcs[0]:.1f}x over one PC.", "", "![scaling](scaling.png)"]
(d / "REPORT.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
