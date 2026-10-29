"""Simplest SHA-256 example: hash every number 1..N, count hashes starting with "0000", show time.

One computer (plain Python, no setup):
    python examples/simple_sha256.py

Same computer, ALL its cores:
    python examples/simple_sha256.py --cores

Whole cluster (run on the head PC). Works with either engine, it picks whichever head is running:
    ./kit py head   (or ./kit rs head)  on the head PC, worker on the others, then
    ./kit py run examples/simple_sha256.py --cluster

Bigger job = bigger cluster win:  add  --n 5e7

The Rust engine runs this same file on each worker as `simple_sha256.py START END`.
"""
import argparse
import hashlib
import json
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

KIT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(KIT / "python"))
from cluster import SCHED_PORT, tcp_ok  # noqa: E402

RS_PORT, RS_DASH_PORT = 7700, 7702  # rust/src/main.rs PORT, DASH_PORT


def hash_range(start: int, end: int) -> int:
    """The work. Counts numbers in [start, end) whose SHA-256 starts with 0000."""
    return sum(hashlib.sha256(str(i).encode()).hexdigest().startswith("0000") for i in range(start, end))


def run_dask(pieces: list[tuple[int, int]]) -> tuple[list[int], str]:
    from distributed import Client
    client = Client(f"tcp://127.0.0.1:{SCHED_PORT}", timeout=10)
    parts = client.gather(client.map(hash_range, *zip(*pieces)))
    return parts, f"cluster, Dask engine ({len(client.scheduler_info()['workers'])} workers)"


def run_rust(n: int, chunk: int) -> tuple[list[int], str]:
    # Workers run `<cmd> START END` from their kit folder and print one count each.
    kit = KIT / ("kit.bat" if sys.platform == "win32" else "kit")
    cmd = "uv run --project python examples/simple_sha256.py"
    out = subprocess.run([str(kit), "rs", "run", "--n", str(n), "--from", "1", "--chunk", str(chunk), "--cmd", cmd],
                         cwd=KIT, capture_output=True, text=True)
    lines = out.stdout.split()
    errors = [l for l in out.stdout.splitlines() if l.startswith("ERROR")]
    if out.returncode or errors:
        sys.exit("Rust engine job failed:\n" + "\n".join(errors or [out.stderr.strip()]))
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{RS_DASH_PORT}/stats", timeout=2) as r:
            workers = len(json.load(r)["workers"])
    except (OSError, ValueError, KeyError):
        workers = "?"
    return [int(x) for x in lines], f"cluster, Rust engine ({workers} workers)"


def main() -> None:
    if len(sys.argv) == 3 and all(a.isdigit() for a in sys.argv[1:]):  # Rust engine worker call
        print(hash_range(int(sys.argv[1]), int(sys.argv[2])))
        return

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
        if tcp_ok("127.0.0.1", SCHED_PORT, 1):
            parts, where = run_dask(pieces)
        elif tcp_ok("127.0.0.1", RS_PORT, 1):
            parts, where = run_rust(n, n // 10 + 1)  # few big chunks: each one starts a Python per core
        else:
            sys.exit("No cluster head running on this PC. Start one first:  ./kit py head   or   ./kit rs head")
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
