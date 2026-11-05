"""pandas: shard a DataFrame with ck.shard, process each shard on a PC, concat the results. Needs: pandas (here and on the PCs).
python3 examples/sdk/ml_pandas.py"""
import os
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import numpy as np
import pandas as pd
import clusterkit as ck


def per_shard(rows: "list[dict]") -> "list[dict]":  # fn receives one slice of the list: rebuild a DataFrame, answer in plain records
    d = pd.DataFrame(rows)  # (plain records, not a DataFrame, so a PC with another pandas major version can still be read here)
    return d.groupby("key", as_index=False).agg(total=("x", "sum"), n=("x", "size")).to_dict("records")


if __name__ == "__main__":
    ck.config(local=os.environ.get("CK_LOCAL", "auto"))  # CK_LOCAL=never forces the cluster even for this small demo
    rng = np.random.default_rng(1)
    df = pd.DataFrame({"key": rng.integers(0, 10, 50000), "x": rng.standard_normal(50000)})
    rows = df.to_dict("records")  # ck.shard splits lists; each shard is a blob that goes to one PC
    parts = ck.map(per_shard, ck.shard(rows, 8))
    got = pd.concat([pd.DataFrame(p) for p in parts]).groupby("key", as_index=False).sum().sort_values("key").reset_index(drop=True)
    want = df.groupby("key", as_index=False).agg(total=("x", "sum"), n=("x", "size"))
    assert np.allclose(got["total"], want["total"]) and (got["n"].values == want["n"].values).all()
    print("pandas ok:", len(got), "groups, concat of", len(parts), "shard results equals the local groupby")
