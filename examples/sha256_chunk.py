"""Stand-alone version of simple_sha256.py for `ck run --push`.

Counts the numbers in START..END whose SHA-256 (of the number written in decimal) starts with 0000.
Uses only the Python standard library, so the head can send this one file to workers that have no copy of
the kit:   rust/target/release/ck run --n 5e7 --from 1 --chunk 1e6 --push examples/sha256_chunk.py
Each chunk prints one count; add them up (see docs/SIMPLE.md).
"""
import hashlib
import sys

start, end = int(sys.argv[1]), int(sys.argv[2])
print(sum(hashlib.sha256(str(i).encode()).hexdigest().startswith("0000") for i in range(start, end)))
