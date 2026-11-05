def inc(x):
    return x + 1


def boom(x):
    if x == 3:
        raise ValueError("bad item 3")
    return x


def kw(a, b):
    return a * 10 + b


def two(p, s):
    return [p[0] + s, p[1]]


def slow(x):
    s = 0
    for i in range(300_000):
        s += i
    return s + x


def ident(x):
    return x
