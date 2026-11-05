"""Figures for the Cluster Kit paper, drawn from the benchmark's raw.csv."""
import csv, pathlib, numpy as np
import matplotlib
matplotlib.use("pdf")
import matplotlib.pyplot as plt

HERE = pathlib.Path(__file__).parent
rows = list(csv.DictReader(open(HERE / "data/raw.csv")))
by = {}
for r in rows:
    by.setdefault(r["config"], []).append(int(r["n"]) / float(r["seconds"]) / 1e6)
S = ["head", "192.168.10.200", "192.168.10.3", "192.168.10.5"]
M = ["head", "2 PCs", "3 PCs", "4 PCs"]
med = {k: float(np.median(v)) for k, v in by.items()}
avg = np.mean([med[s] for s in S])
p = np.arange(1, 5)
X = np.array([med[c] for c in M])
ideal = np.array([sum(med[s] for s in S[:k]) for k in p])
inside = [21.31, 23.23, 23.12, 23.15]
share = [23.9, 25.7, 25.2, 25.2]

plt.rcParams.update({
    "font.family": "serif", "font.serif": ["STIX Two Text", "STIXGeneral", "Times New Roman"], "mathtext.fontset": "stix",
    "font.size": 9, "axes.labelsize": 9, "legend.fontsize": 8, "xtick.labelsize": 8, "ytick.labelsize": 8,
    "axes.linewidth": 0.6, "xtick.direction": "in", "ytick.direction": "in", "xtick.top": True, "ytick.right": True,
    "lines.linewidth": 1.1, "lines.markersize": 4, "legend.frameon": False, "pdf.fonttype": 42,
})
W = 3.4  # one column of the paper, inches

def save(fig, name):
    fig.tight_layout(pad=0.3); fig.savefig(HERE / f"{name}.pdf"); plt.close(fig)

# Throughput scaling with per-repeat spread.
fig, ax = plt.subplots(figsize=(W, 2.35))
ax.plot(p, ideal, "k--", lw=0.8, label=r"ideal, $\sum_{i\leq p} X_i$")
lo = [X[i] - min(by[c]) for i, c in enumerate(M)]; hi = [max(by[c]) - X[i] for i, c in enumerate(M)]
ax.errorbar(p, X, yerr=[lo, hi], fmt="o-", color="k", mfc="white", capsize=2.5, label="measured (median, range)")
for i in range(4):
    ax.annotate(f"{X[i]:.1f}", (p[i], X[i]), textcoords="offset points", xytext=(6, -10) if i < 3 else (-24, -3), fontsize=7.5)
ax.set_xticks(p); ax.set_xlabel(r"nodes $p$"); ax.set_ylabel(r"throughput $X_p$ (MH/s)")
ax.set_xlim(0.7, 4.3); ax.set_ylim(0, 100); ax.legend(loc="upper left")
save(fig, "fig_throughput")

# Speed-up and efficiency.
fig, (a1, a2) = plt.subplots(1, 2, figsize=(W, 2.0))
a1.plot([1, 4], [1, 4], "k:", lw=0.8, label="linear")
a1.plot(p, X / X[0], "s-", color="0.45", mfc="white", label="vs. node 1")
a1.plot(p, X / avg, "o-", color="k", label=r"vs. $\bar{X}_1$")
a1.set_xticks(p); a1.set_xlabel(r"$p$"); a1.set_ylabel(r"speed-up $S_p$"); a1.legend(loc="upper left", fontsize=7)
a1.set_ylim(0.5, 4.6)
a2.bar(p, X / ideal * 100, color="0.75", edgecolor="k", lw=0.5, width=0.55)
for i in range(4):
    a2.text(p[i], X[i] / ideal[i] * 100 + 0.4, f"{X[i]/ideal[i]*100:.1f}", ha="center", fontsize=7)
a2.set_ylim(85, 105); a2.set_xticks(p); a2.set_xlabel(r"$p$"); a2.set_ylabel(r"efficiency $E_p$ (%)")
a2.axhline(100, color="k", lw=0.6, ls=":")
save(fig, "fig_speedup")

# Per-node rate alone vs inside the 4-node run.
fig, ax = plt.subplots(figsize=(W, 2.1))
x = np.arange(4); w = 0.36
ax.bar(x - w / 2, [med[s] for s in S], w, color="white", edgecolor="k", lw=0.6, hatch="////", label="alone")
ax.bar(x + w / 2, inside, w, color="0.55", edgecolor="k", lw=0.6, label=r"inside $p=4$ run")
for i in range(4):
    ax.text(x[i] + w / 2, inside[i] + 0.5, f"{share[i]:.1f}%", ha="center", fontsize=7)
ax.set_xticks(x, ["N1 (head)", "N2", "N3", "N4"]); ax.set_ylabel("rate (MH/s)"); ax.set_ylim(0, 29)
ax.legend(loc="upper center", ncol=2, bbox_to_anchor=(0.5, 1.02))
save(fig, "fig_pernode")

# Run-to-run variability.
fig, ax = plt.subplots(figsize=(W, 2.0))
order = S + ["2 PCs", "3 PCs", "4 PCs"]
data = [[(v / med[c] - 1) * 100 for v in by[c]] for c in order]
ax.boxplot(data, widths=0.5, medianprops={"color": "k"}, flierprops={"markersize": 3}, whis=(0, 100))
rng = np.random.default_rng(0)
for i, d in enumerate(data):
    ax.scatter(i + 1 + rng.uniform(-0.12, 0.12, len(d)), d, s=6, color="k", zorder=3)
ax.axhline(0, color="k", lw=0.5, ls=":")
ax.set_xticks(range(1, 8), ["N1", "N2", "N3", "N4", "$p$=2", "$p$=3", "$p$=4"])
ax.set_ylabel("deviation from median (%)")
save(fig, "fig_variability")
name = {"head": "N1", "192.168.10.200": "N2", "192.168.10.3": "N3", "192.168.10.5": "N4", "2 PCs": "$p{=}2$", "3 PCs": "$p{=}3$", "4 PCs": "$p{=}4$"}
(HERE / "raw.tex").write_text("\n".join(
    f"{name[r['config']]} & {r['repeat']} & {int(r['n']):,} & {float(r['seconds']):.3f} & {int(r['hashes_per_s'])/1e6:.2f} & {r['hits']} \\\\" for r in rows))
print("figures written")
