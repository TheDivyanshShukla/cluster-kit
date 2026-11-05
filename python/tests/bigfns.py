import os

DATA = list(range(30000))  # a big module-level table (pickle about 100 KB)


def lookup(i):
    return DATA[i] + 1


def total(x):
    return sum(x)


def fsize(path):
    return os.path.getsize(path)


def lines(path):
    with open(path) as f:
        return sum(1 for _ in f)


def star_len(params, shard):
    return len(params) + len(shard)


def make(n):
    return b"x" * n


def slow_use(x):
    return len(x)


def shared_plus(blob, i):
    return len(blob) + i


def heavy(x):
    import time
    time.sleep(0.2)
    return len(x)
