"""Charts and a Markdown report from an experiment.py run.

  python3 bench/report.py bench/<run-dir> [--head http://127.0.0.1:7702]

With --head it also reads each job's time breakdown from the head and reports the head-measured efficiency: the share
of PC-time spent computing (every PC judged at its own speed, so it is fair on mixed hardware). Saved to heff.json for
bench/pdf.py. Without it, only the naive efficiency (vs N copies of the first PC) is shown.
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

heff, tail = {}, {}
if head:
    for l in open(d / "runs.jsonl"):
        r = json.loads(l)
        try:  # breakdowns are not replicated: a job run under an earlier head has none here
            b = json.load(urllib.request.urlopen(f"{head}/api/v1/jobs/{r['job']}/breakdown", timeout=10))["breakdown"]
        except OSError:
            continue
        k = int(r["config"].split()[0])
        heff.setdefault(k, []).append(100 * b["efficiency"])
        tail.setdefault(k, []).append(100 * b["pc_seconds"]["tail_idle"] / max(sum(b["pc_seconds"].values()), 1e-9))
    heff = {k: statistics.median(v) for k, v in heff.items()}
    tail = {k: statistics.median(v) for k, v in tail.items()}
    if set(heff) != set(pcs):
        print(f"head has breakdowns for {sorted(heff)} only; head-measured efficiency left out", file=sys.stderr)
        heff, tail = {}, {}
    else:
        (d / "heff.json").write_text(json.dumps({"eff": heff, "tail": tail}))

fig, ax = plt.subplots(1, 3, figsize=(15, 4.2))
ax[0].plot(pcs, ideal, "--", color="#999", label="ideal (N x one PC)")
ax[0].errorbar(pcs, med, yerr=[[m - l for m, l in zip(med, lo)], [h - m for m, h in zip(med, hi)]], fmt="o-", color="#2563eb", capsize=3, label="measured (median, min-max)")
ax[0].set(title="Throughput", xlabel="PCs", ylabel="M hashes/s"); ax[0].legend()
if heff:
    ax[1].plot(pcs, [heff[p] for p in pcs], "o-", color="#16a34a", label="head-measured (each PC at its own speed)")
    ax[1].plot(pcs, eff, "s--", color="#9ca3af", label="vs N copies of the first PC")
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

hcol = " head-measured eff | tail idle |" if heff else ""
lines = [f"# Benchmark report: {d.name}", "", f"{len(rows)} recorded runs, SHA-256 search, {rows[0]['policy']} policy.", "",
         f"| PCs | median M/s | min | max | ideal M/s | eff vs N x one PC |{hcol} median s |", "|---" * (8 + 2 * bool(heff)) + "|"]
for p, m, l, h, i, e in zip(pcs, med, lo, hi, ideal, eff):
    hv = f" {heff[p]:.1f}% | {tail[p]:.1f}% |" if heff else ""
    lines.append(f"| {p} | {m:.1f} | {l:.1f} | {h:.1f} | {i:.1f} | {e:.0f}% |{hv} {statistics.median(secs[p]):.2f} |")
lines += ["", f"Speed-up at {pcs[-1]} PCs: {med[-1] / med[0] * pcs[0]:.1f}x over one PC.", "", "![scaling](scaling.png)"]
(d / "REPORT.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
