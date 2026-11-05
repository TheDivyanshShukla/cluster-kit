"""One small function per advisor rule (test fixtures; never run on a cluster)."""
import os, random, time

CACHE = {}


def tiny(x):
    return x + 1


def blob(x):
    return os.urandom(100_000)


def loop(x):
    s = 0
    for i in range(150_000):
        s += i * x % 7
    return s


def waits(x):
    time.sleep(0.01)
    return x


def hog(x):
    b = bytearray(120 * 1024 * 1024)
    return len(b) + x


def remember(x):
    CACHE[x] = x * 2
    return x


def noisy(x):
    return x + random.random()


def uses_pandas(x):
    if x < 0:
        import pandas  # noqa: F401  (only read by the advisor; never executed)
    return x
