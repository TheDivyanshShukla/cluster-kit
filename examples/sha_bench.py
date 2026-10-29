"""SHA-256 benchmark on the Rust engine (ck): does adding PCs make the search faster, provably?

Start `./kit rs head` (+ `./kit rs worker` on the other PCs, add --gpu on all for GPU hashing), then
on the head PC:
    ./kit py run examples/sha_bench.py                    # ~3 s per run, 7 repeats
    ./kit py run examples/sha_bench.py --seconds 5 --repeats 9 --label gpu

What it runs (head PC first, then the others in IP order):
  - every PC alone                 -> each PC's own compute
  - the first 2, 3, ..., N PCs     -> scaling, ending with all PCs together

How it stays consistent:
  - every config is sized to take about --seconds, so fixed costs weigh the same everywhere
  - one discarded warm-up run per config (calibration doubles as warm-up)
  - repeats run round-robin (A B C 2PC 3PC, A B C 2PC 3PC, ...) so drift such as heat hits all equally
  - each config hashes the same range every time, so its hit count must not change; a change aborts
  - reports median, mean, stdev, CV and a 95% confidence interval; CV above --max-cv is flagged

How it shows the PCs help:
  - exact one-sided permutation test: k PCs faster than k-1 PCs, and all PCs faster than the best
    single PC (no normality assumption; with 7 repeats the smallest possible p is 1/3432)
  - speedup of all PCs over the head alone, with a bootstrap 95% CI
  - parallel efficiency: measured throughput / sum of the PCs' solo throughputs

Output: bench_out/<time>[-label]/ raw.csv, summary.csv, tests.csv, report.md, bench.png
"""
import argparse
import csv
import json
import random
import re
import statistics
import subprocess
import sys
import time
import urllib.request
from itertools import combinations
from math import comb
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CK = ROOT / "rust" / "target" / "release" / ("ck.exe" if sys.platform == "win32" else "ck")
T95 = {1: 12.71, 2: 4.30, 3: 3.18, 4: 2.78, 5: 2.57, 6: 2.45, 7: 2.36, 8: 2.31, 9: 2.26, 10: 2.23}
ALPHA = 0.01
# Reference points for the time-to-find chart. Measured: the Python Dask run. Estimates: the rest.
REFS = {"Python Dask, this cluster (measured)": 5.55e6,
        "1 datacenter GPU (est.)": 1e10,
        "Frontier supercomputer (est.)": 3.8e14,
        "Bitcoin network (est.)": 1e21}


# ------------------------------------------------------------------ running --
def stats(head: str) -> dict[str, dict]:
    with urllib.request.urlopen(f"http://{head}:7702/stats", timeout=5) as r:
        return {w["addr"]: w for w in json.load(r)["workers"]}


def ck_run(head: str, n: int, prefix: str, only: str) -> tuple[float, int]:
    """One job. Returns (seconds measured by ck, hit count)."""
    cmd = [str(CK), "run", "--head", head, "--n", str(n), "--sha", prefix, "--only", only]
    p = subprocess.run(cmd, capture_output=True, text=True)
    secs = re.search(r"numbers in ([\d.]+)s", p.stderr)
    hits = re.search(r"^(\d+) hashes start", p.stdout, re.M)
    if p.returncode or not secs or not hits:
        sys.exit(f"ck run failed: {p.stderr.strip() or p.stdout.strip()}")
    return float(secs[1]), int(hits[1])


def measure(head: str, n: int, prefix: str, only: str) -> dict:
    before = stats(head)
    secs, hits = ck_run(head, n, prefix, only)
    after = stats(head)
    per = {a: (after[a]["numbers"] - before[a]["numbers"], after[a]["secs"] - before[a]["secs"])
           for a in after if a in before}
    return {"secs": secs, "hits": hits, "rate": n / secs, "per": per}


def calibrate(head: str, prefix: str, only: str, target: float) -> int:
    """Grow n until a run lasts >= 0.5 s, then size it to `target` seconds (2 significant digits)."""
    n = 10_000_000
    while True:
        r = measure(head, n, prefix, only)
        if r["secs"] >= 0.5:
            want = r["rate"] * target
            mag = 10 ** (len(str(int(want))) - 2)
            return max(mag, round(want / mag) * mag)
        n *= 4


# -------------------------------------------------------------- statistics --
def summarize(rates: list[float]) -> dict:
    k, med, mean = len(rates), statistics.median(rates), statistics.fmean(rates)
    sd = statistics.stdev(rates) if k > 1 else 0.0
    ci = T95.get(k - 1, 2.0) * sd / k ** 0.5 if k > 1 else 0.0
    return {"median": med, "mean": mean, "stdev": sd, "cv_pct": 100 * sd / mean,
            "ci95": ci, "min": min(rates), "max": max(rates)}


def perm_p(slow: list[float], fast: list[float]) -> float:
    """One-sided permutation test, H1: `fast` has the higher mean. Exact up to 200k splits."""
    pool, k = slow + fast, len(fast)
    obs = statistics.fmean(fast) - statistics.fmean(slow)
    total = sum(pool)
    if comb(len(pool), k) <= 200_000:
        splits = combinations(range(len(pool)), k)
    else:
        rng = random.Random(0)
        splits = (rng.sample(range(len(pool)), k) for _ in range(50_000))
    hit = n = 0
    for idx in splits:
        s = sum(pool[i] for i in idx)
        hit += s / k - (total - s) / len(slow) >= obs - 1e-9
        n += 1
    return hit / n


def boot_ratio(num: list[float], den: list[float]) -> tuple[float, float]:
    """95% bootstrap CI of median(num) / median(den)."""
    rng = random.Random(0)
    rs = sorted(statistics.median(rng.choices(num, k=len(num))) / statistics.median(rng.choices(den, k=len(den)))
                for _ in range(5000))
    return rs[125], rs[4874]


def fmt_time(s: float) -> str:
    for unit, size in (("years", 31_557_600), ("days", 86_400), ("h", 3600), ("min", 60), ("s", 1),
                       ("ms", 1e-3), ("µs", 1e-6), ("ns", 1e-9)):
        if s >= size:
            return f"{s / size:,.1f} {unit}"
    return f"{s * 1e12:.1f} ps"


# ------------------------------------------------------------------- charts --
def plot(out: Path, label: str, names: dict, addrs: list, scale: list, summary: dict, raw: list,
         combined: dict, speedup: tuple) -> None:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    M = 1e6
    fig, ax = plt.subplots(2, 2, figsize=(14, 10))
    fig.suptitle(f"SHA-256 prefix search on ck{f' ({label})' if label else ''}: "
                 f"median of repeats, error bars = 95% CI", fontsize=13)

    a = ax[0][0]
    ks = list(range(1, len(scale) + 1))
    med = [summary[c]["median"] / M for c in scale]
    ideal = [sum(summary[x]["median"] for x in addrs[:k]) / M for k in ks]
    a.errorbar(ks, med, yerr=[summary[c]["ci95"] / M for c in scale], fmt="o-", capsize=5, lw=2,
               color="#F58518", label="measured")
    a.plot(ks, ideal, "s--", color="gray", label="ideal: sum of those PCs alone")
    for k, m in zip(ks, med):
        a.text(k, m, f" {m:,.1f}", va="top", fontsize=9)
    s, lo, hi = speedup
    a.set(title=f"Scaling: all {len(scale)} PCs = {s:.2f}x the head alone (95% CI {lo:.2f}-{hi:.2f})",
          xlabel="PCs used (head first)", ylabel="million hashes / s", xticks=ks, ylim=(0, None))
    a.legend()

    a = ax[0][1]
    x = range(len(addrs))
    a.bar([i - 0.2 for i in x], [summary[c]["median"] / M for c in addrs], 0.4,
          yerr=[summary[c]["ci95"] / M for c in addrs], capsize=4, label="alone", color="#4C78A8")
    if combined:
        a.bar([i + 0.2 for i in x], [combined[c]["rate"] / M for c in addrs], 0.4,
              label=f"inside the {len(scale)}-PC run", color="#F58518")
        for i, c in enumerate(addrs):
            a.text(i + 0.2, combined[c]["rate"] / M, f"{combined[c]['share']:.0%} of work\n"
                   f"busy {combined[c]['busy']:.0%}", ha="center", va="bottom", fontsize=8)
    a.set_xticks(list(x), [names[c] for c in addrs])
    a.set(title="Each PC's compute: alone vs inside the all-PC run", ylabel="million hashes / s")
    a.legend()

    a = ax[1][0]
    cols = list(summary)
    for i, c in enumerate(cols):
        ys = [100 * (r["rate"] / summary[c]["median"] - 1) for r in raw if r["config"] == c]
        a.scatter([i] * len(ys), ys, alpha=0.7)
        a.text(i, max(ys), f"CV {summary[c]['cv_pct']:.2f}%", ha="center", va="bottom", fontsize=8)
    a.axhline(0, color="gray", lw=0.8)
    a.set_xticks(range(len(cols)), [names[c] for c in cols], rotation=20)
    a.set(title="Run-to-run consistency (each dot = one repeat)", ylabel="% from that config's median")

    a = ax[1][1]
    ks = list(range(6, 21))
    series = {f"head alone (measured)": summary[addrs[0]]["median"],
              f"all {len(scale)} PCs (measured)": summary[scale[-1]]["median"]} | REFS
    for lbl, rate in series.items():
        a.plot(ks, [16 ** k / rate for k in ks], "o-" if "measured" in lbl else "--", ms=3, label=lbl)
    for t, txt in ((1, "1 s"), (3600, "1 h"), (86_400 * 365, "1 yr")):
        a.axhline(t, color="gray", lw=0.6, ls=":")
        a.text(ks[0], t, txt, fontsize=8, va="bottom")
    a.set_yscale("log")
    a.set(title="Expected time to find ONE hash with k leading hex zeros (16^k / rate)",
          xlabel="k (leading zeros)", ylabel="seconds", xticks=ks)
    a.legend(fontsize=7)

    for row in ax:
        for a in row:
            a.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(out / "bench.png", dpi=150)


# --------------------------------------------------------------------- main --
def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--head", default="127.0.0.1", help="head PC (run this on the head)")
    ap.add_argument("--prefix", default="000000", help="hex prefix searched (hit count = correctness check)")
    ap.add_argument("--seconds", type=float, default=3.0, help="target length of each run")
    ap.add_argument("--repeats", type=int, default=7, help="at least 5 for meaningful p-values")
    ap.add_argument("--max-cv", type=float, default=2.0, help="flag configs noisier than this (%%)")
    ap.add_argument("--job", type=float, default=1e11, help="example job size for the time-saved table")
    ap.add_argument("--label", default="", help="tag for this run, e.g. cpu or gpu")
    args = ap.parse_args()
    if not CK.exists():
        sys.exit("ck not built. Run: ./kit setup rs")
    if args.repeats < 5:
        sys.exit("--repeats must be at least 5 for the significance tests")

    t_all = time.perf_counter()
    try:
        workers = stats(args.head)
    except OSError as e:
        sys.exit(f"Cannot reach the ck head dashboard at {args.head}:7702 ({e}). Is `./kit rs head` running?")
    if len(workers) < 2:
        sys.exit(f"{len(workers)} ck worker(s) connected; start `./kit rs worker` on the other PCs first.")
    addrs = sorted(workers, key=lambda a: (a != "127.0.0.1", a))   # head's own worker first
    names = {a: "head" if a == "127.0.0.1" else a for a in addrs}
    configs = {a: a for a in addrs}                                  # config -> --only value
    for k in range(2, len(addrs) + 1):
        configs[f"{k} PCs"] = ",".join(addrs[:k])
        names[f"{k} PCs"] = f"{k} PCs"
    scale = [addrs[0]] + [f"{k} PCs" for k in range(2, len(addrs) + 1)]   # 1, 2, ..., N PCs
    top = scale[-1]
    print("PCs: " + ", ".join(f"{names[a]} [{workers[a]['threads']} threads]" for a in addrs))
    print(f"Configs: {', '.join(names[c] for c in configs)}   repeats: {args.repeats}")

    print(f"\nCalibrating to ~{args.seconds:g} s per run (also warms up) ...")
    sizes = {}
    for c, only in configs.items():
        sizes[c] = calibrate(args.head, args.prefix, only, args.seconds)
        print(f"  {names[c]:<16} n = {sizes[c]:,}")

    raw = []
    for rep in range(1, args.repeats + 1):
        line = []
        for c, only in configs.items():
            r = measure(args.head, sizes[c], args.prefix, only)
            raw.append({"config": c, "repeat": rep, **r})
            line.append(f"{names[c]} {r['rate'] / 1e6:,.1f} M/s {r['secs']:.2f}s")
        print(f"  repeat {rep}/{args.repeats}: " + "  ".join(line))

    # --- correctness: same range, same hits, every time
    for c in configs:
        hits = {r["hits"] for r in raw if r["config"] == c}
        if len(hits) != 1:
            sys.exit(f"INCONSISTENT RESULT: {names[c]} found {sorted(hits)} hits on the same range. Results discarded.")

    rates = {c: [r["rate"] for r in raw if r["config"] == c] for c in configs}
    summary = {c: summarize(rates[c]) for c in configs}
    noisy = [names[c] for c in configs if summary[c]["cv_pct"] > args.max_cv]

    # --- significance: each added PC helps; all PCs beat the best single PC
    tests = []
    for prev, cur in zip(scale, scale[1:]):
        tests.append((f"{names[cur]} > {names[prev] if prev in addrs else prev}", prev, cur))
    best = max(addrs, key=lambda a: summary[a]["median"])
    tests.append((f"{names[top]} > best single PC ({names[best]})", best, top))
    test_rows = []
    for title, slow, fast in tests:
        p = perm_p(rates[slow], rates[fast])
        gain = summary[fast]["median"] / summary[slow]["median"]
        test_rows.append({"test": title, "gain": gain, "p": p, "significant": p < ALPHA})
    speedup = (summary[top]["median"] / summary[addrs[0]]["median"], *boot_ratio(rates[top], rates[addrs[0]]))
    ideal = sum(summary[a]["median"] for a in addrs)
    eff = summary[top]["median"] / ideal
    all_sig = all(t["significant"] for t in test_rows)

    # --- each PC inside the all-PC runs
    top_runs = [r for r in raw if r["config"] == top]
    combined = {}
    for a in addrs:
        num = sum(r["per"].get(a, (0, 0))[0] for r in top_runs)
        busy = sum(r["per"].get(a, (0, 0))[1] for r in top_runs)
        combined[a] = {"share": num / (sizes[top] * len(top_runs)), "rate": num / busy if busy else 0.0,
                       "busy": busy / sum(r["secs"] for r in top_runs)}

    # --- write everything
    stamp = time.strftime("%Y%m%d-%H%M%S") + (f"-{args.label}" if args.label else "")
    out = ROOT / "bench_out" / stamp
    out.mkdir(parents=True)
    threads = {c: sum(workers[a]["threads"] for a in configs[c].split(",")) for c in configs}
    with open(out / "raw.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["config", "repeat", "n", "seconds", "hashes_per_s", "hits"])
        for r in raw:
            w.writerow([names[r["config"]], r["repeat"], sizes[r["config"]], r["secs"], round(r["rate"]), r["hits"]])
    with open(out / "summary.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["config", "pcs", "threads", "n", "avg_seconds", *summary[top]])
        for c, s in summary.items():
            w.writerow([names[c], len(configs[c].split(",")), threads[c], sizes[c], round(statistics.fmean(r["secs"] for r in raw if r["config"] == c), 3), *(round(v, 3) for v in s.values())])
    with open(out / "tests.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(test_rows[0]))
        w.writeheader()
        w.writerows(test_rows)

    M = 1e6
    rows = [f"| {names[c]} | {len(configs[c].split(','))} | {threads[c]} | {sizes[c]:,} | **{v['median'] / M:,.2f}** "
            f"| ±{v['ci95'] / M:,.2f} | {v['cv_pct']:.2f}% | {v['min'] / M:,.2f} | {v['max'] / M:,.2f} "
            f"| {statistics.fmean(r['secs'] for r in raw if r['config'] == c):.2f} |"
            for c, v in summary.items()]
    trows = [f"| {t['test']} | {t['gain']:.2f}x | {t['p']:.4f} | {'**yes**' if t['significant'] else 'NO'} |"
             for t in test_rows]
    srows = [f"| {k} | {summary[c]['median'] / M:,.2f} | {sum(summary[a]['median'] for a in addrs[:k]) / M:,.2f} "
             f"| {summary[c]['median'] / summary[addrs[0]]['median']:.2f}x "
             f"| {summary[c]['median'] / sum(summary[a]['median'] for a in addrs[:k]):.1%} |"
             for k, c in enumerate(scale, 1)]
    crows = [f"| {names[a]} | {summary[a]['median'] / M:,.2f} | {combined[a]['rate'] / M:,.2f} "
             f"| {combined[a]['share']:.1%} | {combined[a]['busy']:.1%} |" for a in addrs]
    job = int(args.job)
    jrows = [f"| {names[c]} | {fmt_time(job / summary[c]['median'])} |" for c in scale]
    ttf = [f"| {k} | {16 ** k:,} | {fmt_time(16 ** k / summary[addrs[0]]['median'])} "
           f"| {fmt_time(16 ** k / summary[top]['median'])} |" for k in range(6, 17)]
    s, lo, hi = speedup
    verdict = (f"**Yes.** Every added PC raised throughput significantly (one-sided permutation test, p < {ALPHA}). "
               f"All {len(addrs)} PCs reach **{s:.2f}x** the head alone (95% CI {lo:.2f}-{hi:.2f}x) "
               f"at {eff:.1%} parallel efficiency."
               if all_sig else
               f"**Not proven.** At least one step below was not significant at p < {ALPHA}; see the table. "
               f"Speedup all PCs / head alone: {s:.2f}x (95% CI {lo:.2f}-{hi:.2f}x).")
    consistency = ("All configs ran within the CV limit." if not noisy else
                   f"**Noisy:** {', '.join(noisy)} above {args.max_cv:g}% CV. Close other programs, keep laptops "
                   f"plugged in, and rerun with more --seconds or --repeats.")
    report = f"""# SHA-256 cluster benchmark{f' ({args.label})' if args.label else ''}

{time.strftime('%Y-%m-%d %H:%M')}. Engine `ck`, prefix `{args.prefix}`, {args.repeats} repeats per config of
~{args.seconds:g} s each, run round-robin after one warm-up.

## Does adding PCs help?

{verdict}

| Test | Throughput gain | p-value | Significant (p < {ALPHA}) |
|---|---|---|---|
{chr(10).join(trows)}

## Consistency

Every repeat of every config found exactly the same hits (correctness check). {consistency}

| Config | PCs | Threads | n per run | Median (M/s) | 95% CI | CV | Min | Max | Avg time (s) |
|---|---|---|---|---|---|---|---|---|---|
{chr(10).join(rows)}

## Scaling

| PCs | Measured (M/s) | Ideal: sum alone (M/s) | Speedup vs head | Efficiency |
|---|---|---|---|---|
{chr(10).join(srows)}

## Each PC inside the {len(addrs)}-PC run

| PC | Alone (M/s) | While computing in the {len(addrs)}-PC run (M/s) | Share of numbers | Busy |
|---|---|---|---|---|
{chr(10).join(crows)}

Busy below 100% means the PC waited on the network or on the tail of the job.

## What it means for a real job

Hashing {job:,} numbers:

| PCs | Time |
|---|---|
{chr(10).join(jrows)}

Expected time to find one hash with k leading zeros:

| k | Expected hashes (16^k) | Head alone | All {len(addrs)} PCs |
|---|---|---|---|
{chr(10).join(ttf)}

## Method

- Each config is sized to run ~{args.seconds:g} s and hashes the same numbers in every repeat.
- Repeats are interleaved round-robin, so slow drift (heat, background load) affects every config equally.
- Throughput = numbers / seconds as timed by `ck run` from job submit to last result.
- Significance: exact one-sided permutation test on the repeat throughputs (no normality assumption).
- Speedup CI: 5000 bootstrap resamples of the ratio of medians.
- "Busy" and "while computing" come from the compute time each worker reports per chunk.

![charts](bench.png)
"""
    (out / "report.md").write_text(report)
    plot(out, args.label, names, addrs, scale, summary, raw, combined, speedup)

    print(f"\n{'config':<16}{'median M/s':>12}{'±95% CI':>10}{'CV':>8}{'avg s':>10}{'median s':>10}{'total s':>9}")
    for c, v in summary.items():
        secs = [r["secs"] for r in raw if r["config"] == c]
        print(f"{names[c]:<16}{v['median'] / M:>12,.2f}{v['ci95'] / M:>10,.2f}{v['cv_pct']:>7.2f}%"
              f"{statistics.fmean(secs):>10.2f}{statistics.median(secs):>10.2f}{sum(secs):>9.2f}")
    print()
    for t in test_rows:
        print(f"{t['test']:<40} {t['gain']:5.2f}x  p = {t['p']:.4f}  {'significant' if t['significant'] else 'NOT significant'}")
    print(f"\nSpeedup all PCs / head alone: {s:.2f}x (95% CI {lo:.2f}-{hi:.2f}), efficiency {eff:.1%}")
    print("VERDICT: adding PCs helps (significant)" if all_sig else "VERDICT: not proven, see report")
    if noisy:
        print(f"WARNING: noisy configs (CV > {args.max_cv:g}%): {', '.join(noisy)}")
    print(f"Saved: {out.relative_to(ROOT)}/ (report.md, summary.csv, tests.csv, raw.csv, bench.png)")
    print(f"Total benchmark time: {time.perf_counter() - t_all:.1f} s")


if __name__ == "__main__":
    main()
