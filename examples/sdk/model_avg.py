"""Data-parallel linear regression by model averaging, pure Python: python3 examples/sdk/model_avg.py"""
import _path  # noqa: F401
import random
import clusterkit as ck


def make_shard(seed, n=200):  # shards are (seed, n): the data is generated on the PC, so items stay tiny
    r = random.Random(seed)
    return [(x, 2.0 * x + 1.0 + r.gauss(0, 0.1)) for x in (r.uniform(-1, 1) for _ in range(n))]


def step(params, shard, lr=0.1, epochs=20):  # step(params, shard) -> new params
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
    print("w=%.3f b=%.3f (true 2, 1)" % (p["w"], p["b"]))
