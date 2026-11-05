"""E1 scaling figures from scripts/experiment.py output (raw.csv with a `pcs` column), for any number of PCs.

  python3 docs/paper/figs_scale.py exp-e1/raw.csv        -> exp-e1/fig_scale_throughput.pdf, fig_scale_efficiency.pdf

figs.py stays as it is for the 4-PC paper. Ideal = p x the median rate of the 1-PC config (each policy/chunk line on
its own). Virtual-node runs get "(virtual nodes)" in the title (EXP-05).
"""
import csv, pathlib, sys
import numpy as np
import matplotlib
matplotlib.use("pdf")
import matplotlib.pyplot as plt

src = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "raw.csv")
rows = list(csv.DictReader(open(src)))
virtual = any(r.get("virtual") == "1" for r in rows)
lines = {}  # (policy, chunk) -> pcs -> [M/s]
for r in rows:
    lines.setdefault((r["policy"], r["chunk"]), {}).setdefault(int(r["pcs"]), []).append(int(r["n"]) / float(r["seconds"]) / 1e6)

plt.rcParams.update({
    "font.family": "serif", "font.serif": ["STIX Two Text", "STIXGeneral", "Times New Roman"], "mathtext.fontset": "stix",
    "font.size": 9, "axes.labelsize": 9, "legend.fontsize": 7, "xtick.labelsize": 8, "ytick.labelsize": 8,
    "axes.linewidth": 0.6, "xtick.direction": "in", "ytick.direction": "in", "lines.linewidth": 1.1, "lines.markersize": 4,
    "legend.frameon": False, "pdf.fonttype": 42,
})
W, tag = 3.4, " (virtual nodes)" if virtual else ""
marks = "osd^v<>"


def label(pol, ch):
    return pol + ("" if ch == "0" else f", chunk {int(ch):,}")


def save(fig, name):
    fig.tight_layout(pad=0.3)
    fig.savefig(src.parent / f"{name}.pdf")
    plt.close(fig)


fig, ax = plt.subplots(figsize=(W, 2.5))
fig2, ax2 = plt.subplots(figsize=(W, 2.1))
for i, ((pol, ch), by) in enumerate(sorted(lines.items())):
    p = np.array(sorted(by))
    med = np.array([np.median(by[k]) for k in p])
    lo, hi = med - [min(by[k]) for k in p], [max(by[k]) for k in p] - med
    ax.errorbar(p, med, yerr=[lo, hi], fmt=marks[i % len(marks)] + "-", color="k" if i == 0 else f"0.{3 + 2 * i % 6}",
                mfc="white", capsize=2, label=f"{label(pol, ch)} (median, range)")
    if 1 in by:
        base = np.median(by[1])
        if i == 0:
            ax.plot(p, p * base, "k--", lw=0.8, label=r"ideal, $p \cdot X_1$")
        ax2.plot(p, med / (p * base) * 100, marks[i % len(marks)] + "-", color="k" if i == 0 else "0.5", mfc="white", label=label(pol, ch))
log = len(lines) and max(max(by) for by in lines.values()) >= 16
for a in (ax, ax2):
    if log:
        a.set_xscale("log", base=2)
    a.set_xlabel(r"PCs $p$")
ax.set_ylabel(r"throughput $X_p$ (M/s)")
ax.set_title("E1 throughput" + tag, fontsize=9)
ax.legend(loc="upper left")
save(fig, "fig_scale_throughput")
ax2.axhline(100, color="k", lw=0.6, ls=":")
ax2.set_ylabel(r"efficiency $E_p$ (%)")
ax2.set_title("E1 efficiency" + tag, fontsize=9)
ax2.legend(loc="lower left")
save(fig2, "fig_scale_efficiency")
print(f"figures written to {src.parent}")
