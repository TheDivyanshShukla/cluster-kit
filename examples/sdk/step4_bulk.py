"""Step 4: big data. Upload once, cache on the PCs, and let the network estimate pick the strategy.
python3 examples/sdk/step4_bulk.py

A 40 MB table is used by every item. Without blobs it would be pickled into every item (and refused: items are
capped at a few KB). ck.put() uploads it once (content-addressed: the same bytes never upload twice), each PC
fetches it once and caches it, and plan.transfer() shows the bytes, the strategy (direct from the head, or peer
fan-out inside a lab) and the finish time per PC count. Then ck.shard() splits a list so each PC only gets its part.
"""
from __future__ import annotations

import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import clusterkit as ck


def lookup(args: tuple[list[float], int]) -> float:
    table, i = args              # `table` arrives as the real list on the PC (the handle is resolved there)
    return sum(table[i::1000])


def total(part: list[float]) -> float:
    return sum(part)


if __name__ == "__main__":
    ck.connect()
    table = [float(i % 977) for i in range(5_000_000)]   # ~40 MB pickled
    t0 = time.perf_counter()
    h = ck.put(table)
    print(f"put: {time.perf_counter() - t0:.2f} s (again: ", end="")
    t0 = time.perf_counter()
    ck.put(table)
    print(f"{time.perf_counter() - t0:.2f} s, already on the head)")

    items = [(h, i) for i in range(64)]
    p = ck.plan(lookup, items, local="never")
    print("\nplan    :", p)
    print("transfer:", p.transfer())
    t0 = time.perf_counter()
    out = ck.map(lookup, items, local="never")
    print(f"first run : {time.perf_counter() - t0:.2f} s (PCs fetch and cache the table)")
    t0 = time.perf_counter()
    out2 = ck.map(lookup, items, local="never")
    print(f"second run: {time.perf_counter() - t0:.2f} s (cached on every PC: 0 bytes moved); same: {out == out2}")
    print("check     :", out[:3] == [sum(table[i::1000]) for i in range(3)])

    parts = ck.shard(table, parts=8)                      # 8 slices, each sent only to the PC that sums it
    print("\nsharded sum:", sum(ck.map(total, parts, local="never")), "expected", sum(table))
