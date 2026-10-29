"""Simplest example: count prime numbers, and show how long it took.

One computer (just plain Python, no setup):
    python examples/simple_time.py

Whole cluster (run on the head PC after `kit py head` + workers are up):
    ./kit py run examples/simple_time.py --cluster

Bigger job = bigger difference:  add  --n 5e6
"""
import argparse
import sys
import time
from pathlib import Path


def count_primes(start: int, end: int) -> int:
    """The work. Counts primes in [start, end)."""
    count = 0
    for n in range(max(start, 2), end):
        i = 2
        while i * i <= n:
            if n % i == 0:
                break
            i += 1
        else:
            count += 1
    return count


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=float, default=2e6, help="count primes below this number")
    ap.add_argument("--cluster", action="store_true", help="use the cluster instead of this computer only")
    args = ap.parse_args()
    n = int(args.n)

    # cut the work into 100 small pieces
    step = n // 100 + 1
    pieces = [(s, min(s + step, n)) for s in range(0, n, step)]

    t = time.perf_counter()
    if args.cluster:
        from distributed import Client
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
        from cluster import SCHED_PORT
        client = Client(f"tcp://127.0.0.1:{SCHED_PORT}", timeout=10)
        where = f"cluster ({len(client.scheduler_info()['workers'])} workers)"
        parts = client.gather(client.map(count_primes, *zip(*pieces)))
    else:
        where = "this computer, 1 core"
        parts = [count_primes(s, e) for s, e in pieces]

    print(f"Primes below {n:,}: {sum(parts):,}")
    print(f"Ran on: {where}")
    print(f"Time taken: {time.perf_counter() - t:.2f} seconds")


if __name__ == "__main__":
    main()
