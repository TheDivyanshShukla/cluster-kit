"""Cluster Kit Python SDK: run ordinary Python work on the lab cluster. Standard library only. See docs/SDK.md."""
import base64, functools, inspect, itertools, json, logging, math, os, resource, sys, threading, time, traceback, urllib.error, urllib.request, zlib
import tracemalloc
import urllib.parse
from . import _runner, _hist

__all__ = ["connect", "config", "map", "remote", "sweep", "train", "plan", "advise", "mean_params", "RemoteError", "ClusterError", "Plan"]
log = logging.getLogger("clusterkit")

_cfg = {"head": os.environ.get("CK_HEAD", "http://127.0.0.1:7702"), "token": os.environ.get("CK_TOKEN"), "pcs": None,
        "placement": "auto", "priority": "normal", "selector": "", "local": "auto", "mem_mb": None, "timeout_s": None,
        "retries": 0, "name": None, "ship": [], "push_max_kb": 64, "max_item_bytes": 4096, "target_wall_s": 10.0,
        # self-optimizing runtime (docs/SDK.md section 6): defaults come from `python -m clusterkit bench`
        "batch": True, "batch_s": 0.5, "push": True, "adaptive": True, "replan_min_s": 2.0, "history": True,
        "learn_last": 20, "fixed_s": 0.1, "item_c": 1e-5, "start_s": 0.03}
_OPTS = ("pcs", "placement", "priority", "selector", "local", "mem_mb", "timeout_s", "retries", "name", "batch", "adaptive", "history")


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


def _payload(fn, mode, batch=False):
    name, src, q = _describe(fn)
    mods = []
    for p in _cfg["ship"]:
        with open(p, encoding="utf-8") as f:
            mods.append([os.path.splitext(os.path.basename(p))[0], f.read()])
    mods.append([name, src])
    raw = json.dumps({"mods": mods, "main": name, "attr": q, "mode": mode, "batch": batch}).encode()
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


_cmds = {}


def _cmd_for(fn, mode, batch):
    """The pushed command for fn, built once per session (until the source file changes). Returns (cmd, build seconds)."""
    t0 = time.perf_counter()
    fn = getattr(fn, "_ck_fn", fn)
    p = getattr(sys.modules.get(fn.__module__), "__file__", None)
    try:
        st = os.stat(p)
        stamp = (st.st_mtime_ns, st.st_size)
    except (OSError, TypeError):
        stamp = None
    key = (p, stamp, fn.__qualname__, mode, batch, tuple(_cfg["ship"]), _cfg["push_max_kb"])
    if stamp and key in _cmds:
        return _cmds[key], 0.0
    cmd = _cmd(_payload(fn, mode, batch))
    if stamp:
        _cmds[key] = cmd
    return cmd, time.perf_counter() - t0


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


def _model(o, rtt):
    """Overhead constants for the estimate: learned from history.jsonl when there is enough of it, else the defaults."""
    m = _hist.fit(_cfg["head"], o["learn_last"]) if o["history"] else None
    d = {"fixed_s": o["fixed_s"] + 2 * rtt, "item_c": o["item_c"], "speed": 1.0, "runs": 0, "learned": False}
    if m:
        d.update(learned=True, runs=m["runs"], fixed_s=m["fixed_s"])
        for k in ("item_c", "speed"):
            if m[k] is not None:
                d[k] = m[k]
    return d


def _batch_size(per, n, slots, o):
    """Items per remote call: about batch_s of work each, but at least ~2 calls per slot so every slot stays busy."""
    if not o["batch"] or n < 2 or per <= 0:
        return 1
    return max(1, min(round(o["batch_s"] / per), math.ceil(n / (2 * max(slots, 1)))))


def _plan(fn, items, mode, o, profile=None):
    n = len(items)
    per, peak_mb = profile or (_profile(fn, mode, items) if n else (0.0, 0.0))
    cpus = os.cpu_count() or 1
    procs = min(cpus, n) if per * n > 1.0 else 1
    local_s = n * per / procs + (0.2 if procs > 1 else 0)
    mem_mb = o["mem_mb"] or max(1, math.ceil(peak_mb * 1.5))
    p = dict(n=n, item_s=per, mem_mb=mem_mb, local_s=local_s, local_procs=procs, cluster_s=None, pcs=0, where="local",
             placement=o["placement"], priority=o["priority"], reasons=[], profile=(per, peak_mb), batch=1, learned=False,
             model=None, slots=0, all_slots=0)
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
    m = p["model"] = _model(o, rtt)
    p["learned"] = m["learned"]
    if m["learned"]:
        p["reasons"].append(f"overheads learned from {m['runs']} earlier runs (fixed {m['fixed_s']:.2f} s, PC speed {m['speed']:.2f}x)")
    # per PC: copies that fit in free RAM x 0.8, at most one per thread
    conc = sorted((min(th, int(free * 0.8 // mem_mb)) if free is not None else th for th, free in pcs), reverse=True)
    conc = [c for c in conc if c > 0]
    p["all_slots"] = sum(conc)
    def cost(k):  # fixed + per item + compute spread over the slots + one python start per remote call
        sl = sum(conc[:k])
        return m["fixed_s"] + n * m["item_c"] + (n * per + math.ceil(n / _batch_size(per, n, sl, o)) * o["start_s"]) / (m["speed"] * sl)
    best_k, best_t, prev = 0, float("inf"), float("inf")
    for k in range(1, len(conc) + 1):
        t = cost(k)
        if k > 1 and t > prev * 0.95:
            break  # each extra PC must cut the estimate by >= 5 %
        best_k, best_t, prev = k, t, t
    if best_k:
        by_wall = math.ceil(n * per / (o["target_wall_s"] * m["speed"] * (sum(conc) / len(conc))))
        best_k = max(1, min(best_k, by_wall))
        best_t = cost(best_k)
    if o["pcs"]:
        best_k = min(o["pcs"], len(conc)) or best_k
        best_t = cost(best_k) if best_k else best_t
    p["cluster_s"] = best_t if best_k else None
    if not best_k:
        p["reasons"].append(f"no awake PC has room for {mem_mb} MB per item" if pcs else "no awake PCs")
        if o["local"] == "never":
            raise ClusterError("local='never' but " + p["reasons"][-1])
        return Plan(**p)
    p["slots"] = sum(conc[:best_k])
    if o["local"] != "never" and not (local_s >= 1.5 * best_t):
        p["reasons"].append(f"cluster would be {local_s / best_t:.2f}x faster, under the 1.5x needed")
        return Plan(**p)
    p.update(where="cluster", pcs=best_k, batch=_batch_size(per, n, p["slots"], o))
    if o["placement"] == "auto":
        p["placement"] = "fastest" if best_t < 60 else "efficient" if o["priority"] == "background" else "spread"
    if asleep:
        p["reasons"].append(f"{asleep} PCs asleep (woken only if the head needs them)")
    if running:
        p["reasons"].append(f"{running} other job(s) running")
    if p["batch"] > 1:
        p["reasons"].append(f"{p['batch']} items per remote call")
    p["reasons"].append(f"{mem_mb} MB per item")
    return Plan(**p)


def plan(fn, items, **kw):
    """What the optimizer would do and why, without running the work."""
    items = list(items)
    return _plan(getattr(fn, "_ck_fn", fn), items, "call", _opts(kw))


def advise(fn, items, sample=20, **kw):
    """Ranked suggestions to make fn faster on the cluster (or to tell you it cannot help). See docs/SDK.md section 7."""
    from . import _advise
    return _advise.advise(getattr(fn, "_ck_fn", fn), list(items), sample, _opts(kw))


# ---------------------------------------------------------------- results pushed by the head (SSE), polling as the fallback
_TERMINAL = ("done", "failed", "cancelled")


def _stream_open(head, token):
    req = urllib.request.Request(head + "/api/v1/stream", headers={"Accept": "text/event-stream"})
    if token:
        req.add_header("Authorization", "Bearer " + token)
    return urllib.request.urlopen(req, timeout=20)  # the head sends a `cluster` event every second, so 20 s of silence is a dead link


class _Stream:
    """One background connection to /api/v1/stream per session; keeps the newest JobRow of every job it sees."""

    def __init__(self):
        self.cv, self.rows, self.live, self.gen, self.head = threading.Condition(), {}, False, 0, None
        self.arrived, self.wins, self.judge = {}, [], None

    def trusted(self):
        """Do pushed events beat a poll? Judged on the last runs: a head that buffers its stream (as today's does, up to
        2 s) loses, so we keep polling fast; a head that flushes each event wins, and polling drops to a 3 s safety net."""
        if self.judge:
            jid, t = self.judge
            self.judge = None
            with self.cv:
                self.wins.append(self.arrived.get(jid, 1e18) - t < 0.15)
            del self.wins[:-5]
        return len(self.wins) >= 3 and sum(self.wins) >= 3

    def ensure(self):
        if not _cfg["push"]:
            return False
        if self.head != _cfg["head"] or not getattr(self, "thread", None) or not self.thread.is_alive():
            self.head, self.gen, self.live = _cfg["head"], self.gen + 1, False
            self.thread = threading.Thread(target=self._run, args=(self.head, self.gen), daemon=True)
            self.thread.start()
        return self.live  # never waits: until the stream is connected (the head may send its headers late), polling covers

    def _run(self, head, gen):
        while self.gen == gen:
            try:
                r = _stream_open(head, _cfg["token"])
                with self.cv:
                    self.live = True
                    self.cv.notify_all()
                ev = None
                for raw in r:
                    if self.gen != gen:
                        break
                    s = raw.decode(errors="replace").strip() if isinstance(raw, bytes) else raw.strip()
                    if s.startswith("event:"):
                        ev = s[6:].strip()
                    elif s.startswith("data:") and ev in ("job", "cluster"):
                        d = json.loads(s[5:])
                        with self.cv:
                            for row in ([d] if ev == "job" else d.get("jobs") or []):
                                old = self.rows.get(row.get("id"))
                                if row.get("id") is not None and not (old and old.get("state") in _TERMINAL and row.get("state") not in _TERMINAL):
                                    self.rows[row["id"]] = row
                                    if row.get("state") in _TERMINAL:
                                        self.arrived.setdefault(row["id"], time.time())
                            while len(self.rows) > 256:
                                self.arrived.pop(self.rows.pop(next(iter(self.rows))).get("id"), None)
                            self.cv.notify_all()
            except Exception:
                pass
            with self.cv:
                self.live = False
                self.cv.notify_all()
            time.sleep(1.0)


_stream = _Stream()
_last = {}  # plan, timings and wall time of the latest cluster run (python -m clusterkit bench reads it)


class _Replan(Exception):
    pass


def _await(jid, timeout_s, check=None, st=None):
    """Wait for the job; returns its final row. Events from the stream wake us at once; without a live stream, poll."""
    t0, delay, polled = time.time(), 0.03, time.time()
    safety = 3.0 if _stream.live and _cfg["push"] and _stream.trusted() else None
    while True:
        gap = safety or delay  # not trusted: poll on a quick backoff (0.03 s up to 0.3 s); pushed events still wake us early
        live, now = _stream.live and _cfg["push"], time.time()
        if live and now - polled < gap:  # a silent job is still polled now and then as a safety net
            row, via = _stream.rows.get(jid), "push"
        else:
            row, polled, via = _http("GET", f"/api/v1/jobs/{jid}")["job"], now, "poll"
        state = row and row.get("state")
        if state == "done":
            if st is not None:
                st["push"] = via == "push"
            if live and via == "push":
                _stream.wins.append(True)
                del _stream.wins[:-5]
            elif live and (_stream.rows.get(jid) or {}).get("state") not in _TERMINAL:
                _stream.judge = (jid, time.time())  # did the event follow right behind? judged at the next job
            return row
        if state in ("failed", "cancelled"):
            raise ClusterError(f"job {jid} {state}: {row.get('error')}")
        if timeout_s and now - t0 > timeout_s:
            try:
                _http("POST", f"/api/v1/jobs/{jid}/cancel", {})
            except ClusterError:
                pass
            raise ClusterError(f"job {jid} timed out after {timeout_s} s and was cancelled")
        if check and row and state == "running":
            check(row, now - t0)
        if live:
            with _stream.cv:
                _stream.cv.wait_for(lambda: (_stream.rows.get(jid) or {}).get("state") in _TERMINAL, min(gap, 0.5))
        else:
            time.sleep(delay)
        delay = min(delay * 1.6, 0.3)


# ---------------------------------------------------------------- run
def _groups(items, size, limit):
    """Batches of up to `size` items, each encoded as one list item <= limit bytes (split in half when too big)."""
    out = []

    def put(chunk):
        s = _runner.enc(chunk)
        if len(s) > limit and len(chunk) > 1:
            h = len(chunk) // 2
            put(chunk[:h])
            put(chunk[h:])
        elif len(s) > limit:
            raise ClusterError(f"an item is {len(s)} bytes encoded, over max_item_bytes={limit}; raise it with "
                               f"`ck admin tune max_item_bytes {len(s) + 64}` and ck.config(max_item_bytes=...), "
                               "or pass paths every PC can read (or a seed) instead of the data itself")
        else:
            out.append(s)

    for i in range(0, len(items), size):
        put(items[i:i + size])
    return out


def _submit(fn, mode, items, pl, o, st):
    t0 = time.perf_counter()
    batch = pl.batch > 1
    lines = _groups(items, pl.batch, _cfg["max_item_bytes"]) if batch else _encode(items)
    st["encode_s"] = time.perf_counter() - t0
    cmd, st["ship_s"] = _cmd_for(fn, mode, batch)
    st["cmd_bytes"] = len(cmd)
    body = {"kind": "list", "cmd": cmd, "items": lines, "name": o["name"] or f"sdk:{fn.__name__}",
            "priority": o["priority"], "max_pcs": pl.pcs, "placement": pl.placement, "mem_mb": pl.mem_mb}
    # Chunk size cap: about 1 s of work per thread and at least 3 chunks per PC. Today's head ignores `chunk` under the default
    # adaptive policy (it sizes chunks from the PCs' rate on earlier jobs, which hands a PC far too many slow items after fast
    # ones); a head that treats it as a cap fixes that (docs/SDK.md section 8). Never use policy fixed/guided: measured 2-3x slower.
    th = max(1, round(pl.slots / max(pl.pcs, 1)))
    body["chunk"] = st["chunk"] = max(1, min(th * max(1, round(1.0 / max(pl.item_s * pl.batch, 1e-6))), math.ceil(len(lines) / (3 * max(pl.pcs, 1)))))
    if o["selector"]:
        body["selector"] = o["selector"]
    t0 = time.perf_counter()
    jid = _http("POST", "/api/v1/jobs", body)["job"]["id"]
    st["submit_s"] = time.perf_counter() - t0
    st["lines"] = len(lines)
    return jid


def _checker(pl, o, items, st):
    """Called while the job runs: finish locally when the cluster is losing (actual progress vs the plan)."""
    local_ok = o["adaptive"] and o["local"] != "never" and not os.environ.get("CK_JOB")
    n = len(items)

    def check(row, elapsed):
        total, done = row.get("total") or n, row.get("done") or 0
        if not local_ok or elapsed < o["replan_min_s"]:
            return
        f = done / total if total else 0.0
        # time left on the cluster at the observed pace (or, with no progress at all, at least as long as it has taken so far)
        left = elapsed * (1 - f) / f if f > 0 else max((pl.cluster_s or 0) - elapsed, 0) + elapsed
        st["replan_seen"] = (round(elapsed, 2), round(f, 3), round(left, 2))
        # the cluster's finished results would be thrown away, so compare against the whole local time
        if left > 1.25 * pl.local_s:
            raise _Replan(f"replan: the cluster is at {f:.0%} after {elapsed:.1f} s and needs about {left:.1f} s more "
                          f"(plan said {pl.cluster_s:.1f} s in total); finishing locally in about {pl.local_s:.1f} s")
    return check


def _cluster_run(fn, mode, items, pl, o, st):
    st.update(attempts=0)
    _stream.ensure()
    for attempt in range(o["retries"] + 1):
        st["attempts"] += 1
        jid = None
        try:
            jid = _submit(fn, mode, items, pl, o, st)
            row = _await(jid, o["timeout_s"], _checker(pl, o, items, st), st)
            st["detect_t"] = time.time()
            t0 = time.perf_counter()
            text = _http("GET", f"/api/v1/jobs/{jid}/result", text=True, timeout=300)
            st["fetch_s"] = time.perf_counter() - t0
            break
        except _Replan:
            try:
                _http("POST", f"/api/v1/jobs/{jid}/cancel", {})
            except ClusterError:
                pass
            raise
        except ClusterError:
            if attempt == o["retries"]:
                raise
    c, s, f = row.get("created_ms"), row.get("started_ms"), row.get("finished_ms")
    if c and s and f:
        st["queue_s"], st["run_s"] = max(0.0, (s - c) / 1000), max(0.0, (f - s) / 1000)
        d = st["detect_t"] - f / 1000
        st["detect_s"] = d if 0 <= d < 10 else None  # head and this PC may disagree on the time of day
    lines = [l for l in text.split("\n") if l]
    if len(lines) != st["lines"]:
        raise ClusterError(f"job {jid} returned {len(lines)} results for {st['lines']} items")
    t0 = time.perf_counter()
    out = []
    for l in lines:
        r = _runner.dec(l)
        for tag, v in (r if pl.batch > 1 and isinstance(r, list) else [r]):  # a failed batch line is a lone ("err", tb)
            if tag == "err":
                raise RemoteError(v)
            out.append(v)
    st["decode_s"] = time.perf_counter() - t0
    if len(out) != len(items):
        raise ClusterError(f"job {jid} returned {len(out)} results for {len(items)} items")
    st["job"] = jid
    return out


def _learn(st, pl, n, actual, where, o, fn):
    """Append predicted vs actual to history.jsonl (the next plan() fits its constants from it)."""
    if not o["history"]:
        return
    rec = {"head": _cfg["head"], "fn": getattr(fn, "__qualname__", "?"), "n": n, "where": where, "pcs": pl.pcs, "slots": pl.slots,
           "batch": pl.batch, "item_s": round(pl.item_s, 6), "pred_local_s": round(pl.local_s, 3),
           "pred_s": round(pl.cluster_s, 3) if pl.cluster_s else None, "actual_s": round(actual, 3), "learned": pl.learned}
    if where == "cluster" and st.get("run_s") is not None:
        item_c = (st["encode_s"] + st.get("fetch_s", 0) + st.get("decode_s", 0)) / max(n, 1)
        rec.update(run_s=round(st["run_s"], 3), queue_s=round(st["queue_s"], 3), submit_s=round(st["submit_s"], 3),
                   ship_s=round(st["ship_s"], 4), detect_s=round(st["detect_s"], 3) if st.get("detect_s") is not None else None,
                   fetch_s=round(st["fetch_s"], 3), decode_s=round(st["decode_s"], 3), encode_s=round(st["encode_s"], 3),
                   item_c=round(item_c, 7), push=st.get("push"),
                   fixed_s=round(max(0.0, actual - st["run_s"] - n * item_c), 3))
        if pl.slots and pl.item_s > 0:
            rec["speed"] = round(n * pl.item_s / (max(st["run_s"], 1e-6) * pl.slots), 4)  # per-slot throughput vs this PC
    elif where == "replanned":
        seen = st.get("replan_seen")
        rec.update(failed=True, replan=seen)
        if seen and seen[1] > 0 and pl.slots and pl.item_s > 0:  # what the cluster actually delivered per slot before we gave up
            rec.update(run_s=seen[0], speed=round(seen[1] * n * pl.item_s / (seen[0] * pl.slots), 4))
    _hist.record(rec)


def _run(fn, mode, items, o, pl=None, learn=True):
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
    global _last
    t0, st = time.perf_counter(), {}
    _last = {"plan": pl, "st": st}
    if pl.where == "local":
        out = _local(fn, mode, items, pl.local_procs)
        if learn:
            _learn(st, pl, len(items), time.perf_counter() - t0, "local", o, fn)
        return out
    try:
        out = _cluster_run(fn, mode, items, pl, o, st)
    except _Replan as r:
        log.warning("%s", r)
        _learn(st, pl, len(items), time.perf_counter() - t0, "replanned", o, fn)
        return _local(fn, mode, items, pl.local_procs)
    _last["wall"] = time.perf_counter() - t0
    if learn:
        _learn(st, pl, len(items), _last["wall"], "cluster", o, fn)
    return out


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
        outs = _run(step, "star", [(params, s) for s in shards], o, pl, learn=r == 0)
        params = average(outs)
    return params
