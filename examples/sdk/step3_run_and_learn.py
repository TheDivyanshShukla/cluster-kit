"""Step 3: run on the cluster, see where every millisecond went, and watch the SDK learn.  python3 examples/sdk/step3_run_and_learn.py

Runs the same map five times, forced onto the cluster (local="never") so the steps are visible even on a one-machine
test bed. After each run it prints the time per step (encode, submit, queue, run, detect, fetch, decode) and the
next plan's reasons: after 3 runs the plan says "overheads learned from N earlier runs" and uses measured numbers
instead of defaults. History lives in ~/.clusterkit/history.jsonl (CK_SDK_HOME moves it).
"""
from __future__ import annotations

import math
import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import clusterkit as ck


def work(x: int) -> float:
    return sum(math.sin(x * i) for i in range(20_000))  # ~5 ms


STEPS = ("encode_s", "ship_s", "submit_s", "queue_s", "run_s", "detect_s", "fetch_s", "decode_s")

if __name__ == "__main__":
    ck.connect()
    items = list(range(800))
    t0 = time.perf_counter()
    ref = [work(x) for x in items]
    print(f"plain Python loop on this PC: {time.perf_counter() - t0:.2f} s\n")
    for run in range(1, 6):
        p = ck.plan(work, items, local="never")
        t0 = time.perf_counter()
        out = ck.map(work, items, local="never")
        wall = time.perf_counter() - t0
        st = ck._last.get("st", {})  # internal: the step timings of the latest cluster run (what `python -m clusterkit bench` prints)
        steps = "  ".join(f"{k[:-2]} {st[k]:.3f}" for k in STEPS if isinstance(st.get(k), (int, float)))
        print(f"run {run}: {wall:.2f} s on {p.pcs} PCs, batch {p.batch}, same result: {out == ref}")
        print(f"   steps (s): {steps}")
        print(f"   plan said: {p.cluster_s:.2f} s; learned: {p.learned}")
    print("\nlast plan:", ck.plan(work, items))
