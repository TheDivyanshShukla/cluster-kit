"""Cluster Kit Python SDK: run ordinary Python work on the lab cluster. Standard library only. See docs/SDK.md."""
import base64, functools, inspect, itertools, json, logging, math, os, resource, sys, time, traceback, urllib.error, urllib.request, zlib
import tracemalloc
import urllib.parse
from . import _runner

__all__ = ["connect", "config", "map", "remote", "sweep", "train", "plan", "mean_params", "RemoteError", "ClusterError", "Plan"]
log = logging.getLogger("clusterkit")

_cfg = {"head": os.environ.get("CK_HEAD", "http://127.0.0.1:7702"), "token": os.environ.get("CK_TOKEN"), "pcs": None,
        "placement": "auto", "priority": "normal", "selector": "", "local": "auto", "mem_mb": None, "timeout_s": None,
        "retries": 0, "name": None, "ship": [], "push_max_kb": 64, "max_item_bytes": 4096, "target_wall_s": 10.0}
_OPTS = ("pcs", "placement", "priority", "selector", "local", "mem_mb", "timeout_s", "retries", "name")


class ClusterError(Exception):
    """The head refused or failed the job."""


class RemoteError(Exception):
    """fn raised on a PC; .remote_traceback has the traceback from there."""

    def __init__(self, tb):
        super().__init__("remote call failed:\n" + tb)
        self.remote_traceback = tb


def connect(head=None, token=None):
    _cfg["head"] = (head or _cfg["head"]).rstrip("/")
    if token is not None:
        _cfg["token"] = token


def config(**kw):
    bad = set(kw) - set(_cfg)
    if bad:
        raise TypeError("unknown option(s): " + ", ".join(sorted(bad)))
    _cfg.update(kw)


def _opts(kw):
    bad = set(kw) - set(_OPTS)
    if bad:
        raise TypeError("unknown option(s): " + ", ".join(sorted(bad)))
    return {**_cfg, **{k: v for k, v in kw.items() if v is not None}}


# ---------------------------------------------------------------- HTTP
def _http(method, path, body=None, text=False, timeout=60):
    req = urllib.request.Request(_cfg["head"] + path, method=method, data=None if body is None else json.dumps(body).encode())
    if body is not None:
        req.add_header("Content-Type", "application/json")
    if _cfg["token"]:
        req.add_header("Authorization", "Bearer " + _cfg["token"])
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = r.read()
    except urllib.error.HTTPError as e:
        raise ClusterError(f"{method} {path}: HTTP {e.code} {e.read()[:300].decode(errors='replace')}") from None
    except OSError as e:
        raise ClusterError(f"cannot reach the head at {_cfg['head']} ({e}); call ck.connect(url) or set CK_HEAD") from None
    return data.decode() if text else json.loads(data)


# ---------------------------------------------------------------- code shipping
def _describe(fn):
    """(module name, module source, attr path) for a function the PCs can re-import, or raise ValueError."""
    fn = getattr(fn, "_ck_fn", fn)
    q = getattr(fn, "__qualname__", "")
    if not q or "<" in q:
        raise ValueError(f"{q or fn!r} is a lambda or nested function; define it at the top level of your script")
    mod = sys.modules.get(fn.__module__)
    path = getattr(mod, "__file__", None)
    if not path or not os.path.exists(path):
        raise ValueError(f"the source file of module {fn.__module__!r} is not available (interactive session?)")
    with open(path, encoding="utf-8") as f:
        src = f.read()
    name = "__ck_user__" if fn.__module__ == "__main__" else fn.__module__
    return name, src, q


def _payload(fn, mode):
    name, src, q = _describe(fn)
    mods = []
    for p in _cfg["ship"]:
        with open(p, encoding="utf-8") as f:
            mods.append([os.path.splitext(os.path.basename(p))[0], f.read()])
    mods.append([name, src])
    raw = json.dumps({"mods": mods, "main": name, "attr": q, "mode": mode}).encode()
    return base64.b64encode(zlib.compress(raw, 9)).decode()


def _cmd(payload):
    with open(_runner.__file__, encoding="utf-8") as f:
        runner = f.read()
    script = f"PAYLOAD = {payload!r}\n" + runner
    limit = _cfg["push_max_kb"] * 1024
    if len(script) > limit:
        raise ClusterError(f"the shipped code is {len(script)} bytes, over the push limit of {limit}; "
                           f"raise it with `ck admin tune push_max_kb {len(script) // 1024 + 1}` and ck.config(push_max_kb=...)")
    return "\x01PUSH\npython3\nck_runner.py\n" + script


def _encode(items):
    lines = [_runner.enc(x) for x in items]
    lim = _cfg["max_item_bytes"]
    for i, s in enumerate(lines):
        if len(s) > lim:
            raise ClusterError(f"item {i} is {len(s)} bytes encoded, over max_item_bytes={lim}; raise it with "
                               f"`ck admin tune max_item_bytes {len(s) + 64}` and ck.config(max_item_bytes=...), "
                               "or pass paths every PC can read instead of the data itself")
    return lines


# ---------------------------------------------------------------- local execution
_G = None


def _lw(x):
    return _runner.apply(_G[0], _G[1], x)


def _local(fn, mode, items, procs=1):
    global _G
    fn = getattr(fn, "_ck_fn", fn)
    if procs > 1 and len(items) > 1:
        try:
            import multiprocessing as mp
            _G = (mode, fn)
            with mp.get_context("fork").Pool(procs) as p:
                return p.map(_lw, items, chunksize=max(1, len(items) // (procs * 4)))
        except Exception as e:  # no fork (Windows) or unpicklable result: serial is always right
            log.info("local parallel run failed (%s); running serially", e)
    return [_runner.apply(mode, fn, x) for x in items]


# ---------------------------------------------------------------- plan
class Plan:
    def __init__(self, **kw):
        self.__dict__.update(kw)

    def __str__(self):
        t = lambda s: f"{s:.1f} s" if s < 120 else f"{s / 60:.1f} min"
        head = (f"{self.n:,} items x {self.item_s:.3g} s: " +
                (f"cluster, {self.pcs} PCs, about {t(self.cluster_s)} (local: {t(self.local_s)})" if self.where == "cluster"
                 else f"local, about {t(self.local_s)} (cluster: {t(self.cluster_s) if self.cluster_s else 'n/a'})"))
        return head + "".join("; " + r for r in self.reasons)

    __repr__ = __str__


def _profile(fn, mode, items):
    n = len(items)
    k = max(3, math.ceil(n * 0.01))
    t0, done = time.perf_counter(), 0
    for x in items[:k]:
        _runner.apply(mode, fn, x)
        done += 1
        if time.perf_counter() - t0 > 2.0:
            break
    per = (time.perf_counter() - t0) / max(done, 1)
    # memory: python allocations of one call (tracemalloc), or the RSS growth if that is larger
    rss0 = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    tracemalloc.start()
    try:
        _runner.apply(mode, fn, items[0])
        peak = tracemalloc.get_traced_memory()[1]
    finally:
        tracemalloc.stop()
    scale = 1 if sys.platform == "darwin" else 1024
    rss = (resource.getrusage(resource.RUSAGE_SELF).ru_maxrss - rss0) * scale
    return per, max(peak, rss) / 1048576


def _cluster_view(o):
    """[(threads, free_mb)] of usable PCs, asleep count, rtt seconds, queue note."""
    t0 = time.perf_counter()
    c = _http("GET", "/api/v1/cluster", timeout=10)
    rtt = time.perf_counter() - t0
    allowed = None
    if o["selector"]:
        s = _http("GET", "/api/v1/select?q=" + urllib.parse.quote(o["selector"]), timeout=10)
        allowed = set(s.get("node_ids") or [])
    pcs = []
    for w in c.get("workers", []):
        if w.get("blocked") or (allowed is not None and w.get("node_id") not in allowed):
            continue
        th = w.get("threads") or 1
        cap = w.get("cap")
        th = min(th, cap) if cap else th
        total, used = w.get("mem_total") or 0, w.get("mem_used") or 0
        pcs.append((th, (total - used) / 1048576 if total else None))
    asleep = 0
    try:
        for nd in _http("GET", "/api/v1/nodes", timeout=10).get("nodes", []):
            if nd.get("state") == "active" and not nd.get("online") and (allowed is None or nd["node_id"] in allowed):
                asleep += 1
    except ClusterError:
        pass
    return pcs, asleep, rtt, len(c.get("jobs") or ([c["job"]] if c.get("job") else []))


def _plan(fn, items, mode, o, profile=None):
    n = len(items)
    per, peak_mb = profile or (_profile(fn, mode, items) if n else (0.0, 0.0))
    cpus = os.cpu_count() or 1
    procs = min(cpus, n) if per * n > 1.0 else 1
    local_s = n * per / procs + (0.2 if procs > 1 else 0)
    mem_mb = o["mem_mb"] or max(1, math.ceil(peak_mb * 1.5))
    p = dict(n=n, item_s=per, mem_mb=mem_mb, local_s=local_s, local_procs=procs, cluster_s=None, pcs=0, where="local",
             placement=o["placement"], priority=o["priority"], reasons=[], profile=(per, peak_mb))
    if o["local"] == "always" or (n < 2 and o["local"] != "never"):
        p["reasons"].append("local forced" if o["local"] == "always" else "one item")
        return Plan(**p)
    try:
        pcs, asleep, rtt, running = _cluster_view(o)
    except ClusterError as e:
        if o["local"] == "never":
            raise
        p["reasons"].append(f"head unreachable ({e})")
        return Plan(**p)
    # per PC: copies that fit in free RAM x 0.8, at most one per thread
    conc = sorted((min(th, int(free * 0.8 // mem_mb)) if free is not None else th for th, free in pcs), reverse=True)
    conc = [c for c in conc if c > 0]
    overhead = 1.0 + 2 * rtt
    best_k, best_t, prev = 0, float("inf"), float("inf")
    for k in range(1, len(conc) + 1):
        t = overhead + n * per / sum(conc[:k])
        if k > 1 and t > prev * 0.95:
            break  # each extra PC must cut the estimate by >= 5 %
        best_k, best_t, prev = k, t, t
    if best_k:
        by_wall = math.ceil(n * per / (o["target_wall_s"] * (sum(conc) / len(conc))))
        best_k = max(1, min(best_k, by_wall))
        best_t = overhead + n * per / sum(conc[:best_k])
    if o["pcs"]:
        best_k = min(o["pcs"], len(conc)) or best_k
        best_t = overhead + n * per / sum(conc[:best_k]) if best_k else best_t
    p["cluster_s"] = best_t if best_k else None
    if not best_k:
        p["reasons"].append(f"no awake PC has room for {mem_mb} MB per item" if pcs else "no awake PCs")
        if o["local"] == "never":
            raise ClusterError("local='never' but " + p["reasons"][-1])
        return Plan(**p)
    if o["local"] != "never" and not (local_s >= 1.5 * best_t):
        p["reasons"].append(f"cluster would be {local_s / best_t:.2f}x faster, under the 1.5x needed")
        return Plan(**p)
    p.update(where="cluster", pcs=best_k)
    if o["placement"] == "auto":
        p["placement"] = "fastest" if best_t < 60 else "efficient" if o["priority"] == "background" else "spread"
    if asleep:
        p["reasons"].append(f"{asleep} PCs asleep (woken only if the head needs them)")
    if running:
        p["reasons"].append(f"{running} other job(s) running")
    p["reasons"].append(f"{mem_mb} MB per item")
    return Plan(**p)


def plan(fn, items, **kw):
    """What the optimizer would do and why, without running the work."""
    items = list(items)
    return _plan(getattr(fn, "_ck_fn", fn), items, "call", _opts(kw))


# ---------------------------------------------------------------- run
def _submit(fn, mode, items, pl, o):
    body = {"kind": "list", "cmd": _cmd(_payload(fn, mode)), "items": _encode(items), "name": o["name"] or f"sdk:{fn.__name__}",
            "priority": o["priority"], "max_pcs": pl.pcs, "placement": pl.placement, "mem_mb": pl.mem_mb}
    if o["selector"]:
        body["selector"] = o["selector"]
    return _http("POST", "/api/v1/jobs", body)["job"]["id"]


def _await(jid, timeout_s):
    t0, delay = time.time(), 0.1
    while True:
        j = _http("GET", f"/api/v1/jobs/{jid}")["job"]
        if j["state"] == "done":
            return
        if j["state"] in ("failed", "cancelled"):
            raise ClusterError(f"job {jid} {j['state']}: {j.get('error')}")
        if timeout_s and time.time() - t0 > timeout_s:
            try:
                _http("POST", f"/api/v1/jobs/{jid}/cancel", {})
            except ClusterError:
                pass
            raise ClusterError(f"job {jid} timed out after {timeout_s} s and was cancelled")
        time.sleep(delay)
        delay = min(delay * 1.5, 1.0)


def _cluster_run(fn, mode, items, pl, o):
    for attempt in range(o["retries"] + 1):
        try:
            jid = _submit(fn, mode, items, pl, o)
            _await(jid, o["timeout_s"])
            text = _http("GET", f"/api/v1/jobs/{jid}/result", text=True, timeout=300)
            break
        except ClusterError:
            if attempt == o["retries"]:
                raise
    lines = [l for l in text.split("\n") if l]
    if len(lines) != len(items):
        raise ClusterError(f"job {jid} returned {len(lines)} results for {len(items)} items")
    out = []
    for l in lines:
        tag, v = _runner.dec(l)
        if tag == "err":
            raise RemoteError(v)
        out.append(v)
    return out


def _run(fn, mode, items, o, pl=None):
    fn = getattr(fn, "_ck_fn", fn)
    if os.environ.get("CK_JOB"):  # already on a PC (the user's module ran this call at import): never recurse
        return _local(fn, mode, items)
    if not items:
        return []
    try:
        _describe(fn)
    except ValueError as e:
        if o["local"] == "never":
            raise ClusterError(str(e)) from None
        log.info("running locally: %s", e)
        return _local(fn, mode, items, os.cpu_count() or 1)
    pl = pl or _plan(fn, items, mode, o)
    log.info("%s", pl)
    if pl.where == "local":
        return _local(fn, mode, items, pl.local_procs)
    return _cluster_run(fn, mode, items, pl, o)


def map(fn, items, reduce=None, **kw):
    """Same results, same order as [fn(x) for x in items]; runs on the cluster when that is faster."""
    items = list(items)
    out = _run(fn, "call", items, _opts(kw))
    return reduce(out) if reduce else out


class _Remote:
    def __init__(self, fn, kw):
        self._ck_fn, self._kw = fn, kw
        functools.update_wrapper(self, fn)

    def __call__(self, *a, **k):
        return self._ck_fn(*a, **k)

    def map(self, items, **kw):
        return map(self._ck_fn, items, **{**self._kw, **kw})


def remote(fn=None, **kw):
    """@ck.remote: calling fn is unchanged; fn.map(items) uses the cluster."""
    return _Remote(fn, kw) if fn else (lambda f: _Remote(f, kw))


def sweep(fn, grid, key=None, top=None, minimize=False, **kw):
    """Run fn(**config) for every combination in grid. Returns [{'params','result'}], best first when key is given."""
    names = list(grid)
    cfgs = [dict(zip(names, v)) for v in itertools.product(*(grid[k] for k in names))]
    res = _run(fn, "kw", cfgs, _opts(kw))
    rows = [{"params": c, "result": r} for c, r in zip(cfgs, res)]
    if key:
        rows.sort(key=lambda r: key(r["result"]), reverse=not minimize)
    return rows[:top] if top else rows


def mean_params(ps):
    """Average a list of params: dicts of floats, lists of floats, or plain floats (recursive)."""
    p0 = ps[0]
    if isinstance(p0, dict):
        return {k: mean_params([p[k] for p in ps]) for k in p0}
    if isinstance(p0, (list, tuple)):
        return type(p0)(mean_params(list(c)) for c in zip(*ps))
    return sum(ps) / len(ps)


def train(step, init, shards, rounds=10, average=mean_params, **kw):
    """Model averaging (local SGD): each round every shard runs step(params, shard) on a PC, then results are averaged."""
    shards, o = list(shards), _opts(kw)
    params = init
    pl = _plan(getattr(step, "_ck_fn", step), [(init, s) for s in shards], "star", o) if shards else None
    for r in range(rounds):
        outs = _run(step, "star", [(params, s) for s in shards], o, pl)
        params = average(outs)
    return params
