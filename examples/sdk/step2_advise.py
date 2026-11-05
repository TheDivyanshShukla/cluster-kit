"""Step 2: the advisor tells you what to change in YOUR code.  python3 examples/sdk/step2_advise.py

`slow_score` has four classic problems: a hot pure-Python loop, a write to a module-level list (a side effect that is
lost on remote PCs), unseeded randomness (results differ run to run) and tiny items. `good_score` fixes them.
ck.advise() runs each on a small sample and ranks the suggestions by the seconds they would save.
"""
from __future__ import annotations

import random

try:
    import _path  # noqa: F401
except ImportError:
    pass
import clusterkit as ck

SEEN: list[int] = []  # module-level state: on a cluster every PC has its own copy, so this stays empty here


def slow_score(x: int) -> float:
    SEEN.append(x)                       # side effect: lost on the PCs
    noise = random.random()              # unseeded: not reproducible
    s = 0.0
    for i in range(3_000):               # hot Python loop
        s += (x * i) % 7
    return s + noise


def good_score(x: int, seed: int = 0) -> float:
    rnd = random.Random(seed * 1_000_003 + x)        # seeded per item: same answer on any PC
    s = sum((x * i) % 7 for i in range(3_000))       # same work, no shared state
    return s + rnd.random()


if __name__ == "__main__":
    ck.connect()
    items = list(range(5_000))
    print("== advice for slow_score")
    print(ck.advise(slow_score, items, sample=20))
    print("\n== advice for good_score")
    print(ck.advise(good_score, items, sample=20))
