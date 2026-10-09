"""Rust engine `--cmd` example in Python: count primes in [START, END).

Every worker runs `<cmd> START END` for its chunk; whatever it prints goes back to the head.
    ./kit rs run --n 1e7 --chunk 1e6 --cmd "uv run --project python examples/primes.py" | awk '{s+=$1} END {print s}'
"""
import sys


def is_prime(n: int) -> bool:
    if n < 2:
        return False
    if n % 2 == 0:
        return n == 2
    i = 3
    while i * i <= n:
        if n % i == 0:
            return False
        i += 2
    return True


start, end = int(sys.argv[1]), int(sys.argv[2])
print(sum(is_prime(n) for n in range(start, end)))
