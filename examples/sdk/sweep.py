"""Parameter sweep: python3 examples/sdk/sweep.py"""
import _path  # noqa: F401
import clusterkit as ck


def train_and_score(lr, depth):
    w = 0.0
    for step in range(300_000 // depth * 4):  # toy fit of y = 3x
        w += lr * (3 - w)
    return {"score": -abs(3 - w) - 0.001 * depth}


if __name__ == "__main__":
    best = ck.sweep(train_and_score, grid={"lr": [1e-5, 1e-3, 1e-2], "depth": [4, 8, 12, 16]}, key=lambda r: r["score"], top=3)
    for b in best:
        print(b["params"], b["result"])
