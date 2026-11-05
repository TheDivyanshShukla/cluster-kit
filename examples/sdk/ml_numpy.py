"""NumPy on the cluster: arrays as arguments, results and blobs. Needs: numpy (on this machine and on every PC that runs it).
python3 examples/sdk/ml_numpy.py"""
import os
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import numpy as np
import clusterkit as ck


def gram(a: "np.ndarray") -> "np.ndarray":  # a big array argument travels once as a blob (pickle protocol 5, no extra copy)
    return a.T @ a


def col_mean(a: "np.ndarray") -> "np.ndarray":
    return a.mean(axis=0)


if __name__ == "__main__":
    ck.config(local=os.environ.get("CK_LOCAL", "auto"))  # CK_LOCAL=never forces the cluster even for this small demo
    rng = np.random.default_rng(0)
    parts = [rng.standard_normal((20000, 32)) for _ in range(8)]
    out = ck.map(gram, parts)  # each ~5 MB array: sent as a blob when the head has a blob store
    total = sum(out)
    assert np.allclose(total, sum(p.T @ p for p in parts))
    handles = [ck.put(p) for p in parts]  # same bytes are the same blob: already on the head from the first call, so nothing is sent again
    means = ck.map(col_mean, handles)
    assert np.allclose(np.vstack(means), np.vstack([p.mean(axis=0) for p in parts]))
    print("numpy ok:", total.shape, f"Gram trace {np.trace(total):.1f}")
