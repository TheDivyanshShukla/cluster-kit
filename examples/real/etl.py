"""Filter and aggregate a 5-million-row transactions CSV: shard it, filter and group on the PCs, merge the small answers here.

  python3 examples/real/etl.py local     # pandas on the whole file
  python3 examples/real/etl.py cluster   # ck.shard cuts the file into 24 blobs (uploaded once); each PC aggregates its part
Money is kept in whole cents so sums are exact and both runs must match to the last cent. Needs numpy and pandas here and on every PC.
"""
import hashlib
import json
import os
import sys
import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import numpy as np
import pandas as pd
import clusterkit as ck

ROWS = 5_000_000
COLS = ["txn", "customer", "category", "country", "status", "cents", "day", "memo"]


def make_csv(path: str) -> None:
    """Writes the file once (about 330 MB, no header line) and reuses it afterwards."""
    if os.path.exists(path):
        return
    rng = np.random.default_rng(7)
    shops = ["payment to shop", "refund for order", "subscription renewal", "transfer from friend", "cash withdrawal", "card top up"]
    memos = np.array([f"{shops[i % 6]} {i} ref {'ABCDEF'[i % 6]}{i % 97}" for i in range(20_000)])
    tmp = path + ".part"
    with open(tmp, "w") as f:
        for k in range(10):  # 10 pieces of 500k rows keeps memory small
            n = ROWS // 10
            d = pd.DataFrame({"txn": np.arange(k * n, (k + 1) * n), "customer": rng.integers(0, 200_000, n),
                              "category": rng.choice(["food", "fuel", "rent", "games", "books", "travel", "health", "tools"], n, p=[.3, .15, .05, .1, .1, .1, .1, .1]),
                              "country": rng.choice(["IN", "US", "DE", "BR", "JP", "FR"], n), "status": rng.choice(["ok", "ok", "ok", "refund", "failed"], n),
                              "cents": rng.lognormal(6, 1.2, n).astype(np.int64), "day": rng.integers(0, 365, n), "memo": memos[rng.integers(0, 20_000, n)]})
            d.to_csv(f, header=False, index=False)
    os.replace(tmp, path)


def aggregate(path: str) -> list:
    """Runs on a PC (or here): the rows of one part of the file -> totals per (category, country, month). Only 'ok' rows over 5.00."""
    d = pd.read_csv(path, header=None, names=COLS, dtype={"cents": "int64"})
    d = d[(d["status"] == "ok") & (d["cents"] > 500) & d["memo"].str.contains(r" ref [ABC]\d+$", regex=True)]
    d = d.assign(month=d["day"] // 30, kind=d["memo"].str.split(" ").str[0])
    g = d.groupby(["category", "country", "month", "kind"]).agg(n=("cents", "size"), cents=("cents", "sum"), top=("cents", "max"))
    return g.reset_index().values.tolist()


def merge(parts: list) -> list:
    t: dict = {}
    for rows in parts:
        for cat, cty, mon, kind, n, cents, top in rows:
            a = t.setdefault((cat, cty, int(mon), kind), [0, 0, 0])
            a[0] += int(n); a[1] += int(cents); a[2] = max(a[2], int(top))
    return sorted([list(k) + v for k, v in t.items()])


def main(mode: str) -> None:
    path = os.path.join(os.environ.get("WL_DATA", "."), "txns.csv")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    make_csv(path)
    t0 = time.time()
    if mode == "local":
        out = merge([aggregate(path)])
    else:
        ck.config(local="never", name="real-etl")
        out = merge(ck.map(aggregate, ck.shard(path, parts=24)))
    print(json.dumps({"digest": hashlib.sha256(json.dumps(out).encode()).hexdigest()[:16], "groups": len(out), "rows_ok": sum(r[4] for r in out), "secs": round(time.time() - t0, 2)}))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "local")
