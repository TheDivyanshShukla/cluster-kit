"""A tour of the Cluster Kit Python SDK: python3 examples/sdk/tour.py

Point it at your head with CK_HEAD=http://<head-ip>:7702 (and CK_TOKEN=... if sign-in is on).
Every function below is ordinary Python; only the ck.* calls are new.
"""
import math
import random

try:
    import _path  # noqa: F401  (lets the example import the SDK from this repo without installing it; the PCs do not have it)
except ImportError:
    pass
import clusterkit as ck


# 1. Data processing: an existing function, unchanged ------------------------------------------------
def clean(row: str) -> dict[str, object]:
    """Parse one CSV-ish row and derive a feature. A stand-in for real per-record work."""
    name, x, y = row.split(",")
    s = 0.0
    for i in range(20_000):  # some real CPU work per row
        s += math.sin(float(x) * i) * math.cos(float(y) * i)
    return {"name": name.strip().lower(), "r": math.hypot(float(x), float(y)), "feature": s}


# 2. A model to tune: plain function of hyper-parameters ---------------------------------------------
def train_and_score(lr: float, epochs: int) -> float:
    """Fit y = 3x + 1 with SGD on synthetic data; return the final squared error (lower is better)."""
    rnd = random.Random(42)
    data = [(x, 3 * x + 1 + rnd.gauss(0, 0.1)) for x in (rnd.uniform(-1, 1) for _ in range(2_000))]
    w = b = 0.0
    for _ in range(epochs):
        for x, y in data:
            err = w * x + b - y
            w -= lr * err * x
            b -= lr * err
    return sum((w * x + b - y) ** 2 for x, y in data) / len(data)


# 3. Data-parallel training: one step on one shard ---------------------------------------------------
def sgd_step(params: dict[str, float], shard_seed: int) -> dict[str, float]:
    """Run a few SGD epochs on this PC's shard (generated from a seed, so no data is shipped)."""
    rnd = random.Random(shard_seed)
    w, b = params["w"], params["b"]
    for _ in range(3):
        for _ in range(1_000):
            x = rnd.uniform(-1, 1)
            y = 2 * x + 1 + rnd.gauss(0, 0.05)
            err = w * x + b - y
            w -= 0.05 * err * x
            b -= 0.05 * err
    return {"w": w, "b": b}


@ck.remote  # calling it is unchanged; .map() spreads it over the cluster
def is_prime(n: int) -> bool:
    if n < 2:
        return False
    return all(n % d for d in range(2, int(n ** 0.5) + 1))


if __name__ == "__main__":
    ck.connect()  # CK_HEAD / CK_TOKEN from the environment, default http://127.0.0.1:7702

    rows = [f"Row{i}, {random.random():.4f}, {random.random():.4f}" for i in range(1_000)]

    # See what the optimizer would do before spending anything
    print("plan:", ck.plan(clean, rows))

    # Ask what would make it faster: tiny items, a hot Python loop, big arguments, side effects, ...
    print(ck.advise(clean, rows))

    # map: same result and order as [clean(r) for r in rows], run where it is fastest
    out = ck.map(clean, rows)
    print("cleaned", len(out), "rows; first:", out[0])

    # map + reduce on your PC
    big = range(10_000_000, 10_002_000)
    print("primes found:", sum(is_prime.map(big)))

    # hyper-parameter sweep: every combination runs in parallel, best first
    best = ck.sweep(train_and_score, grid={"lr": [0.001, 0.01, 0.05], "epochs": [5, 20]}, key=lambda err: err, minimize=True, top=2)
    print("best configs:", best)

    # data-parallel training by model averaging: 8 shards, 5 rounds
    model = ck.train(step=sgd_step, init={"w": 0.0, "b": 0.0}, shards=range(8), rounds=5)
    print(f"trained model: w={model['w']:.3f} b={model['b']:.3f} (true 2, 1)")

    # Options when you want control: at most 3 PCs, fastest first, background priority
    out = ck.map(clean, rows, pcs=3, placement="fastest", priority="background")
    print("again on at most 3 PCs:", len(out), "rows")
