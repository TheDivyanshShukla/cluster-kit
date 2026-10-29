"""Simplest SHA-256 example: hash every number 1..N, count hashes starting with "0000", show time.

One computer (plain Python, no setup):
    python examples/simple_sha256.py

Same computer, ALL its cores:
    python examples/simple_sha256.py --cores

Whole cluster (run on the head PC after `kit py head` + workers are up):
    ./kit py run examples/simple_sha256.py --cluster

Bigger job = bigger cluster win:  add  --n 5e7
"""
import argparse
import hashlib
import sys
import time
from pathlib import Path


def hash_range(start: int, end: int) -> int:
    """The work. Counts numbers in [start, end) whose SHA-256 starts with 0000."""
    return sum(hashlib.sha256(str(i).encode()).hexdigest().startswith("0000") for i in range(start, end))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=float, default=2e6, help="hash numbers 1..N")
    ap.add_argument("--cluster", action="store_true", help="use the cluster instead of this computer only")
    ap.add_argument("--cores", action="store_true", help="use every core of this computer")
    args = ap.parse_args()
    n = int(args.n)

    step = n // 100 + 1  # cut the work into ~100 pieces
    pieces = [(s, min(s + step, n + 1)) for s in range(1, n + 1, step)]

    t = time.perf_counter()
    if args.cluster:
        from distributed import Client
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
        from cluster import SCHED_PORT
        client = Client(f"tcp://127.0.0.1:{SCHED_PORT}", timeout=10)
        where = f"cluster ({len(client.scheduler_info()['workers'])} workers)"
        parts = client.gather(client.map(hash_range, *zip(*pieces)))
    elif args.cores:
        import os
        from multiprocessing import Pool
        with Pool() as pool:
            parts = pool.starmap(hash_range, pieces)
        where = f"this computer, {os.cpu_count()} cores"
    else:
        where = "this computer, 1 core"
        parts = [hash_range(s, e) for s, e in pieces]

    print(f"Hashes of 1..{n:,} starting with 0000: {sum(parts):,}")
    print(f"Ran on: {where}")
    print(f"Time taken: {time.perf_counter() - t:.2f} seconds")


if __name__ == "__main__":
    main()
