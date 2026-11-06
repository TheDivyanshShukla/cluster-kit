"""Train a small CNN on 28x28 digit images, with the work spread over the cluster.

The images come from scikit-learn's bundled 8x8 digits, blown up to 28x28 with random shifts and noise (no download).
Each PC trains on its own shard for a few epochs, then ck.train averages the models and starts the next round.
  python3 examples/real/train.py local      # one process, all data, the reference accuracy
  python3 examples/real/train.py cluster    # same data split in 8 shards, averaged each round
Needs numpy, scikit-learn and torch here and on every PC. Prints one JSON line with the test accuracy.
"""
import json
import os
import sys
import time

try:
    import _path  # noqa: F401  (lets you run from a checkout without pip install)
except ImportError:
    pass
import numpy as np
import torch
from torch import nn
import clusterkit as ck


def make_images(n_per_digit: int, seed: int) -> "tuple[np.ndarray, np.ndarray]":
    """Digits 0-9 as (n, 28, 28) float32 images in 0..1 plus labels, with a random shift and noise on each."""
    from sklearn.datasets import load_digits

    d = load_digits()
    rng = np.random.default_rng(seed)
    idx = rng.integers(0, len(d.target), n_per_digit * 10)
    small = torch.tensor(d.images[idx] / 16.0, dtype=torch.float32)[:, None]
    big = nn.functional.interpolate(small, size=(24, 24), mode="bilinear", align_corners=False)
    out = torch.zeros(len(idx), 1, 28, 28)
    dx, dy = rng.integers(0, 5, len(idx)), rng.integers(0, 5, len(idx))
    for i in range(len(idx)):
        out[i, :, dy[i]:dy[i] + 24, dx[i]:dx[i] + 24] = big[i]
    out += torch.tensor(rng.normal(0, 0.1, out.shape), dtype=torch.float32)
    return out.clamp(0, 1).numpy(), d.target[idx].astype(np.int64)


def make_model() -> nn.Module:
    torch.manual_seed(0)
    return nn.Sequential(nn.Conv2d(1, 8, 3), nn.ReLU(), nn.MaxPool2d(2), nn.Conv2d(8, 16, 3), nn.ReLU(), nn.MaxPool2d(2),
                         nn.Flatten(), nn.Linear(16 * 5 * 5, 64), nn.ReLU(), nn.Linear(64, 10))


def fit(model: nn.Module, x: np.ndarray, y: np.ndarray, epochs: int, seed: int) -> None:
    torch.set_num_threads(1)
    g = torch.Generator().manual_seed(seed)
    opt = torch.optim.SGD(model.parameters(), lr=0.05, momentum=0.9)
    xt, yt = torch.tensor(x), torch.tensor(y)
    for _ in range(epochs):
        order = torch.randperm(len(yt), generator=g)
        for i in range(0, len(order), 64):
            b = order[i:i + 64]
            opt.zero_grad()
            nn.functional.cross_entropy(model(xt[b]), yt[b]).backward()
            opt.step()


def accuracy(model: nn.Module, x: np.ndarray, y: np.ndarray) -> float:
    model.eval()
    with torch.no_grad():
        return float((model(torch.tensor(x)).argmax(1).numpy() == y).mean())


def step(state: dict, shard: tuple) -> dict:
    """Runs on a PC: one round of training on that PC's shard. shard = (images, labels, seed)."""
    x, y, seed = shard
    m = make_model()
    m.load_state_dict(state)
    fit(m, x, y, epochs=3, seed=seed)
    return {k: v.detach().clone() for k, v in m.state_dict().items()}


SHARDS, ROUNDS, PER_SHARD = 8, 5, 400  # 400 images per digit per shard = 4000 images; 8 shards = 32000 images


def main(mode: str) -> None:
    t0 = time.time()
    xs, ys = zip(*[make_images(PER_SHARD, 100 + s) for s in range(SHARDS)])
    xt, yt = make_images(200, 999)  # held-out test images
    if mode == "local":
        m = make_model()
        x, y = np.concatenate(xs), np.concatenate(ys)
        for r in range(ROUNDS):
            fit(m, x, y, epochs=3, seed=r)  # same amount of passes over the data as the cluster does in total
    else:
        ck.config(local="never", name="real-train")
        final = ck.train(step=step, init={k: v.clone() for k, v in make_model().state_dict().items()},
                         shards=[(xs[s], ys[s], s) for s in range(SHARDS)], rounds=ROUNDS)
        m = make_model()
        m.load_state_dict(final)
    acc = accuracy(m, xt, yt)
    print(json.dumps({"digest": "acc", "metric": round(acc, 4), "tol": 0.02, "secs": round(time.time() - t0, 2)}))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "local")
    sys.stdout.flush()
    os._exit(0)  # do not wait for torch worker threads
