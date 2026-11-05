"""Step 1: how the optimizer decides where work runs.  python3 examples/sdk/step1_plan.py

Three ordinary functions with very different cost per item. ck.plan() profiles each one on a few items, reads the
cluster (PCs, threads, free RAM) and prints its decision with the reasons. Nothing is submitted.
"""
from __future__ import annotations

import math

try:
    import _path  # noqa: F401  (repo checkout only: find the SDK without installing it)
except ImportError:
    pass
import clusterkit as ck


def tiny(x: int) -> int:
    """~1 microsecond: shipping it anywhere costs more than doing it."""
    return x * x


def medium(x: int) -> float:
    """~0.5 ms of pure-Python maths per item."""
    return sum(math.sin(x * i) for i in range(5_000))


def heavy(x: int) -> float:
    """~20 ms per item: the kind of work a cluster is for."""
    return sum(math.sin(x * i) * math.cos(i) for i in range(150_000))


if __name__ == "__main__":
    ck.connect()  # CK_HEAD / CK_TOKEN, default http://127.0.0.1:7702
    for fn, n in ((tiny, 100_000), (medium, 2_000), (heavy, 400)):
        p = ck.plan(fn, range(n))
        print(f"\n== {fn.__name__}: {n:,} items")
        print("decision :", p.where, f"({p.pcs} PCs, {p.slots} slots)" if p.where == "cluster" else "")
        print(f"measured : {p.item_s * 1e3:.3f} ms per item, {p.mem_mb} MB per item")
        print(f"estimate : local {p.local_s:.2f} s on {p.local_procs} process(es), cluster {p.cluster_s or 0:.2f} s")
        print("batch    :", p.batch, "items per remote call")
        for r in p.reasons:
            print("  why    :", r)
    # Same decision, but forced onto the cluster: what it would cost if you insisted
    p = ck.plan(medium, range(2_000), local="never")
    print("\nforced   :", p)
