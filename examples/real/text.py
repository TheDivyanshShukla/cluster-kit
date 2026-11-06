"""Word counts and exact duplicate-line removal over a 1 GB generated text corpus.

  python3 examples/real/text.py local     # one pass over the file
  python3 examples/real/text.py cluster   # ck.shard cuts it into 32 line-aligned blobs; each PC counts words and hashes its lines
Stage 1 (on the PCs): word counts and a 64-bit hash of every distinct line. Stage 2 (here): merge counts, count distinct hashes.
Needs numpy here and on every PC.
"""
import hashlib
import json
import os
import sys
import time
import zlib
from collections import Counter

try:
    import _path  # noqa: F401
except ImportError:
    pass
import numpy as np
import clusterkit as ck

TARGET_MB = int(os.environ.get("WL_TEXT_MB", "1024"))


def make_corpus(path: str) -> None:
    """About 1 GB: lines of 6-18 words from a 60,000 word Zipf-shaped vocabulary, one line in five a repeat of an earlier line."""
    if os.path.exists(path):
        return
    rng = np.random.default_rng(3)
    vocab = np.array([f"w{i:x}" for i in range(60_000)])
    with open(path + ".part", "w") as f:
        done, prev = 0, []
        while done < TARGET_MB * 2**20:
            ids = np.minimum(rng.zipf(1.15, 2_000_000) - 1, 59_999)
            words = vocab[ids].tolist()
            lines, k = [], 0
            while k < len(words) - 20:
                n = int(rng.integers(6, 19))
                lines.append(" ".join(words[k:k + n]))
                k += n
            for j in rng.integers(0, len(lines), len(lines) // 5):
                lines[j] = prev[int(rng.integers(0, len(prev)))] if prev else lines[j]  # repeat an earlier line
            prev = lines[-5000:]
            blob = "\n".join(lines) + "\n"
            f.write(blob)
            done += len(blob)
    os.replace(path + ".part", path)


def scan(path: str) -> "tuple[dict, int, bytes]":
    """One part of the file -> (word counts, number of lines, sorted array of distinct line hashes as bytes)."""
    c: Counter = Counter()
    hs = []
    n = 0
    with open(path, "rb") as f:
        for line in f:
            n += 1
            c.update(line.split())
            hs.append(zlib.crc32(line) | (zlib.adler32(line) << 32))
    return dict(c), n, np.unique(np.array(hs, dtype=np.uint64)).tobytes()


def main(mode: str) -> None:
    path = os.path.join(os.environ.get("WL_DATA", "."), "corpus.txt")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    make_corpus(path)
    t0 = time.time()
    if mode == "local":
        parts = [scan(path)]
    else:
        ck.config(local="never", name="real-text", timeout_s=1800)
        parts = ck.map(scan, ck.shard(path, parts=32))
    total: Counter = Counter()
    for c, _, _ in parts:
        total.update(c)
    lines = sum(p[1] for p in parts)
    distinct = len(np.unique(np.concatenate([np.frombuffer(p[2], dtype=np.uint64) for p in parts])))
    top = [(w.decode(), n) for w, n in sorted(total.items(), key=lambda kv: (-kv[1], kv[0]))[:1000]]
    dig = hashlib.sha256(json.dumps([top, sum(total.values()), len(total), lines, distinct]).encode()).hexdigest()[:16]
    print(json.dumps({"digest": dig, "words": sum(total.values()), "vocab": len(total), "lines": lines, "distinct_lines": distinct, "secs": round(time.time() - t0, 2)}))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "local")
