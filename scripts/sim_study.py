#!/usr/bin/env python3
"""Simulation study for the Cluster Kit paper: validation (SIM-04) first, then E1, E3, E4, E5, placement, eco, E7.

    cargo build --release --no-default-features   (in rust/, CARGO_TARGET_DIR=target-g2 here)
    python3 scripts/sim_study.py [--only validate,e1,e3,e4,e5,place,eco,e7] [--bin PATH] [--quick]

Writes CSVs, PNG + PDF figures, VALIDATION.md and results.json into docs/sim/. If the validation fails, the study is not run.
Every number that is an assumption (power, wake latency, MTBF, timetable, speed spread) is a parameter below and is
labelled in docs/sim/REPORT.md. Only stdlib + matplotlib.
"""
from __future__ import annotations

import argparse, csv, json, math, os, statistics as st, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any, Callable, Iterable, TypeVar

T = TypeVar("T")
U = TypeVar("U")
Scn = dict[str, Any]  # a ck sim scenario (see rust/src/sim.rs header)

R = Path(__file__).resolve().parents[1]
OUT = R / "docs" / "sim"
PAPER = R / "docs" / "paper" / "data"
ap = argparse.ArgumentParser()
ap.add_argument("--only", default="validate,e1,e3,e4,e5,place,eco,e7")
ap.add_argument("--bin", default=os.environ.get("CK_BIN") or str(R / "rust" / "target-g2" / "release" / "ck"))
ap.add_argument("--quick", action="store_true", help="fewer repeats and sizes (smoke test)")
A = ap.parse_args()
ONLY = set(A.only.split(","))
OUT.mkdir(parents=True, exist_ok=True)
TMP = tempfile.mkdtemp(prefix="simstudy-")
RESULTS = {}
POOL = ThreadPoolExecutor(max_workers=os.cpu_count() or 4)

# ---------------------------------------------------------------- parameters (see REPORT.md for the source of each)
HEAD_RATE, WORKER_RATE = 20.92e6, 23.34e6          # paper: SHA-256 hashes/s at 12 threads (mean of 7), head PC runs the head too
HEAD_RUN_CV, WORKER_RUN_CV = 0.0478, 0.0055          # paper: CV of the repeats (head PC 4.78 %, workers 0.2-0.8 %)
SVC_US, SVC_AGENT_US = 379.0, 0.199                  # calibrated on the two measured head points (VALIDATION.md)
LAB_SPEED_CV = 0.03                                  # ASSUMPTION: spread of PC speed in a real, mixed lab (the paper's 3 PCs are identical)
POWER = {"idle_w": 25.0, "busy_w": 65.0, "sleep_w": 2.0, "boot_w": 65.0, "interactive_w": 45.0, "wake_s": 45.0}  # ASSUMPTIONS
ECO = {"enabled": True, "idle_sleep_s": 600.0, "prewake_s": 120.0, "keep_awake": 0}                                # ASSUMPTIONS
EFF = 0.97                                           # job-level efficiency (chunk dispatch, tail): from E1 at 200 PCs, fixed 30 s jobs
SEEDS = 3 if A.quick else 7


def run(scn: Scn, name: str = "x") -> tuple[dict[str, Any], Path]:
    """Run `ck sim` on a scenario dict, return (summary dict, timeline path)."""
    d = Path(TMP) / name
    d.mkdir(parents=True, exist_ok=True)
    f = d / "scn.json"
    f.write_text(json.dumps(scn))
    r = subprocess.run([A.bin, "sim", str(f), "--out", str(d)], capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"ck sim failed: {r.stderr or r.stdout}")
    return json.loads((d / "summary.json").read_text()), d / "timeline.csv"


def pmap(fn: Callable[[T], U], items: Iterable[T]) -> list[U]:
    return list(POOL.map(fn, items))


def lab_nodes(p: int, speed_cv: float = LAB_SPEED_CV, **kw: Any) -> list[dict[str, Any]]:
    """p PCs: the head PC (slower, it also runs the head) + p-1 workers."""
    g = [{"count": 1, "rate": HEAD_RATE, "run_cv": HEAD_RUN_CV, "lab": "L0"}]
    if p > 1:
        g.append({"count": p - 1, "rate": WORKER_RATE, "speed_cv": speed_cv, "run_cv": WORKER_RUN_CV, "lab": "L0", **kw})
    return g


def nominal_rate(p: int) -> float:
    return HEAD_RATE + (p - 1) * WORKER_RATE


def csv_write(name: str, rows: Iterable[Iterable[Any]], header: list[str]) -> None:
    with open(OUT / name, "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(header)
        for r in rows:
            w.writerow([f"{x:.6g}" if isinstance(x, float) else x for x in r])


def mean(x: list[float]) -> float:
    return sum(x) / len(x)


def cvp(x: list[float]) -> float:
    return 100 * st.stdev(x) / mean(x) if len(x) > 1 and mean(x) else 0.0


import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

C = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#E69F00", "#56B4E9", "#555555"]
plt.rcParams.update({"font.size": 10, "axes.spines.top": False, "axes.spines.right": False, "axes.grid": True, "grid.alpha": 0.25,
                     "figure.dpi": 110, "savefig.bbox": "tight", "axes.prop_cycle": matplotlib.cycler(color=C)})


def save(fig: Any, name: str) -> None:
    fig.savefig(OUT / f"{name}.png", dpi=130)
    fig.savefig(OUT / f"{name}.pdf")
    plt.close(fig)


# ---------------------------------------------------------------- SIM-04 validation
def read_paper() -> dict[str, list[tuple[float, float]]]:
    raw = {}
    for r in csv.DictReader(open(PAPER / "raw.csv")):
        raw.setdefault(r["config"], []).append((float(r["hashes_per_s"]), float(r["n"])))
    return raw


def validate() -> bool:
    raw = read_paper()
    single = {k: mean([x[0] for x in v]) for k, v in raw.items() if k in ("head", "192.168.10.200", "192.168.10.3", "192.168.10.5")}
    rows, ok = [], True

    def add(what: str, measured: float, simulated: float, unit: str, tol: float = 10.0, kind: str = "err") -> None:
        nonlocal ok
        err = 100 * (simulated - measured) / measured
        good = abs(err) <= tol
        ok &= good if kind == "err" else True
        rows.append((what, measured, simulated, unit, err, ("pass" if good else "FAIL") if kind == "err" else "info"))

    # (1) the paper's 4-PC benchmark: head PC + 1..3 workers, fixed chunks (total / 16 p), 7 repeats, n as in the paper
    ns = {2: 130e6, 3: 200e6, 4: 270e6}
    for p, n in ns.items():
        meas = [x[0] for x in raw[f"{p} PCs"]]
        nominal = single["head"] + (p - 1) * mean([single[k] for k in ("192.168.10.200", "192.168.10.3", "192.168.10.5")])
        nodes = [{"count": 1, "rate": single["head"], "run_cv": HEAD_RUN_CV}, {"count": p - 1, "rate": WORKER_RATE, "speed_cv": 0.001, "run_cv": WORKER_RUN_CV}]
        def one(seed: int) -> Any:
            s, _ = run({"nodes": nodes, "job": {"n": n, "policy": "fixed"}, "seed": seed}, f"val{p}_{seed}")
            return s["throughput_per_s"]
        sim = pmap(one, range(1, 61))
        add(f"{p} PCs throughput (mean of runs)", mean(meas) / 1e6, mean(sim) / 1e6, "MH/s")
        add(f"{p} PCs efficiency vs sum of single PCs", 100 * mean(meas) / nominal, 100 * mean(sim) / nominal, "%")
        add(f"{p} PCs CV of the repeats", cvp(meas), cvp(sim), "%", kind="info")
    # one PC: the single-PC rate is the input, so only the check that one node costs nothing
    s, _ = run({"nodes": [{"count": 1, "rate": single["192.168.10.200"]}], "job": {"n": 70e6, "policy": "fixed"}}, "val1")
    add("1 PC throughput", single["192.168.10.200"] / 1e6, s["throughput_per_s"] / 1e6, "MH/s")

    # (2) the local head points (STATE rows 7, 13, 14): saturated head, tiny tasks
    for n, lo, hi in ((200, 2100, 2670), (1000, 1560, 1900)):
        s, _ = run({"nodes": [{"count": n, "rate": 1e6}], "job": {"n": n * 5000 * 40, "policy": "fixed", "chunk": 5000}}, f"head{n}")
        mid = (lo + hi) / 2
        t = s["tasks_per_s"]
        rows.append((f"head ceiling, {n} agents (measured range {lo}-{hi})", mid, t, "tasks/s", 100 * (t - mid) / mid, "pass" if lo * 0.9 <= t <= hi * 1.1 else "FAIL"))
        ok &= lo * 0.9 <= t <= hi * 1.1
    s, _ = run({"nodes": [{"count": 200, "rate": 1e6}], "job": {"n": 2e9}}, "adapt200")
    add("adaptive 10 s-ideal job, 200 agents", 11.0, s["makespan_s"], "s")
    add("adaptive 10 s-ideal job, number of parts", 4919, s["tasks"], "parts", tol=10)

    md = ["# Simulator validation (SIM-04)", "",
          "The simulator (`ck sim`, rust/src/sim.rs, using the real `sched::chunk_len`) is compared with the measured points before any study number is used.",
          "Rule: throughput and efficiency of the 4-PC paper benchmark and the local head points within 10 %. CV rows are informational.", "",
          "| Point | Measured | Simulated | Unit | Error % | Result |", "| --- | ---: | ---: | --- | ---: | --- |"]
    for w, m, s_, u, e, g in rows:
        md.append(f"| {w} | {m:.4g} | {s_:.4g} | {u} | {e:+.1f} | {g} |")
    md += ["", f"**Overall: {'PASS - the study below may be used' if ok else 'FAIL - study numbers are NOT published'}**", "",
           "How the model is calibrated (the only fitted numbers):", "",
           f"- Head PC rate {HEAD_RATE/1e6:.2f} MH/s and worker rate {WORKER_RATE/1e6:.2f} MH/s are the means of the paper's seven single-PC runs; run-to-run CV is 4.78 % for the head PC and 0.55 % for workers (raw.csv). The 2-4 PC results are predictions: nothing is fitted to them.",
           f"- Head service time per task = {SVC_US:.0f} us + {SVC_AGENT_US} us x connected agents. Two measured saturated points (200 agents: 2,100-2,670 tasks/s; 1,000 agents after the fix: 1,560-1,900 tasks/s) fix the two numbers (mid-points 2,385 and 1,730 tasks/s give 419 us and 578 us).",
           "- Not validated here (inputs, taken from measurements): standby promotion 5.7 s, planned switchover 3.7 s. The measured 'job done 11.5 s after the kill' (10 virtual agents) is 5.7 s promotion plus the work that remained, so it is consistent with, not independent of, the input.",
           "- Not validated (needs the lab): anything above 200 agents with real PCs, per-PC speed spread of a mixed lab, power and wake latency, failure rates.",
           "- Extrapolation: the per-agent head cost is a straight line through two points; beyond 1,000 agents on one head (the 10,000 case) it is an extrapolation."]
    (OUT / "VALIDATION.md").write_text("\n".join(md) + "\n")
    csv_write("validation.csv", rows, ["point", "measured", "simulated", "unit", "error_pct", "result"])
    RESULTS["validation"] = {"pass": ok, "rows": [dict(zip(["point", "measured", "simulated", "unit", "error_pct", "result"], r)) for r in rows]}
    print("validation:", "PASS" if ok else "FAIL")
    for w, m, s_, u, e, g in rows:
        print(f"  {w}: measured {m:.4g} sim {s_:.4g} {u} err {e:+.1f}% {g}")
    return ok


# ---------------------------------------------------------------- E1 throughput and efficiency 1..200 PCs
def e1() -> None:
    ps = [1, 2, 3, 4, 8, 16, 32, 64, 100, 150, 200] if not A.quick else [1, 4, 16, 64, 200]
    variants = [("fixed", 70e6, "fixed, ~3 s per PC (the paper)"), ("adaptive", 70e6, "adaptive, ~3 s per PC"), ("fixed", 700e6, "fixed, ~30 s per PC")]
    jobs = [(v, p, r) for v in variants for p in ps for r in range(1, SEEDS + 1)]

    def one(a: Any) -> Any:
        (pol, npc, _), p, r = a
        s, _t = run({"nodes": lab_nodes(p), "job": {"n": npc * p, "policy": pol}, "head": {"service_us": SVC_US, "service_us_per_agent": SVC_AGENT_US}, "seed": 100 * p + r}, f"e1_{pol}_{int(npc)}_{p}_{r}")
        return (pol, int(npc), p, r, s["throughput_per_s"], 100 * s["throughput_per_s"] / nominal_rate(p), s["makespan_s"], s["tasks_per_s"], s["head_utilisation_pct"])
    rows = pmap(one, jobs)
    csv_write("e1_scaling.csv", rows, ["policy", "n_per_pc", "pcs", "repeat", "throughput_per_s", "efficiency_pct", "makespan_s", "tasks_per_s", "head_util_pct"])
    fig, ax = plt.subplots(1, 2, figsize=(10, 3.8))
    summ = {}
    for i, (pol, npc, lab) in enumerate(variants):
        xs, med, lo, hi, em, elo, ehi = [], [], [], [], [], [], []
        for p in ps:
            sel = [r for r in rows if r[0] == pol and r[1] == int(npc) and r[2] == p]
            th = sorted(r[4] / 1e6 for r in sel); ef = sorted(r[5] for r in sel)
            xs.append(p); med.append(st.median(th)); lo.append(th[0]); hi.append(th[-1]); em.append(st.median(ef)); elo.append(ef[0]); ehi.append(ef[-1])
            summ[f"{pol}_{int(npc)}_{p}"] = {"thr_mhs": st.median(th), "eff_pct": st.median(ef), "head_util_pct": mean([r[8] for r in sel])}
        ax[0].errorbar(xs, med, yerr=[[m - l for m, l in zip(med, lo)], [h - m for m, h in zip(med, hi)]], marker="o", ms=4, lw=1.5, capsize=2, label=lab, color=C[i])
        ax[1].errorbar(xs, em, yerr=[[m - l for m, l in zip(em, elo)], [h - m for m, h in zip(em, ehi)]], marker="o", ms=4, lw=1.5, capsize=2, label=lab, color=C[i])
    ax[0].plot(ps, [nominal_rate(p) / 1e6 for p in ps], "k--", lw=1, label="ideal (sum of single PCs)")
    ax[0].set_xscale("log", base=2); ax[0].set_yscale("log"); ax[0].set_xlabel("PCs"); ax[0].set_ylabel("throughput (MH/s SHA-256)"); ax[0].legend(fontsize=8); ax[0].set_title("E1 throughput, median and range of %d runs" % SEEDS)
    ax[1].axhline(100, color="k", ls="--", lw=1); ax[1].set_xscale("log", base=2); ax[1].set_xlabel("PCs"); ax[1].set_ylabel("efficiency (% of ideal)"); ax[1].set_ylim(60, 105); ax[1].set_title("E1 efficiency")
    save(fig, "fig_e1_scaling")
    RESULTS["e1"] = summ


# ---------------------------------------------------------------- E3 cost of losing k of N PCs mid-job
def e3() -> None:
    out = {}
    fig, ax = plt.subplots(1, 2, figsize=(10, 3.8))
    for ci, N in enumerate([20, 200]):
        ks = [0, 1, 2, 5, 10, 20, 50, 100] if N == 200 else [0, 1, 2, 4, 8, 10]
        n = N * WORKER_RATE * 600
        jobs = [(k, perm, r) for k in ks for perm in (True, False) for r in range(1, SEEDS + 1)]

        def one(a: Any) -> Any:
            k, perm, r = a
            g = lab_nodes(N - k)
            if k:
                g.append({"count": k, "rate": WORKER_RATE, "speed_cv": LAB_SPEED_CV, "run_cv": WORKER_RUN_CV, "lab": "L0", "fail_at_s": 300.0, "down_s": 1e9 if perm else 180.0})
            s, _t = run({"nodes": g, "job": {"n": n, "policy": "adaptive"}, "seed": 1000 * N + 10 * k + r, "max_s": 1e5}, f"e3_{N}_{k}_{perm}_{r}")
            return (N, k, "permanent" if perm else "reboot_180s", r, s["makespan_s"], s["ideal_s"], 100 * s["recovered_work"] / s["n"], s["node_failures"])
        rows = pmap(one, jobs)
        csv_write(f"e3_lose_k_of_{N}.csv", rows, ["N", "k", "kind", "repeat", "makespan_s", "ideal_s_all_PCs_up", "recomputed_pct_of_job", "failures"])
        for j, kind in enumerate(["permanent", "reboot_180s"]):
            xs = [100 * k / N for k in ks]
            base = mean([r[4] for r in rows if r[1] == 0 and r[2] == kind])
            ex = [100 * (mean([r[4] for r in rows if r[1] == k and r[2] == kind]) / base - 1) for k in ks]
            # ideal cost: lose k/N of the capacity for the remaining half
            lb = [100 * (0.5 * (1 / (1 - k / N) - 1) if k < N else 0) for k in ks]
            ax[0].plot(xs, ex, marker="o", ms=4, label=f"N={N}, {kind.replace('_', ' ')}", color=C[ci * 2 + j])
            if kind == "permanent":
                ax[0].plot(xs, lb, ls=":", color=C[ci * 2 + j], lw=1)
                ax[1].plot(xs, [mean([r[6] for r in rows if r[1] == k and r[2] == kind]) for k in ks], marker="o", ms=4, label=f"N={N}", color=C[ci])
        out[f"N{N}"] = {str(k): {"permanent_extra_pct": 100 * (mean([r[4] for r in rows if r[1] == k and r[2] == 'permanent']) / mean([r[4] for r in rows if r[1] == 0 and r[2] == 'permanent']) - 1),
                                 "reboot_extra_pct": 100 * (mean([r[4] for r in rows if r[1] == k and r[2] == 'reboot_180s']) / mean([r[4] for r in rows if r[1] == 0 and r[2] == 'reboot_180s']) - 1),
                                 "recomputed_pct": mean([r[6] for r in rows if r[1] == k and r[2] == 'permanent'])} for k in ks}
    ax[0].set_xlabel("PCs lost at 50 % of the job (% of N)"); ax[0].set_ylabel("extra completion time (%)"); ax[0].legend(fontsize=7); ax[0].set_title("E3 cost of losing PCs (dotted: capacity bound)")
    ax[1].set_xlabel("PCs lost (% of N)"); ax[1].set_ylabel("recomputed work (% of the job)"); ax[1].legend(fontsize=8); ax[1].set_title("E3 work redone (chunks in flight)")
    save(fig, "fig_e3_lose_k")
    RESULTS["e3"] = out


# ---------------------------------------------------------------- E4 head crash
def e4() -> None:
    cases = [("standby, crash", "standby", {"recovery_s": 5.7}), ("planned switchover", "standby", {"recovery_s": 3.7}),
             ("no standby, manual restart 5 min", "single", {"manual_restart_s": 300.0}), ("no standby, manual restart 30 min", "single", {"manual_restart_s": 1800.0}),
             ("no standby, never restarted", "single", {})]
    Ns = [20, 200, 1000]
    jobs = [(N, c, r) for N in Ns for c in cases for r in range(1, SEEDS + 1)]
    def one(a: Any) -> Any:
        N, (name, arch, hf), r = a
        n = N * WORKER_RATE * 600
        scn = {"nodes": lab_nodes(N), "job": {"n": n, "policy": "adaptive"}, "architecture": arch, "seed": 7 * N + r, "head": {"service_us": SVC_US, "service_us_per_agent": SVC_AGENT_US}, "max_s": 1e5}
        s0, _t = run(scn, f"e4b_{N}_{name}_{r}")
        scn["head_failure"] = {"at_s": 300.0, **hf}
        s, _t = run(scn, f"e4_{N}_{name}_{r}")
        fin = s["finished"]
        delay = s["makespan_s"] - s0["makespan_s"] if fin else float("nan")
        return (N, name, r, 1 if fin else 0, delay, delay * (s0["n"] / s0["makespan_s"]) / 1e9 if fin else float("nan"), 100 * s["done"] / s["n"], s["recovered_work"], 0)
    rows = pmap(one, jobs)
    csv_write("e4_head_crash.csv", rows, ["N", "case", "repeat", "finished", "delay_s", "idle_capacity_Ghash", "done_pct_at_end", "recomputed_units", "duplicate_units"])
    fig, ax = plt.subplots(1, 2, figsize=(10.5, 3.9))
    w = 0.16
    summ = {}
    for ci, (name, _a, _h) in enumerate(cases[:4]):
        ys = [mean([r[4] for r in rows if r[0] == N and r[1] == name]) for N in Ns]
        ax[0].bar([i + ci * w for i in range(len(Ns))], ys, w, label=name, color=C[ci])
        summ[name] = {str(N): y for N, y in zip(Ns, ys)}
    ax[0].set_yscale("log"); ax[0].set_xticks([i + 1.5 * w for i in range(len(Ns))]); ax[0].set_xticklabels([f"{N} PCs" for N in Ns]); ax[0].set_ylabel("completion delay (s, log)")
    ax[0].axhline(15, color="k", ls="--", lw=1); ax[0].text(0, 17, "target < 15 s", fontsize=8); ax[0].legend(fontsize=7, loc="upper center", bbox_to_anchor=(0.5, -0.12), ncol=2); ax[0].set_title("E4 delay after a head crash at 50 % of a 10 min job")
    never = {N: mean([r[6] for r in rows if r[0] == N and r[1] == cases[4][0]]) for N in Ns}
    summ["never_restarted_done_pct"] = never
    # sensitivity to the promotion time
    recs = [2, 5.7, 10, 20, 30, 60]
    def sens(rec: Any) -> Any:
        N = 200
        scn = {"nodes": lab_nodes(N), "job": {"n": N * WORKER_RATE * 600}, "architecture": "standby", "seed": 5}
        s0, _t = run(scn, f"e4s0_{rec}")
        scn["head_failure"] = {"at_s": 300.0, "recovery_s": rec}
        s, _t = run(scn, f"e4s_{rec}")
        return s["makespan_s"] - s0["makespan_s"]
    ds = pmap(sens, recs)
    ax[1].plot(recs, ds, marker="o", color=C[0]); ax[1].plot(recs, recs, "k:", lw=1, label="delay = outage")
    ax[1].set_xlabel("standby promotion time (s)"); ax[1].set_ylabel("completion delay (s)"); ax[1].legend(fontsize=8); ax[1].set_title("E4 sensitivity (200 PCs)")
    summ["promotion_sensitivity_200pcs"] = dict(zip(map(str, recs), ds))
    save(fig, "fig_e4_head_crash")
    RESULTS["e4"] = summ


# ---------------------------------------------------------------- E5 architectures
def e5() -> None:
    archs = ["single", "standby", "relays", "relays+standby"]
    Ns = [200, 1000, 10000] if not A.quick else [200, 1000]
    jobs = [(N, a, r) for N in Ns for a in archs for r in range(1, (3 if N < 10000 else 2) + 1)]
    def one(x: Any) -> Any:
        N, a, r = x
        lab = 50
        g = [{"count": min(lab, N - i * lab), "rate": WORKER_RATE, "speed_cv": LAB_SPEED_CV, "run_cv": WORKER_RUN_CV, "lab": f"L{i}"} for i in range((N + lab - 1) // lab)]
        n = sum(x["count"] for x in g) * WORKER_RATE * 600
        scn = {"nodes": g, "job": {"n": n, "policy": "adaptive"}, "architecture": a, "relays": {"per_lab": True, "batch": 20}, "seed": r, "max_s": 2e5, "sample_s": 5,
               "head": {"service_us": SVC_US, "service_us_per_agent": SVC_AGENT_US}}
        s, _t = run(scn, f"e5_{N}_{a}_{r}")
        scn["head_failure"] = {"at_s": 300.0}
        sf, _t = run(scn, f"e5f_{N}_{a}_{r}")
        return (N, a, r, s["makespan_s"], s["efficiency_pct"], s["head_utilisation_pct"], s["tasks_per_s"], (sf["makespan_s"] - s["makespan_s"]) if sf["finished"] else float("nan"))
    rows = pmap(one, jobs)
    csv_write("e5_architectures.csv", rows, ["N", "architecture", "repeat", "makespan_s", "efficiency_pct", "head_util_pct", "tasks_per_s", "delay_if_head_crashes_at_300s"])
    fig, ax = plt.subplots(1, 2, figsize=(10.5, 3.9))
    w = 0.2
    summ = {}
    for ai, a in enumerate(archs):
        eff = [mean([r[4] for r in rows if r[0] == N and r[1] == a]) for N in Ns]
        hu = [mean([r[5] for r in rows if r[0] == N and r[1] == a]) for N in Ns]
        ax[0].bar([i + ai * w for i in range(len(Ns))], eff, w, label=a, color=C[ai])
        ax[1].bar([i + ai * w for i in range(len(Ns))], hu, w, label=a, color=C[ai])
        for N, e_, h_ in zip(Ns, eff, hu):
            summ[f"{a}_{N}"] = {"eff_pct": e_, "makespan_s": mean([r[3] for r in rows if r[0] == N and r[1] == a]), "head_util_pct": h_,
                                "crash_delay_s": mean([r[7] for r in rows if r[0] == N and r[1] == a])}
    for k in (0, 1):
        ax[k].set_xticks([i + 1.5 * w for i in range(len(Ns))]); ax[k].set_xticklabels([f"{N:,} PCs" for N in Ns])
    ax[0].set_ylabel("efficiency (% of ideal 600 s)"); ax[0].set_title("E5 efficiency by architecture"); ax[0].set_ylim(0, 130); ax[0].legend(fontsize=8, ncol=4, loc="upper center")
    ax[1].set_ylabel("head busy (%)"); ax[1].set_title("E5 head utilisation")
    save(fig, "fig_e5_architectures")
    RESULTS["e5"] = summ


# ---------------------------------------------------------------- day scenarios
HOUR_RES = [  # ASSUMED weekday timetable: (start_h, end_h, lab index, PCs of the lab used)
    (9, 11, 0, 50), (14, 16, 0, 40), (10, 12, 1, 50), (13, 15, 1, 45), (9, 12, 2, 50), (11, 13, 3, 50), (15, 17, 3, 35)]
LAB_RATES = [18e6, 23.34e6, 23.34e6, 28e6]  # ASSUMED four labs of 50 PCs: an old, two typical, a newer one
UNIT = 200 * 23.34e6 * 600  # work that keeps the whole 200-PC lab busy for 10 minutes
USER_JOBS = [  # ASSUMED user jobs: (name, arrive_h, work in "10-minute whole-lab" units)
    ("u1", 11.0, 1.0), ("u2", 13.5, 0.5), ("u3", 15.0, 2.0), ("u4", 19.5, 0.2), ("u5", 2.0, 0.4)]


def day_scn(placement: str = "spread", max_pcs: int = 0, pow_job: bool = False, eco: bool = True, power: dict[str, float] | None = None,
            ecocfg: dict[str, Any] | None = None, seed: int = 1, mtbf: float = 0.0, head_mtbf: float = 0.0, classes: bool = True,
            user: bool = True, hours: float = 24, wake_s: float | None = None, tick: float = 10) -> Scn:
    pcs = [{"count": 50, "rate": LAB_RATES[i], "speed_cv": LAB_SPEED_CV, "run_cv": WORKER_RUN_CV, "mtbf_s": mtbf, "down_s": 180.0, "threads": 12} for i in range(4)]
    pw = dict(POWER, **(power or {}))
    if wake_s:
        pw["wake_s"] = wake_s
    jobs = [{"name": n, "arrive_h": h, "work": w * UNIT, "priority": 2, "max_pcs": max_pcs, "placement": placement} for n, h, w in USER_JOBS] if user else []
    if pow_job:
        jobs.append({"name": "pow", "arrive_h": 0, "work": 0, "priority": 4, "max_pcs": 0, "placement": "spread"})
    res = [{"start_h": a, "end_h": b, "first_pc": 50 * lab, "pcs": c} for a, b, lab, c in HOUR_RES] if classes else []
    return {"day": {"hours": hours, "tick_s": tick, "sample_s": 300, "seed": seed, "efficiency": EFF, "chunk_s": 1.0}, "pcs": pcs, "power": pw,
            "eco": dict(ECO, **(ecocfg or {}), enabled=eco), "reservations": res, "jobs": jobs, "head": {"mtbf_s": head_mtbf, "standby": True, "recovery_s": 5.7}}


def user_stats(s: dict[str, Any]) -> tuple[float, float, float, int]:  # mean makespan, max, mean wait, unfinished
    us = [j for j in s["jobs"] if j["name"].startswith("u")]
    mk = [j["makespan_s"] for j in us if j["makespan_s"] is not None]
    return (mean(mk) if mk else float("nan"), max(mk) if mk else float("nan"), mean([j["wait_s"] for j in us if j["wait_s"] is not None] or [float("nan")]), sum(1 for j in us if j["makespan_s"] is None))


def place() -> None:
    modes = ["spread", "pack", "fastest", "efficient"]
    caps = [0, 100, 50, 20]
    jobs = [(m, c, pw, r) for m in modes for c in caps for pw in (False, True) for r in range(1, SEEDS + 1 if not A.quick else 2)]
    def one(a: Any) -> Any:
        m, c, pw, r = a
        s, _t = run(day_scn(placement=m, max_pcs=c, pow_job=pw, eco=not pw, seed=r), f"pl_{m}_{c}_{pw}_{r}")
        mk, mx, wait, unf = user_stats(s)
        powdone = next((j["done"] for j in s["jobs"] if j["name"] == "pow"), 0.0)
        return (m, c, int(pw), r, mk, mx, wait, unf, s["kwh"], powdone / 1e12, s["wakes"], s["class_delay_pc_s"])
    rows = pmap(one, jobs)
    csv_write("placement_modes.csv", rows, ["placement", "max_pcs", "with_pow_background", "repeat", "user_mean_makespan_s", "user_max_makespan_s", "user_mean_wait_s", "unfinished", "kwh_day", "pow_Thash", "wakes", "class_delay_pc_s"])
    def cell(m: Any, c: Any, pw: Any, i: Any) -> Any:
        return mean([r[i] for r in rows if r[0] == m and r[1] == c and r[2] == pw])
    fig, ax = plt.subplots(1, 3, figsize=(14, 4), constrained_layout=True)
    for k, (i, ttl, fmt) in enumerate([(4, "user job mean makespan (s)\nno background job, eco on", "{:.0f}"), (8, "energy of the day (kWh)\nno background job, eco on", "{:.1f}")]):
        grid = [[cell(m, c, 0, i) for c in caps] for m in modes]
        im = ax[k].imshow(grid, cmap="viridis_r", aspect="auto")
        ax[k].set_xticks(range(len(caps))); ax[k].set_xticklabels(["no limit" if c == 0 else str(c) for c in caps]); ax[k].set_yticks(range(len(modes))); ax[k].set_yticklabels(modes if k == 0 else [])
        ax[k].set_xlabel("max_pcs"); ax[k].set_title(ttl, fontsize=9); ax[k].grid(False)
        lo, hi = min(map(min, grid)), max(map(max, grid))
        for y in range(len(modes)):
            for x in range(len(caps)):
                ax[k].text(x, y, fmt.format(grid[y][x]), ha="center", va="center", color="white" if (grid[y][x] - lo) / (hi - lo + 1e-12) < 0.5 else "black", fontsize=8)
        fig.colorbar(im, ax=ax[k], fraction=0.046)
    for mi, m in enumerate(modes):
        for c in caps:
            ax[2].scatter(cell(m, c, 0, 4), cell(m, c, 0, 8), s=30 + (60 if c == 0 else 100 - c) , color=C[mi], alpha=0.75, label=m if c == 0 else None, marker="osD^"[mi])
    ax[2].set_xscale("log"); ax[2].set_xticks([1200, 2000, 3000, 5000]); ax[2].set_xticklabels(["1200", "2000", "3000", "5000"]); ax[2].minorticks_off(); ax[2].set_xlabel("user job mean makespan (s, log)"); ax[2].set_ylabel("energy of the day (kWh)"); ax[2].legend(fontsize=8); ax[2].set_title("Trade-off: faster jobs vs energy (bigger = fewer PCs)", fontsize=9)
    save(fig, "fig_place_modes")
    RESULTS["place"] = {f"{m}_{c}_{'pow' if pw else 'nopow'}": {"user_mean_makespan_s": cell(m, c, pw, 4), "user_max_makespan_s": cell(m, c, pw, 5), "kwh": cell(m, c, pw, 8), "pow_Thash": cell(m, c, pw, 9), "wakes": cell(m, c, pw, 10)} for m in modes for c in caps for pw in (0, 1)}


def eco() -> None:
    cfgs = [("always on", dict(eco=False)), ("eco (idle 10 min, prewake 2 min)", dict(eco=True))]
    rows, tls = [], {}
    for name, kw in cfgs:
        for r in range(1, SEEDS + 1):
            s, tl = run(day_scn(seed=r, **kw), f"eco_{name[:3]}_{r}")
            mk, mx, wait, unf = user_stats(s)
            rows.append((name, r, s["kwh"], s["kwh_busy"], s["kwh_idle"], s["kwh_sleep"], s["kwh_boot"], s["kwh_class"], s["wakes"], s["class_delay_pc_s"], s["class_delay_max_s"], mk, wait))
            if r == 1:
                tls[name] = list(csv.DictReader(open(tl)))
    csv_write("eco_day.csv", rows, ["case", "repeat", "kwh", "kwh_busy", "kwh_idle", "kwh_sleep", "kwh_boot", "kwh_class", "wakes", "class_delay_pc_s", "class_delay_max_s", "user_mean_makespan_s", "user_mean_wait_s"])
    on = mean([r[2] for r in rows if r[0] == "always on"]); ec = mean([r[2] for r in rows if r[0] != "always on"])
    # sensitivity: wake latency x idle timeout
    wakes, idles = [30, 60, 120], [300, 600, 1800, 3600]
    jobs = [(w, i) for w in wakes for i in idles]
    def sens(a: Any) -> Any:
        w, i = a
        s, _t = run(day_scn(seed=1, ecocfg={"idle_sleep_s": i}, wake_s=w), f"ecos_{w}_{i}")
        return 100 * (1 - s["kwh"] / on), mean([x["makespan_s"] for x in s["jobs"] if x["makespan_s"] is not None])
    sres = pmap(sens, jobs)
    csv_write("eco_sensitivity.csv", [(w, i, sv, mk) for (w, i), (sv, mk) in zip(jobs, sres)], ["wake_s", "idle_sleep_s", "saving_pct", "user_mean_makespan_s"])
    # no prewake: class delay
    s_np, _t = run(day_scn(seed=1, ecocfg={"prewake_s": 0.0}), "eco_noprewake")
    s_pw, _t = run(day_scn(seed=1), "eco_prewake")
    # power draw assumptions: saving vs the idle/sleep ratio
    sav_power = {}
    for idle in (15, 25, 40):
        a, _t = run(day_scn(seed=1, eco=False, power={"idle_w": idle}), f"ecop_a{idle}")
        b, _t = run(day_scn(seed=1, eco=True, power={"idle_w": idle}), f"ecop_b{idle}")
        sav_power[idle] = 100 * (1 - b["kwh"] / a["kwh"])
    fig, ax = plt.subplots(1, 3, figsize=(14, 3.9))
    for ci, (name, tl) in enumerate(tls.items()):
        ax[0].plot([float(x["t_s"]) / 3600 for x in tl], [float(x["power_w"]) / 1e3 for x in tl], label=name, color=C[ci], lw=1.5)
    ax[0].set_xlabel("hour of the day"); ax[0].set_ylabel("lab power (kW)"); ax[0].legend(fontsize=8); ax[0].set_title("Eco: lab power over a weekday (200 PCs)"); ax[0].set_xlim(0, 24)
    for t0, t1, lab, c in HOUR_RES:
        ax[0].axvspan(t0, t1, color="#999999", alpha=0.05)
    for k, w in enumerate(wakes):
        ax[1].plot([i / 60 for i in idles], [sres[k * len(idles) + j][0] for j in range(len(idles))], marker="o", label=f"wake {w} s", color=C[k])
    ax[1].set_xscale("log"); ax[1].set_xticks([5, 10, 30, 60]); ax[1].set_xticklabels(["5", "10", "30", "60"]); ax[1].minorticks_off(); ax[1].set_xlabel("idle time before sleep (min)"); ax[1].set_ylabel("energy saved vs always-on (%)"); ax[1].legend(fontsize=8); ax[1].set_title("Eco: saving, sensitivity")
    kinds = ["always on", "eco"]
    comp = ["kwh_busy", "kwh_class", "kwh_idle", "kwh_boot", "kwh_sleep"]
    names = ["jobs", "class sessions", "idle", "booting", "asleep"]
    bottom = [0, 0]
    for ci, (c, nm) in enumerate(zip(comp, names)):
        idx = 3 + ["kwh_busy", "kwh_idle", "kwh_sleep", "kwh_boot", "kwh_class"].index(c)
        ys = [mean([r[idx] for r in rows if (r[0] == "always on") == (k == "always on")]) for k in kinds]
        ax[2].bar(kinds, ys, bottom=bottom, label=nm, color=C[ci]); bottom = [b + y for b, y in zip(bottom, ys)]
    ax[2].set_ylabel("kWh per day"); ax[2].set_ylim(0, 165); ax[2].legend(fontsize=8); ax[2].set_title("Eco: where the energy goes")
    save(fig, "fig_eco_day")
    RESULTS["eco"] = {"always_on_kwh": on, "eco_kwh": ec, "saving_pct": 100 * (1 - ec / on), "wakes": mean([r[8] for r in rows if r[0] != "always on"]),
                      "user_mean_wait_s_eco": mean([r[12] for r in rows if r[0] != "always on"]), "user_mean_wait_s_on": mean([r[12] for r in rows if r[0] == "always on"]),
                      "class_delay_pc_s_with_prewake": s_pw["class_delay_pc_s"], "class_delay_pc_s_no_prewake": s_np["class_delay_pc_s"], "class_delay_max_s_no_prewake": s_np["class_delay_max_s"],
                      "sensitivity": {f"wake{w}_idle{i}": sv for (w, i), (sv, _m) in zip(jobs, sres)}, "saving_pct_by_idle_w": sav_power}


# ---------------------------------------------------------------- E7 24 h soak projection
def e7() -> None:
    mtbfs = [("6 h", 6 * 3600.0), ("1 day", 86400.0), ("3 days", 3 * 86400.0), ("7 days", 7 * 86400.0), ("30 days", 30 * 86400.0)]
    reps = 10 if not A.quick else 3
    jobs = [(nm, m, r) for nm, m in mtbfs for r in range(1, reps + 1)]
    def one(a: Any) -> Any:
        nm, m, r = a
        s, tl = run(day_scn(seed=r, mtbf=m, head_mtbf=7 * 86400.0, pow_job=True, eco=False, classes=False, user=False), f"e7_{nm}_{r}")
        rows = list(csv.DictReader(open(tl)))
        busy = [float(x["busy"]) for x in rows]
        pow_done = s["jobs"][0]["done"]
        return (nm, m / 86400, r, s["node_failures"] / 24, s["head_failures"], pow_done / 86400 / 1e6, min(busy), st.median(busy), max(busy), s["kwh"], pow_done / (s["kwh"] * 3.6e6) / 1e6, s["lost_work"] / 1e9)
    rows = pmap(one, jobs)
    csv_write("e7_soak.csv", rows, ["pc_mtbf", "pc_mtbf_days", "repeat", "pc_failures_per_hour", "head_failovers", "mean_MHs", "pcs_busy_min", "pcs_busy_median", "pcs_busy_max", "kwh", "MH_per_joule", "lost_Ghash"])
    summ = {}
    for nm, m in mtbfs:
        sel = [r for r in rows if r[0] == nm]
        summ[nm] = {"pc_failures_per_hour": mean([r[3] for r in sel]), "expected_per_hour": 200 / (m / 3600), "mean_MHs": mean([r[5] for r in sel]), "pcs_busy_median": mean([r[7] for r in sel]),
                    "pcs_busy_min": mean([r[6] for r in sel]), "kwh": mean([r[9] for r in sel]), "MH_per_joule": mean([r[10] for r in sel]), "head_failovers": mean([r[4] for r in sel]),
                    "loss_vs_30d_pct": 0.0}
    ref = summ["30 days"]["mean_MHs"]
    for v in summ.values():
        v["loss_vs_30d_pct"] = 100 * (1 - v["mean_MHs"] / ref)
    fig, ax = plt.subplots(1, 2, figsize=(10, 3.8))
    xs = [m / 86400 for _n, m in mtbfs]
    ax[0].plot(xs, [summ[n]["pc_failures_per_hour"] for n, _m in mtbfs], marker="o", label="simulated", color=C[0])
    ax[0].plot(xs, [summ[n]["expected_per_hour"] for n, _m in mtbfs], "k:", label="200 / MTBF")
    ax[0].set_xscale("log"); ax[0].set_yscale("log"); ax[0].set_xlabel("per-PC MTBF (days)  [assumption]"); ax[0].set_ylabel("PC failures per hour"); ax[0].legend(fontsize=8); ax[0].set_title("E7 failures per hour, 24 h, 200 PCs")
    ax[1].plot(xs, [summ[n]["loss_vs_30d_pct"] for n, _m in mtbfs], marker="o", color=C[1]); ax[1].set_ylim(bottom=0)
    ax[1].set_xscale("log"); ax[1].set_xlabel("per-PC MTBF (days)  [assumption]"); ax[1].set_ylabel("PoW work lost vs 30-day MTBF (%)"); ax[1].set_title("E7 work lost (3 min outage per failure)")
    save(fig, "fig_e7_soak")
    RESULTS["e7"] = summ


if __name__ == "__main__":
    if not Path(A.bin).exists():
        sys.exit(f"{A.bin} not found: build with  cd rust && CARGO_TARGET_DIR=target-g2 cargo build --release --no-default-features")
    ok = validate() if "validate" in ONLY else json.loads((OUT / "results.json").read_text())["validation"]["pass"]
    if not ok:
        print("validation failed: study not run")
    else:
        for name, fn in (("e1", e1), ("e3", e3), ("e4", e4), ("e5", e5), ("place", place), ("eco", eco), ("e7", e7)):
            if name in ONLY:
                fn()
                print("done", name, flush=True)
    old = {}
    if (OUT / "results.json").exists():
        try:
            old = json.loads((OUT / "results.json").read_text())
        except Exception:
            pass
    old.update(RESULTS)
    (OUT / "results.json").write_text(json.dumps(old, indent=1))
