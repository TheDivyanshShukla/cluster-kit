"""PyTorch (CPU): train a small model by state_dict averaging with ck.train; ck.mean_params averages tensors without importing torch.
Needs: torch (here and on the PCs). python3 examples/sdk/ml_torch.py"""
import os
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import clusterkit as ck
import torch
from torch import nn


def make_model() -> "nn.Module":
    torch.manual_seed(0)
    return nn.Sequential(nn.Linear(4, 16), nn.ReLU(), nn.Linear(16, 1))


def data(seed: int, n: int = 256) -> "tuple[torch.Tensor, torch.Tensor]":
    g = torch.Generator().manual_seed(seed)
    x = torch.randn(n, 4, generator=g)
    return x, (x @ torch.tensor([[1.0], [-2.0], [0.5], [3.0]])) + 0.01 * torch.randn(n, 1, generator=g)


def step(state: dict, shard: int) -> dict:  # one local epoch set on this shard, returns the new state_dict
    torch.set_num_threads(1)
    m = make_model()
    m.load_state_dict(state)
    opt = torch.optim.SGD(m.parameters(), lr=0.05)
    x, y = data(shard)
    for _ in range(30):
        opt.zero_grad()
        nn.functional.mse_loss(m(x), y).backward()
        opt.step()
    return {k: v.detach().clone() for k, v in m.state_dict().items()}


if __name__ == "__main__":
    ck.config(local=os.environ.get("CK_LOCAL", "auto"))  # CK_LOCAL=never forces the cluster even for this small demo
    init = {k: v.clone() for k, v in make_model().state_dict().items()}
    x, y = data(99)

    def loss(s: dict) -> float:
        m = make_model()
        m.load_state_dict(s)
        return float(nn.functional.mse_loss(m(x), y))

    final = ck.train(step, init, shards=list(range(6)), rounds=8)
    print(f"torch ok: held-out MSE {loss(init):.3f} -> {loss(final):.3f}")
    assert loss(final) < loss(init) * 0.5
