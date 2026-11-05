"""Step 5: data-parallel training with model averaging, and the warm runner.  python3 examples/sdk/step5_train.py

Each round sends the current model to every shard's PC, runs `step` there, and averages the returned models
(ck.mean_params). The first round starts a fresh Python on each PC; it leaves a warm worker behind that already
imported your module, so later rounds skip the interpreter start and imports. Round times show that.
CK_WARM=off on the PCs turns the warm runner off for comparison.
"""
from __future__ import annotations

import random
import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import clusterkit as ck

TRUE_W, TRUE_B = 2.5, -1.0


def step(model: dict[str, float], shard: int) -> dict[str, float]:
    """A few SGD epochs on this shard's data (made from a seed, so no data travels)."""
    rnd = random.Random(shard)
    w, b = model["w"], model["b"]
    for _ in range(2_000):
        x = rnd.uniform(-1, 1)
        err = w * x + b - (TRUE_W * x + TRUE_B + rnd.gauss(0, 0.05))
        w -= 0.05 * err * x
        b -= 0.05 * err
    return {"w": w, "b": b}


if __name__ == "__main__":
    ck.connect()
    model = {"w": 0.0, "b": 0.0}
    for r in range(1, 7):
        t0 = time.perf_counter()
        model = ck.train(step=step, init=model, shards=range(8), rounds=1, local="never")
        print(f"round {r}: {time.perf_counter() - t0:.2f} s  w={model['w']:.3f} b={model['b']:.3f}")
    print(f"true: w={TRUE_W} b={TRUE_B}")
