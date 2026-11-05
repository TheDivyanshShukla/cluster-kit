"""python -m clusterkit bench: time every overhead step of a forced-cluster job with empty work."""
from __future__ import annotations
import os, statistics, subprocess, tempfile, time
from typing import Any
import clusterkit as ck
from clusterkit import _runner, _benchfn

ROWS = [("encode", "encode_s", "pickle + compress the items (this PC)"),
        ("ship", "ship_s", "build the pushed command (cached after the first job)"),
        ("submit", "submit_s", "POST /jobs"),
        ("dispatch", "queue_s", "job created -> first piece running"),
        ("remote run", "run_s", "running on the PCs, python start included"),
        ("detect", "detect_s", "job finished -> this PC knows (pushed by SSE)"),
        ("fetch", "fetch_s", "GET result"),
        ("decode", "decode_s", "unpickle the results (this PC)")]


def python_start() -> tuple[float, float]:
    """Seconds to start python3 and the runner once (what each piece pays on a PC like this one)."""
    pay = ck._payload(_benchfn.noop, "call", False)
    script = f"PAYLOAD = {pay!r}\n" + open(_runner.__file__).read()
    with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as f:
        f.write(script)
    try:
        def one(cmd: list[str], inp: str = "") -> float:
            t = time.perf_counter()
            subprocess.run(cmd, input=inp, capture_output=True, text=True)
            return time.perf_counter() - t
        bare = statistics.median(one(["python3", "-c", "pass"]) for _ in range(5))
        full = statistics.median(one(["python3", f.name], _runner.enc(1) + "\n") for _ in range(5))
    finally:
        os.unlink(f.name)
    return bare, full


def measure(n: int, batch: bool, reps: int) -> dict[str, float]:
    runs: list[dict[str, Any]] = []
    for _ in range(reps):
        o = ck._opts({"local": "never", "history": False, "batch": batch})
        items = list(range(n))
        pl = ck._plan(_benchfn.noop, items, "call", o, profile=(1e-7, 0.0))
        ck._run(_benchfn.noop, "call", items, o, pl)
        r = dict(ck._last["st"], wall=ck._last["wall"], pcs=pl.pcs, slots=pl.slots, batch=pl.batch)
        runs.append(r)
    keys = {k for r in runs for k, v in r.items() if isinstance(v, (int, float)) and not isinstance(v, bool)}
    return {k: statistics.median(r[k] for r in runs if r.get(k) is not None) for k in keys}


def main(sizes: tuple[int, ...] = (1, 100, 10000), reps: int = 3) -> None:
    cols = [(n, False) for n in sizes] + [(sizes[-1], True)]
    ck._stream.ensure()
    res = [measure(n, b, reps) for n, b in cols]
    bare, full = python_start()
    head = ["step (s)"] + [f"{n:,}" + (" batched" if b else "") for n, b in cols]
    lines = [head]
    for label, key, _ in ROWS:
        lines.append([label] + [f"{r.get(key, 0):.4f}" for r in res])
    lines.append(["total wall"] + [f"{r['wall']:.4f}" for r in res])
    lines.append(["per item (us)"] + [f"{r['wall'] / n * 1e6:,.0f}" for r, (n, _) in zip(res, cols)])
    lines.append(["pieces / PCs / slots"] + [f"{r['lines']:.0f} lines / {r['pcs']:.0f} / {r['slots']:.0f}" for r in res])
    w = [max(len(l[i]) for l in lines) for i in range(len(head))]
    print(f"Forced-cluster job with empty work (median of {reps} runs, head {ck._cfg['head']})\n")
    for i, l in enumerate(lines):
        print("  ".join(c.ljust(w[j]) if j == 0 else c.rjust(w[j]) for j, c in enumerate(l)))
        if i == 0:
            print("  ".join("-" * x for x in w))
    print(f"\npython3 start: {bare * 1000:.0f} ms bare, {full * 1000:.0f} ms with the runner and one item (paid once per piece, per thread, on each PC)")
    f_s = res[0]["wall"]
    mid, big, bat = res[len(sizes) - 2], res[len(sizes) - 1], res[-1]
    n_mid, n_big = sizes[-2], sizes[-1]
    c = (big["wall"] - mid["wall"]) / (n_big - n_mid)  # s per item, naive one-item-per-line path
    cb = (bat["wall"] - res[0]["wall"]) / (n_big - sizes[0])  # same with automatic batching
    k = max(res[0].get("slots") or 0, _slots())
    if k > 1.5:
        kf = 1 - 1.5 / k
        print(f"\nBreak-even (K={k} slots on the awake PCs, fixed overhead F={f_s:.3f} s, per item c={c * 1e6:.0f} us naive / {cb * 1e6:.0f} us batched):")
        print(f"  cluster wins by 1.5x or more when total work > {1.5 * f_s / kf:.2f} s and per-item work > {1.5 * cb / kf * 1000:.2f} ms "
              f"({1.5 * c / kf * 1000:.2f} ms without batching)")
        start = max(0.0, (res[1]["wall"] - res[0]["wall"]) * max(res[1].get("slots") or 1, 1) / (sizes[1] - sizes[0]))
        print(f"  suggested defaults: ck.config(fixed_s={f_s:.2f}, item_c={(big['encode_s'] + big.get('fetch_s', 0) + big['decode_s']) / n_big:.1e}, start_s={start:.3f})  "
              f"(start_s = effective cost of one remote call per slot; python alone starts in {full * 1000:.0f} ms)")


def _slots() -> int:
    try:
        pcs, *_ = ck._cluster_view(ck._opts({}))
        return sum(t for t, _ in pcs)
    except ck.ClusterError:
        return 0
