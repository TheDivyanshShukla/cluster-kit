"""Data-parallel linear regression by model averaging, pure Python: python3 examples/sdk/model_avg.py"""
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import random
import clusterkit as ck


def make_shard(seed: int, n: int = 200) -> list[tuple[float, float]]:  # shards are (seed, n): the data is generated on the PC, so items stay tiny
    r = random.Random(seed)
    return [(x, 2.0 * x + 1.0 + r.gauss(0, 0.1)) for x in (r.uniform(-1, 1) for _ in range(n))]


def step(params: dict[str, float], shard: tuple[int, int], lr: float = 0.1, epochs: int = 20) -> dict[str, float]:  # step(params, shard) -> new params
    w, b = params["w"], params["b"]
    data = make_shard(*shard)
    for _ in range(epochs):
        gw = gb = 0.0
        for x, y in data:
            e = w * x + b - y
            gw += e * x
            gb += e
        w -= lr * gw / len(data)
        b -= lr * gb / len(data)
    return {"w": w, "b": b}


if __name__ == "__main__":
    shards = [(s, 200) for s in range(8)]
    p = ck.train(step=step, init={"w": 0.0, "b": 0.0}, shards=shards, rounds=5)
    print("w={:.3f} b={:.3f} (true 2, 1)".format(p["w"], p["b"]))
