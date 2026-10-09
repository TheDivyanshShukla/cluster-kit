"""Dask engine example: SHA-256 every number in 1..N across the cluster.

Run on the head PC while `kit py head` + workers are up:
    ./kit py run examples/dask_sha256.py                  # 1..1,000,000, Dask spreads chunks over all PCs
    ./kit py run examples/dask_sha256.py --n 5e7 --chunk 1e6
    ./kit py run examples/dask_sha256.py --pin            # PC 1 gets range 1, PC 2 range 2, ... (manual split)
"""
import argparse
import sys
import time
from pathlib import Path

from distributed import Client

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
from cluster import SCHED_PORT, host_table  # noqa: E402


def hash_range(start: int, end: int, prefix: str) -> list[tuple[int, str]]:
    """Runs on a worker. Returns numbers in [start, end) whose hash starts with prefix."""
    import hashlib
    out = []
    for i in range(start, end):
        h = hashlib.sha256(str(i).encode()).hexdigest()
        if h.startswith(prefix):
            out.append((i, h))
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=float, default=1e6)
    ap.add_argument("--chunk", type=float, default=1e5, help="numbers per task")
    ap.add_argument("--prefix", default="0000")
    ap.add_argument("--pin", action="store_true", help="split 1..N evenly, one range per PC")
    args = ap.parse_args()
    n, chunk = int(args.n), int(args.chunk)

    client = Client(f"tcp://127.0.0.1:{SCHED_PORT}", timeout=10)
    t = time.perf_counter()

    if args.pin:
        order, hosts = host_table(client)
        step = n // len(order) + 1
        futs = []
        for i, h in enumerate(order):
            lo, hi = 1 + i * step, min(1 + (i + 1) * step, n + 1)
            print(f"PC {h}: {lo:,} .. {hi - 1:,}")
            futs.append(client.submit(hash_range, lo, hi, args.prefix,
                                      workers=hosts[h], allow_other_workers=False, pure=False))
    else:
        # many small chunks: scheduler hands them to whichever core is free (load-balances)
        starts = range(1, n + 1, chunk)
        futs = client.map(hash_range, starts, [min(s + chunk, n + 1) for s in starts],
                          prefix=args.prefix, pure=False)

    hits = [x for part in client.gather(futs) for x in part]
    print(f"\n{len(hits)} hashes start with '{args.prefix}' in 1..{n:,}  ({time.perf_counter() - t:.2f}s)")
    for i, h in hits[:10]:
        print(f"  {i:>12,}  {h}")


if __name__ == "__main__":
    main()
