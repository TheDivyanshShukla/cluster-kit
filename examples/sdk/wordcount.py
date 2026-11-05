"""Word count over many texts, summed on your PC: python3 examples/sdk/wordcount.py"""
import _path  # noqa: F401
from collections import Counter
import clusterkit as ck


def count_words(text: str) -> Counter[str]:
    c = Counter(text.split())
    for _ in range(30_000):  # stand-in for heavier tokenising
        c.update(["x"])
    return c


def merge(counters: list[Counter[str]]) -> Counter[str]:
    total: Counter[str] = Counter()
    for c in counters:
        total += c
    return total


if __name__ == "__main__":
    docs = ["the quick brown fox jumps over the lazy dog " * 20 + f"doc{i}" for i in range(300)]
    total = ck.map(count_words, docs, reduce=merge)
    print(total.most_common(3))
