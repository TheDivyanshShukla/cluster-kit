"""Cluster Kit Python SDK: run ordinary Python work on the lab cluster. Standard library only. See docs/SDK.md."""
from __future__ import annotations
import ast, base64, functools, itertools, json, logging, math, os, pickle, resource, sys, threading, time, urllib.error, urllib.request, zlib
import tracemalloc
import urllib.parse
from dataclasses import dataclass, field
from typing import Any, Callable, Generic, Literal, TypedDict, TypeVar, overload
from collections.abc import Iterable, Sequence
from . import _runner, _hist, _bulk, _benchfn
from ._bulk import Handle
from ._advise import Advice, Suggestion

T = TypeVar("T")
R = TypeVar("R")
S = TypeVar("S")
M = TypeVar("M")  # model parameters in train()
Placement = Literal["auto", "spread", "pack", "fastest", "efficient"]
Priority = Literal["urgent", "high", "normal", "low", "background"]
LocalMode = Literal["auto", "always", "never"]
Opts = dict[str, Any]
if sys.version_info >= (3, 10):
    from typing import ParamSpec
    P = ParamSpec("P")
else:  # Python 3.9 has no ParamSpec (and typing_extensions is not allowed): Remote keeps the result type, not the arguments
    P = TypeVar("P")

__all__ = ["connect", "config", "map", "remote", "sweep", "train", "put", "shard", "plan", "Handle", "Suggestion", "Advice", "SweepRow", "Remote", "advise", "mean_params", "pc_python", "RemoteError", "ClusterError", "Plan"]
log = logging.getLogger("clusterkit")

_cfg: Opts = {"head": os.environ.get("CK_HEAD", "http://127.0.0.1:7702"), "token": os.environ.get("CK_TOKEN"), "pcs": None,
        "placement": "auto", "priority": "normal", "selector": "", "local": "auto", "mem_mb": None, "timeout_s": None,
        "retries": 0, "name": None, "ship": [], "push_max_kb": 64, "max_item_bytes": 4096, "target_wall_s": 10.0,
        # self-optimizing runtime (docs/SDK.md section 6): defaults come from `python -m clusterkit bench`
        "batch": True, "batch_s": 0.5, "push": True, "adaptive": True, "replan_min_s": 2.0, "history": True,
        "learn_last": 20, "fixed_s": 0.1, "item_c": 1e-5, "start_s": 0.03,
        # bulk distribution (docs/BULK.md): blob_auto_kb mirrors the head knob of the same name; result_head is how the PCs reach the head
        "pc_python": None, "bulk": True, "blob_per_chunk": True, "blob_auto_kb": 64, "code_blob_kb": 8, "bulk_results": True, "result_head": None}
_OPTS = ("pcs", "placement", "priority", "selector", "local", "mem_mb", "timeout_s", "retries", "name", "batch", "adaptive", "history", "blob_per_chunk", "bulk")


class ClusterError(Exception):
    """The head refused or failed the job."""


class RemoteError(Exception):
    """fn raised on a PC; .remote_traceback has the traceback from there."""

    def __init__(self, tb: str) -> None:
        super().__init__("remote call failed:\n" + tb)
        self.remote_traceback = tb


def connect(head: str | None = None, token: str | None = None) -> None:
    _cfg["head"] = (head or _cfg["head"]).rstrip("/")
    if token is not None:
        _cfg["token"] = token


def config(*, placement: Placement | None = None, priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> None:
    """Set session defaults (any option of docs/SDK.md section 1)."""
    kw = _lit(kw, placement, priority, local)
    bad = set(kw) - set(_cfg)
    if bad:
        raise TypeError("unknown option(s): " + ", ".join(sorted(bad)))
    if kw.get("pc_python") is not None:
        _ver(kw["pc_python"])  # fail now on a typo, not at the first job
    _cfg.update(kw)
    _runner.PROTO = _proto()


def _ver(v: str | tuple[int, ...]) -> tuple[int, int]:
    """'3.9' / '3.9.6' / (3, 9) -> (3, 9)."""
    try:
        t = tuple(int(x) for x in (v.split(".") if isinstance(v, str) else v))[:2]
        if len(t) == 2 and t[0] == 3:
            return t[0], t[1]
    except (ValueError, TypeError):
        pass
    raise TypeError(f"pc_python must look like '3.9' (the oldest Python 3 on any PC), not {v!r}")


def _proto() -> int:
    """Pickle protocol both sides read: the lower HIGHEST_PROTOCOL of this machine and the oldest PC (3.8+ -> 5, 3.4-3.7 -> 4), capped at 5."""
    pc = _cfg["pc_python"]
    return min(pickle.HIGHEST_PROTOCOL, 5, 5 if pc is None or _ver(pc) >= (3, 8) else 4)


def _lit(kw: dict[str, Any], placement: str | None, priority: str | None, local: str | None) -> dict[str, Any]:
    """The three Literal-typed options, folded into the keyword options."""
    return {**kw, **{k: v for k, v in (("placement", placement), ("priority", priority), ("local", local)) if v is not None}}


def _opts(kw: dict[str, Any]) -> Opts:
    bad = set(kw) - set(_OPTS)
    if bad:
        raise TypeError("unknown option(s): " + ", ".join(sorted(bad)))
    return {**_cfg, **{k: v for k, v in kw.items() if v is not None}}


# ---------------------------------------------------------------- HTTP
_in_job = [0]  # cluster runs in flight: while > 0, GET/HEAD retry through a head failover
_reconnected = [False]  # set when such a retry got through: the head may be a new one that lacks the blobs this run uploaded
def _http(method: str, path: str, body: Any = None, text: bool = False, timeout: float = 60, raw: bool = False, length: int | None = None) -> Any:
    """JSON in/out. A bytes (or file-like, with length=) body is sent as is (blob upload); raw=True returns bytes; HEAD returns {}."""
    blob = isinstance(body, (bytes, bytearray)) or hasattr(body, "read")
    req = urllib.request.Request(_cfg["head"] + path, method=method, data=body if blob else None if body is None else json.dumps(body).encode())
    if body is not None:
        req.add_header("Content-Type", "application/octet-stream" if blob else "application/json")
        if hasattr(body, "read"):
            req.add_header("Content-Length", str(length))
    if _cfg["token"]:
        req.add_header("Authorization", "Bearer " + _cfg["token"])
    # while a job runs, reads wait for a failover: the standby takes over the same address within seconds and keeps the job (CK_FAILOVER_SECS, default 60)
    deadline = time.monotonic() + (float(os.environ.get("CK_FAILOVER_SECS") or 60) if _in_job[0] and method in ("GET", "HEAD") else 0.0)
    tries = 0
    while True:
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                data = r.read()
            if tries:
                _reconnected[0] = True
            break
        except urllib.error.HTTPError as e:
            raise ClusterError(f"{method} {path}: HTTP {e.code} {e.read()[:300].decode(errors='replace')}") from None
        except OSError as e:
            if time.monotonic() >= deadline:
                raise ClusterError(f"cannot reach the head at {_cfg['head']} ({e}); call ck.connect(url) or set CK_HEAD") from None
            tries += 1
            time.sleep(0.5)
    if raw:
        return data
    return {} if method == "HEAD" else data.decode() if text else json.loads(data)


# ---------------------------------------------------------------- code shipping
def _describe(fn: Callable[..., Any]) -> tuple[str, str, str]:
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


def _payload(fn: Callable[..., Any], mode: str, batch: bool = False, gl: dict[str, str] | None = None, rb: dict[str, Any] | None = None) -> str:
    name, src, q = _describe(fn)
    mods = []
    for p in _cfg["ship"]:
        with open(p, encoding="utf-8") as f:
            mods.append([os.path.splitext(os.path.basename(p))[0], f.read()])
    mods.append([name, src])
    if _cfg["pc_python"] is not None:
        for n, code in mods:  # newer syntax than the PCs' Python fails here with a clear message, not on every PC
            try:
                ast.parse(code, n + ".py", feature_version=_ver(_cfg["pc_python"]))
            except SyntaxError as e:
                raise ClusterError(f"{n}.py line {e.lineno}: {e.msg}: not valid for pc_python={_cfg['pc_python']} (the oldest Python on the PCs); "
                                   "avoid the newer syntax or upgrade the PCs and ck.config(pc_python=...)") from None
    raw = json.dumps({"mods": mods, "main": name, "attr": q, "mode": mode, "batch": batch, "proto": _proto(), **({"gl": gl} if gl else {}), **({"rb": rb} if rb else {})}).encode()
    return base64.b64encode(zlib.compress(raw, 9)).decode()


def _cmd(payload: str, sha: str | None = None, batch: bool = False) -> str:
    with open(_runner.__file__, encoding="utf-8") as f:
        runner = f.read()
    script = f"BATCH = {batch!r}\n" + (f"PAYLOAD_BLOB = {sha!r}\n" if sha else f"PAYLOAD = {payload!r}\n") + runner
    limit = _cfg["push_max_kb"] * 1024
    if len(script) > limit:
        raise ClusterError(f"the shipped code is {len(script)} bytes, over the push limit of {limit}; "
                           f"raise it with `ck admin tune push_max_kb {len(script) // 1024 + 1}` and ck.config(push_max_kb=...)")
    return "\x01PUSH\npython3\nck_runner.py\n" + script


_cmds: dict[Any, Any] = {}
_self = sys.modules[__name__]


def _big_globals(fn: Callable[..., Any], thr: int) -> dict[str, Handle[Any]]:
    """Module-level data fn reads (not functions/classes/modules) whose pickle is over thr: {name: Handle}."""
    out: dict[str, Handle[Any]] = {}
    names: set[str] = set()
    stack: list[Any] = [getattr(fn, "__code__", None)]
    while stack:
        c = stack.pop()
        if c is not None:
            names |= set(c.co_names)
            stack += [k for k in c.co_consts if hasattr(k, "co_names")]
    g = getattr(fn, "__globals__", {})
    for n in sorted(names):
        v = g.get(n)
        if v is None or callable(v) or isinstance(v, type(sys)) or type(v) in (int, float, bool):
            continue
        try:
            if _bulk._size(v) > thr:
                out[n] = _bulk.from_obj(v)
        except Exception:
            pass
    return out


def _cmd_for(fn: Callable[..., Any], mode: str, batch: bool) -> tuple[str, float, list[Handle[Any]]]:
    """The pushed command for fn, built once per session (until the source file changes). Returns (cmd, build seconds, blob handles).
    With a head that has a blob store: big module-level data and code over code_blob_kb travel as blobs; results above blob_auto_kb come back as blobs."""
    t0 = time.perf_counter()
    fn = getattr(fn, "_ck_fn", fn)
    p = getattr(sys.modules.get(fn.__module__), "__file__", None)
    try:
        st = os.stat(p)
        stamp = (st.st_mtime_ns, st.st_size)
    except (OSError, TypeError):
        stamp = None
    key = (p, stamp, fn.__qualname__, mode, tuple(_cfg["ship"]), _cfg["push_max_kb"], _cfg["head"],
           _cfg["pc_python"], _cfg["bulk"], _cfg["blob_auto_kb"], _cfg["code_blob_kb"], _cfg["bulk_results"], _cfg["result_head"], _cfg["token"])
    if stamp and key in _cmds:
        return _cmd(*_cmds[key][:2], batch), 0.0, _cmds[key][2]
    gl: dict[str, str] | None = None
    hs: list[Handle[Any]] = []
    rb: dict[str, Any] | None = None
    sha: str | None = None
    if _cfg["bulk"] and _bulk.supported(_self):
        big = _big_globals(fn, _cfg["blob_auto_kb"] * 1024)
        gl, hs = {n: h.sha for n, h in big.items()}, list(big.values())
        # Never embed a user API token in a job (job args are visible to viewers, stored and logged). On a head that gives each
        # job its own upload capability (CK_BLOB_PUT_URL / CK_BLOB_PUT_TOKEN in the PC's environment) that is what the runner
        # uses, so bulk results work on secured heads too; an older head gets the plain url, and only when no user token exists.
        if _cfg["bulk_results"]:
            rb = {"kb": _cfg["blob_auto_kb"]}
            if _cfg["result_head"] or not _cfg["token"]:
                rb["url"] = (_cfg["result_head"] or _cfg["head"]).rstrip("/")
    payload = _payload(fn, mode, False, gl, rb)  # batching is a header line of the script, so one payload (and one code blob) serves both
    if _cfg["bulk"] and _bulk._ok.get(_cfg["head"]) and len(payload) > _cfg["code_blob_kb"] * 1024:
        hs.append(_bulk.from_bytes(payload.encode()))
        sha = hs[-1].sha
    cmd = _cmd(payload, sha, batch)
    if stamp:
        _cmds[key] = (payload, sha, hs)
    return cmd, time.perf_counter() - t0, hs


def _refs(x: Any, line: str, many: bool = False) -> str:
    """`@refs:<sha>,..|line` when the item (or, `many`, any item of a batch) holds blob handles: the head then sends each blob only to the PCs that get the item."""
    shas = sorted({h.sha for h in (_bulk.find(x) if not many else [h for y in x for h in _bulk.find(y)])})
    return "@refs:" + ",".join(shas) + "|" + line if shas else line


def _encode(items: Sequence[Any]) -> list[str]:
    lines = [("@blob:" + x.sha) if isinstance(x, _bulk.Handle) else _refs(x, _runner.enc(x)) for x in items]  # the head swaps `@blob:` for a path, and places by it
    lim = _cfg["max_item_bytes"]
    for i, s in enumerate(lines):
        if len(s) > lim:
            raise ClusterError(f"item {i} is {len(s)} bytes encoded, over max_item_bytes={lim}; raise it with "
                               f"`ck admin tune max_item_bytes {len(s) + 64}` and ck.config(max_item_bytes=...), "
                               "or pass paths every PC can read instead of the data itself, or ck.put(data) / ck.shard(path, parts=k) "
                               "(a head with a blob store sends big arguments as blobs by itself)")
    return lines


@overload
def put(x: os.PathLike[str]) -> Handle[str]: ...
@overload
def put(x: T) -> Handle[T]: ...
def put(x: Any) -> Handle[Any]:
    """Upload x (an object, or a file path) to the head once and return a Handle. Same bytes twice are one blob. Use the handle in
    items or arguments: fn receives the object, or the local file path for a file."""
    h = _bulk.make(x)
    _bulk.upload([h], _self)
    return h


@overload
def shard(src: str | os.PathLike[str], parts: int) -> list[Handle[str]]: ...
@overload
def shard(src: Iterable[T], parts: int) -> list[Handle[list[T]]]: ...
def shard(src: Any, parts: int) -> list[Handle[Any]]:
    """Split a file (at line ends) or a list into `parts` blobs and return their handles: ck.map(fn, ck.shard(path, 50)).
    fn gets the path of one shard file (or one slice of the list); each shard travels to the PC that runs it, once."""
    hs = _bulk.shard(src, parts)
    _bulk.upload(hs, _self)
    return hs


# ---------------------------------------------------------------- local execution
def _lw(a: tuple[str, Callable[..., Any], Any]) -> Any:
    return _runner.apply(*a)


def _mp_context() -> Any:
    """A multiprocessing context that works on this Python, or None (run serially). 3.14 made forkserver the default on Linux and
    macOS was already spawn: fn and the items then travel by pickle and the child re-imports your script, so a script without an
    `if __name__ == "__main__":` guard would run its top level again in every child (an endless pool) and is kept serial."""
    import multiprocessing as mp
    methods = mp.get_all_start_methods()
    if "fork" in methods and threading.active_count() == 1:  # fork copies only this thread; with a live stream thread (3.12+ warns) use spawn
        return mp.get_context("fork")
    main = sys.modules.get("__main__")
    path = getattr(main, "__file__", None)
    if path and getattr(getattr(main, "__spec__", None), "name", None) is None:  # a plain script (not python -m): look for the guard
        try:
            with open(path, encoding="utf-8") as f:
                if "__main__" not in f.read():
                    return None
        except OSError:
            return None
    return mp.get_context("forkserver" if "forkserver" in methods else "spawn")


def _local(fn: Callable[..., Any], mode: str, items: list[Any], procs: int = 1) -> list[Any]:
    fn = getattr(fn, "_ck_fn", fn)
    if procs > 1 and len(items) > 1:
        try:
            ctx = _mp_context()
            if ctx is None:
                log.info("local parallel run skipped: guard your script's top level with `if __name__ == '__main__':` to use all cores; running serially")
            else:
                with ctx.Pool(procs) as p:
                    return p.map(_lw, [(mode, fn, x) for x in items], chunksize=max(1, len(items) // (procs * 4)))
        except Exception as e:  # unpicklable fn/item/result or no working start method: serial is always right
            log.info("local parallel run failed (%s); running serially", e)
    return [_runner.apply(mode, fn, x) for x in items]


# ---------------------------------------------------------------- plan
@dataclass(repr=False, eq=False)
class Plan:
    """What the optimizer decided and why (ck.plan). Time fields are seconds; bytes fields are data moved as blobs."""
    n: int = 0
    item_s: float = 0.0
    mem_mb: int = 1
    local_s: float = 0.0
    local_procs: int = 1
    cluster_s: float | None = None
    pcs: int = 0
    where: Literal["local", "cluster"] = "local"
    placement: str = "auto"
    priority: str = "normal"
    reasons: list[str] = field(default_factory=list)
    profile: tuple[float, float] = (0.0, 0.0)
    batch: int = 1
    learned: bool = False
    model: dict[str, Any] | None = None
    slots: int = 0
    all_slots: int = 0
    bytes: int = 0
    shared_bytes: int = 0
    item_bytes: int = 0
    blobs: int = 0
    strategy: str | None = None
    transfer_s: float = 0.0
    transfer_alt: dict[str, float] = field(default_factory=dict)
    finish_by_pcs: dict[int, float] = field(default_factory=dict)
    transfer_src: str | None = None
    code_handles: list[Any] = field(default_factory=list)

    def __str__(self) -> str:
        t = lambda s: f"{s:.1f} s" if s < 120 else f"{s / 60:.1f} min"
        head = (f"{self.n:,} items x {self.item_s:.3g} s: " +
                (f"cluster, {self.pcs} PCs, about {t(self.cluster_s)} (local: {t(self.local_s)})" if self.where == "cluster"
                 else f"local, about {t(self.local_s)} (cluster: {t(self.cluster_s) if self.cluster_s else 'n/a'})"))
        return head + "".join("; " + r for r in self.reasons)

    def transfer(self) -> str:
        """One line on the data movement: bytes, strategy, estimate per PC count."""
        if not self.bytes:
            return "no blobs"
        t = lambda s: f"{s:.1f} s" if s < 120 else f"{s / 60:.1f} min"
        alt = ", ".join(f"{k} {t(v)}" for k, v in self.transfer_alt.items())
        return (f"{_bulk_mb(self.bytes)} to move ({_bulk_mb(self.shared_bytes)} to every PC, {_bulk_mb(self.item_bytes)} split over the PCs): "
                f"{self.strategy}, about {t(self.transfer_s)} on {self.pcs or 'the chosen'} PCs ({alt}); finish by PC count: "
                + ", ".join(f"{k}: {t(v)}" for k, v in self.finish_by_pcs.items()) + f" [{self.transfer_src}]")

    __repr__ = __str__


def _bulk_mb(b: float) -> str:
    return f"{b / 1e9:.2f} GB" if b >= 1e9 else f"{b / 1e6:.1f} MB" if b >= 1e5 else f"{b / 1e3:.1f} KB"


def _profile(fn: Callable[..., Any], mode: str, items: list[Any], limit: float = 30.0) -> tuple[float, float]:
    n = len(items)
    k = max(3, math.ceil(n * 0.01))
    # item 0 runs once under tracemalloc (peak memory; also the only timing when items are slow, so a 2 s item is not run twice);
    # the next ones run untraced and give the time per item. Item 0 runs in a thread so a hung function cannot hang the
    # caller: past `limit` it counts as a `limit`-second item (the plan then sends the work to the cluster, where timeout_s applies).
    rss0 = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    t0 = time.perf_counter()
    box: dict[str, Any] = {}

    def first_item() -> None:
        try:
            _runner.apply(mode, fn, items[0])
        except BaseException as e:  # noqa: BLE001 - re-raised in the caller
            box["err"] = e
        box["peak"] = tracemalloc.get_traced_memory()[1]

    tracemalloc.start()
    th = threading.Thread(target=first_item, daemon=True)
    th.start()
    th.join(limit)
    tracemalloc.stop()
    if th.is_alive():
        log.warning("item 0 still running after %.0f s locally; planning it as a slow item (it keeps running in the background)", limit)
        return limit, 0.0
    if "err" in box:
        raise box["err"]
    peak = box.get("peak", 0)
    first = time.perf_counter() - t0
    scale = 1 if sys.platform == "darwin" else 1024
    rss = (resource.getrusage(resource.RUSAGE_SELF).ru_maxrss - rss0) * scale
    t0, rest = time.perf_counter(), {"done": 0}

    def more_items() -> None:  # untraced timing of a few more items, also in a thread: any one of them may hang
        try:
            for x in items[1:k]:
                _runner.apply(mode, fn, x)
                rest["done"] += 1
                if time.perf_counter() - t0 > max(2.0 - first, 0.0):
                    break
        except BaseException as e:  # noqa: BLE001 - re-raised in the caller
            rest["err"] = e

    if first <= 10.0 and n > 1:  # item 0 ran under tracemalloc and paid one-time costs (imports): even a slow one gets one clean timing from item 1
        th = threading.Thread(target=more_items, daemon=True)
        th.start()
        th.join(min(limit, max(2.0 - first, 0.0) + first + 1.0))
        if th.is_alive():  # an item far slower than item 0: plan with what it has taken so far
            log.warning("a sample item is still running locally after %.1f s; planning the items as slow", time.perf_counter() - t0)
            return max(first, time.perf_counter() - t0), max(peak, rss) / 1048576
        if "err" in rest:
            raise rest["err"]
    done = rest["done"]
    per = (time.perf_counter() - t0) / done if done else first
    return per, max(peak, rss) / 1048576


def _cluster_view(o: Opts) -> tuple[list[tuple[int, float | None]], int, float, int]:
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
        if w.get("lab_pcs"):  # a lab head: plan over the PCs behind it (they are free to run, memory unknown)
            pcs += [(p.get("threads") or 1, None) for p in w["lab_pcs"] if allowed is None or p.get("node_id") in allowed]
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


def _model(o: Opts, rtt: float) -> dict[str, Any]:
    """Overhead constants for the estimate: learned from history.jsonl when there is enough of it, else the defaults."""
    m = _hist.fit(_cfg["head"], o["learn_last"]) if o["history"] else None
    d = {"fixed_s": o["fixed_s"] + 2 * rtt, "item_c": o["item_c"], "speed": 1.0, "runs": 0, "learned": False}
    if m:
        d.update(learned=True, runs=m["runs"], fixed_s=m["fixed_s"])
        for k in ("item_c", "speed"):
            if m[k] is not None:
                d[k] = m[k]
    return d


def _batch_size(per: float, n: int, slots: int, o: Opts) -> int:
    """Items per remote call: about batch_s of work each, but at least ~2 calls per slot so every slot stays busy."""
    if not o["batch"] or n < 2 or per <= 0:
        return 1
    return max(1, min(round(o["batch_s"] / per), math.ceil(n / (2 * max(slots, 1)))))


def _plan(fn: Callable[..., Any], items: list[Any], mode: str, o: Opts, profile: tuple[float, float] | None = None) -> Plan:
    n = len(items)
    per, peak_mb = profile or (_profile(fn, mode, items, min(30.0, o["timeout_s"] or 30.0)) if n else (0.0, 0.0))
    cpus = os.cpu_count() or 1
    procs = min(cpus, n) if per * n > 1.0 else 1
    local_s = n * per / procs + (0.2 if procs > 1 else 0)
    mem_mb = o["mem_mb"] or max(1, math.ceil(peak_mb * 1.5))
    p: dict[str, Any] = dict(n=n, item_s=per, mem_mb=mem_mb, local_s=local_s, local_procs=procs, cluster_s=None, pcs=0, where="local",
             placement=o["placement"], priority=o["priority"], reasons=[], profile=(per, peak_mb), batch=1, learned=False,
             model=None, slots=0, all_slots=0, bytes=0, shared_bytes=0, item_bytes=0, blobs=0, strategy=None, transfer_s=0.0,
             transfer_alt={}, finish_by_pcs={}, transfer_src=None, code_handles=[])
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
    xf = None
    if o["bulk"] and _bulk.supported(_self):  # data movement: the blobs this run needs (code, big data, shards) and what it costs
        try:
            p["code_handles"] = _cmd_for(fn, mode, False)[2]
        except (ClusterError, ValueError, OSError):
            pass
        if p["code_handles"] or any(_bulk.find(x) for x in items):
            sh, pi, p["blobs"] = _bulk.account(items, p["code_handles"], _bulk.holders(_self), o["blob_per_chunk"])
            p.update(shared_bytes=sh, item_bytes=pi, bytes=sh + pi)
            if sh + pi:
                xf = _bulk.Xfer(_self, o, sh, pi, n * per / m["speed"], n, len(conc))

    def cost(k: int) -> float:  # fixed + per item + compute spread over the slots + one python start per remote call + moving the data
        sl = sum(conc[:k])
        return (m["fixed_s"] + n * m["item_c"] + (n * per + math.ceil(n / _batch_size(per, n, sl, o)) * o["start_s"]) / (m["speed"] * sl)
                + (xf.at(k)[0] if xf else 0.0))
    best_k, best_t, prev = 0, float("inf"), float("inf")
    for k in range(1, len(conc) + 1):
        t = cost(k)
        if k > 1 and t > prev * 0.95:
            break  # each extra PC must cut the estimate by >= 5 %
        best_k, best_t, prev = k, t, t
    if best_k:
        by_wall = math.ceil(n * per / (o["target_wall_s"] * m["speed"] * (sum(conc) / len(conc))))
        capped = max(1, min(best_k, by_wall))
        # fewer PCs is only worth it while the capped run still beats local by the 1.5x margin (else the cap alone sends work home)
        if o["local"] == "never" or local_s >= 1.5 * cost(capped):
            best_k = capped
        best_t = cost(best_k)
    if o["pcs"]:
        best_k = min(o["pcs"], len(conc)) or best_k
        best_t = cost(best_k) if best_k else best_t
    p["cluster_s"] = best_t if best_k else None
    if xf and best_k:
        p["transfer_s"], p["strategy"], p["transfer_alt"] = xf.at(best_k)
        p["transfer_src"] = xf.source + (f", assumed {', '.join(xf.assumed)}" if xf.assumed else "")
        p["finish_by_pcs"] = {k: cost(k) for k in sorted({k for k in (1, 2, 4, 8, 16, 32, 64, 128, 256) if k < best_k} | {best_k})}
        p["reasons"].append(f"data {_bulk_mb(p['bytes'])}: {p['strategy']}, transfer about {p['transfer_s']:.1f} s"
                            + (" (" + ", ".join(f"{k} {v:.1f} s" for k, v in p["transfer_alt"].items() if k != p["strategy"]) + " otherwise)" if len(p["transfer_alt"]) > 1 else ""))
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


def _prep(items: list[Any], o: Opts) -> list[Any]:
    """Big arguments become handles (not uploaded yet) when the head has a blob store."""
    if o["bulk"] and o["local"] != "always" and not os.environ.get("CK_JOB") and len(items) > 0 and _bulk.supported(_self):
        return _bulk.prepare(items, min(o["blob_auto_kb"] * 1024, _cfg["max_item_bytes"]))
    return items


def plan(fn: Callable[..., Any], items: Iterable[Any], *, placement: Placement | None = None, priority: Priority | None = None,
         local: LocalMode | None = None, **kw: Any) -> Plan:
    """What the optimizer would do and why, without running the work."""
    o = _opts(_lit(kw, placement, priority, local))
    items = _prep(list(items), o)
    return _plan(getattr(fn, "_ck_fn", fn), items, "call", o)


def advise(fn: Callable[..., Any], items: Iterable[Any], sample: int = 20, *, placement: Placement | None = None,
           priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> Advice:
    """Ranked suggestions to make fn faster on the cluster (or to tell you it cannot help). See docs/SDK.md section 7."""
    from . import _advise
    return _advise.advise(getattr(fn, "_ck_fn", fn), list(items), sample, _opts(_lit(kw, placement, priority, local)))


# ---------------------------------------------------------------- results pushed by the head (SSE), polling as the fallback
_TERMINAL = ("done", "failed", "cancelled")


def _stream_open(head: str, token: str | None) -> Any:
    req = urllib.request.Request(head + "/api/v1/stream", headers={"Accept": "text/event-stream"})
    if token:
        req.add_header("Authorization", "Bearer " + token)
    return urllib.request.urlopen(req, timeout=20)  # the head sends a `cluster` event every second, so 20 s of silence is a dead link


class _Stream:
    """One background connection to /api/v1/stream per session; keeps the newest JobRow of every job it sees."""

    def __init__(self) -> None:
        self.cv, self.live, self.gen = threading.Condition(), False, 0
        self.rows: dict[Any, dict[str, Any]] = {}
        self.head: str | None = None
        self.arrived: dict[Any, float] = {}
        self.wins: list[bool] = []
        self.judge: tuple[Any, float] | None = None
        self.thread: threading.Thread | None = None

    def trusted(self) -> bool:
        """Do pushed events beat a poll? Judged on the last runs: a head that buffers its stream (as today's does, up to
        2 s) loses, so we keep polling fast; a head that flushes each event wins, and polling drops to a 3 s safety net."""
        if self.judge:
            jid, t = self.judge
            self.judge = None
            with self.cv:
                self.wins.append(self.arrived.get(jid, 1e18) - t < 0.15)
            del self.wins[:-5]
        return len(self.wins) >= 3 and sum(self.wins) >= 3

    def ensure(self) -> bool:
        if not _cfg["push"]:
            return False
        if self.head != _cfg["head"] or not getattr(self, "thread", None) or not self.thread.is_alive():
            self.head, self.gen, self.live = _cfg["head"], self.gen + 1, False
            self.thread = threading.Thread(target=self._run, args=(self.head, self.gen), daemon=True)
            self.thread.start()
        return self.live  # never waits: until the stream is connected (the head may send its headers late), polling covers

    def _run(self, head: str, gen: int) -> None:
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
_last: dict[str, Any] = {}  # plan, timings and wall time of the latest cluster run (python -m clusterkit bench reads it)


class _Replan(Exception):
    pass


def _await(jid: int, timeout_s: float | None, check: Callable[[dict[str, Any], float], None] | None = None, st: dict[str, Any] | None = None) -> dict[str, Any]:
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
            if _reconnected[0] and st and st.get("blobs"):  # the head changed under us: put back what the standby had not copied yet
                _reconnected[0] = False
                for h in st["blobs"]:
                    h.on = None
                _bulk.upload(st["blobs"], _self)
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
def _groups(items: Sequence[Any], size: int, limit: int) -> list[str]:
    """Batches of up to `size` items, each encoded as one list item <= limit bytes (split in half when too big)."""
    out: list[str] = []

    def put(chunk: Sequence[Any]) -> None:
        s = _refs(chunk, _runner.enc(chunk), True)
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


def _submit(fn: Callable[..., Any], mode: str, items: list[Any], pl: Plan, o: Opts, st: dict[str, Any]) -> int:
    t0 = time.perf_counter()
    batch = pl.batch > 1
    lines = _groups(items, pl.batch, _cfg["max_item_bytes"]) if batch else _encode(items)
    st["encode_s"] = time.perf_counter() - t0
    cmd, st["ship_s"], code_hs = _cmd_for(fn, mode, batch)
    st["cmd_bytes"] = len(cmd)
    hs = {h.sha: h for x in items for h in _bulk.find(x)}
    hs.update({h.sha: h for h in code_hs})
    st["up_bytes"], st["up_s"] = _bulk.upload(list(hs.values()), _self) if hs else (0, 0.0)
    st["blobs"] = list(hs.values())
    body = {"kind": "list", "cmd": cmd, "items": lines, "name": o["name"] or f"sdk:{fn.__name__}",
            "priority": o["priority"], "max_pcs": pl.pcs, "placement": pl.placement, "mem_mb": pl.mem_mb}
    if hs:
        body["blobs"] = list(hs)
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


def _checker(pl: Plan, o: Opts, items: list[Any], st: dict[str, Any]) -> Callable[[dict[str, Any], float], None]:
    """Called while the job runs: finish locally when the cluster is losing (actual progress vs the plan)."""
    local_ok = o["adaptive"] and o["local"] != "never" and not os.environ.get("CK_JOB")
    n = len(items)

    def check(row: dict[str, Any], elapsed: float) -> None:
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


def _cluster_run(fn: Callable[..., Any], mode: str, items: list[Any], pl: Plan, o: Opts, st: dict[str, Any]) -> list[Any]:
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
    out: list[Any] = []
    big: dict[str, list[int]] = {}
    for l in lines:
        r = _runner.dec(l)
        for tag, v in (r if pl.batch > 1 and isinstance(r, list) else [r]):  # a failed batch line is a lone ("err", tb)
            if tag == "err":
                raise RemoteError(v)
            if tag == "blob":  # a big result the PC left on the head: fetched below, all in parallel
                big.setdefault(v, []).append(len(out))
                v = None
            out.append(v)
    if big:
        t1 = time.perf_counter()
        got = _bulk.fetch_all(list(big), _self)
        for sha, idx in big.items():
            v = _runner.dec(got[sha].decode())
            for i in idx:
                out[i] = v
        st["down_bytes"], st["down_s"] = sum(len(v) for v in got.values()), time.perf_counter() - t1
    st["decode_s"] = time.perf_counter() - t0
    if len(out) != len(items):
        raise ClusterError(f"job {jid} returned {len(out)} results for {len(items)} items")
    st["job"] = jid
    return out


def _learn(st: dict[str, Any], pl: Plan, n: int, actual: float, where: str, o: Opts, fn: Callable[..., Any]) -> None:
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
                   fixed_s=round(max(0.0, actual - st["run_s"] - n * item_c - st.get("up_s", 0.0) - st.get("down_s", 0.0)), 3))
        if pl.bytes:  # predicted vs measured data movement: the uploads to the head (the PCs' side shows up in queue_s)
            rec.update(bytes=pl.bytes, xfer_pred_s=round(pl.transfer_s, 3), strategy=pl.strategy, up_bytes=st.get("up_bytes", 0),
                       up_s=round(st.get("up_s", 0.0), 3), queue_s=round(st["queue_s"], 3))
        if pl.slots and pl.item_s > 0:
            rec["speed"] = round(n * pl.item_s / (max(st["run_s"], 1e-6) * pl.slots), 4)  # per-slot throughput vs this PC
    elif where == "replanned":
        seen = st.get("replan_seen")
        rec.update(failed=True, replan=seen)
        if seen and seen[1] > 0 and pl.slots and pl.item_s > 0:  # what the cluster actually delivered per slot before we gave up
            rec.update(run_s=seen[0], speed=round(seen[1] * n * pl.item_s / (seen[0] * pl.slots), 4))
    _hist.record(rec)


def _run(fn: Callable[..., Any], mode: str, items: list[Any], o: Opts, pl: Plan | None = None, learn: bool = True) -> list[Any]:
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
    items = _prep(items, o)
    pl = pl or _plan(fn, items, mode, o)
    log.info("%s", pl)
    if pl.bytes:
        log.info("data: %s", pl.transfer())
    global _last
    t0, st = time.perf_counter(), {}
    _last = {"plan": pl, "st": st}
    if pl.where == "local":
        out = _local(fn, mode, items, pl.local_procs)
        if learn:
            _learn(st, pl, len(items), time.perf_counter() - t0, "local", o, fn)
        return out
    _in_job[0] += 1
    try:
        out = _cluster_run(fn, mode, items, pl, o, st)
    except _Replan as r:
        log.warning("%s", r)
        _learn(st, pl, len(items), time.perf_counter() - t0, "replanned", o, fn)
        return _local(fn, mode, items, pl.local_procs)
    finally:
        _in_job[0] -= 1
    _last["wall"] = time.perf_counter() - t0
    if learn:
        _learn(st, pl, len(items), _last["wall"], "cluster", o, fn)
    return out


@overload
def map(fn: Callable[[T], R], items: Iterable[T | Handle[T]], reduce: None = None, *, placement: Placement | None = None,
        priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> list[R]: ...
@overload
def map(fn: Callable[[T], R], items: Iterable[T | Handle[T]], reduce: Callable[[list[R]], S], *, placement: Placement | None = None,
        priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> S: ...
def map(fn: Callable[[Any], Any], items: Iterable[Any], reduce: Callable[[list[Any]], Any] | None = None, *, placement: Placement | None = None,
        priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> Any:
    """Same results, same order as [fn(x) for x in items]; runs on the cluster when that is faster."""
    items = list(items)
    out = _run(fn, "call", items, _opts(_lit(kw, placement, priority, local)))
    return reduce(out) if reduce else out


if sys.version_info >= (3, 10):
    class Remote(Generic[P, R]):
        """What @ck.remote returns: calling it is the plain function (same arguments and result types); .map(items) uses the cluster."""

        def __init__(self, fn: Callable[P, R], kw: dict[str, Any]) -> None:
            self._ck_fn, self._kw = fn, kw
            functools.update_wrapper(self, fn)

        def __call__(self, *a: P.args, **k: P.kwargs) -> R:
            return self._ck_fn(*a, **k)

        def map(self, items: Iterable[Any], *, placement: Placement | None = None, priority: Priority | None = None,
                local: LocalMode | None = None, **kw: Any) -> list[R]:
            return map(self._ck_fn, items, **{**self._kw, **_lit(kw, placement, priority, local)})  # type: ignore[no-any-return]
else:
    class Remote(Generic[P, R]):  # type: ignore[no-redef]
        """What @ck.remote returns: calling it is the plain function (result type kept; Python 3.9 cannot keep the argument types)."""

        def __init__(self, fn: Callable[..., R], kw: dict[str, Any]) -> None:
            self._ck_fn, self._kw = fn, kw
            functools.update_wrapper(self, fn)

        def __call__(self, *a: Any, **k: Any) -> R:
            return self._ck_fn(*a, **k)

        def map(self, items: Iterable[Any], *, placement: Placement | None = None, priority: Priority | None = None,
                local: LocalMode | None = None, **kw: Any) -> list[R]:
            return map(self._ck_fn, items, **{**self._kw, **_lit(kw, placement, priority, local)})  # type: ignore[no-any-return]


_Remote = Remote


@overload
def remote(fn: Callable[P, R], **kw: Any) -> Remote[P, R]: ...
@overload
def remote(fn: None = None, **kw: Any) -> Callable[[Callable[P, R]], Remote[P, R]]: ...
def remote(fn: Callable[..., Any] | None = None, **kw: Any) -> Any:
    """@ck.remote: calling fn is unchanged; fn.map(items) uses the cluster."""
    return Remote(fn, kw) if fn else (lambda f: Remote(f, kw))


class SweepRow(TypedDict):
    params: dict[str, Any]
    result: Any


def sweep(fn: Callable[..., R], grid: dict[str, Iterable[Any]], key: Callable[[R], Any] | None = None, top: int | None = None,
          minimize: bool = False, *, placement: Placement | None = None, priority: Priority | None = None,
          local: LocalMode | None = None, **kw: Any) -> list[SweepRow]:
    """Run fn(**config) for every combination in grid. Returns [{'params','result'}], best first when key is given."""
    names = list(grid)
    cfgs = [dict(zip(names, v)) for v in itertools.product(*(grid[k] for k in names))]
    res = _run(fn, "kw", cfgs, _opts(_lit(kw, placement, priority, local)))
    rows: list[SweepRow] = [{"params": c, "result": r} for c, r in zip(cfgs, res)]
    if key:
        rows.sort(key=lambda r: key(r["result"]), reverse=not minimize)
    return rows[:top] if top else rows


def mean_params(ps: Sequence[Any]) -> Any:
    """Average a list of params: dicts (also a torch state_dict), lists, tuples, plain floats, numpy arrays and torch tensors (recursive).
    Neither numpy nor torch is imported: their objects are averaged with the operators they define. Integer arrays keep their dtype."""
    p0 = ps[0]
    if isinstance(p0, dict):
        pairs = [(k, mean_params([p[k] for p in ps])) for k in p0]
        try:
            return type(p0)(pairs)  # OrderedDict stays an OrderedDict (load_state_dict accepts either)
        except TypeError:
            return dict(pairs)
    if isinstance(p0, (list, tuple)) and not hasattr(p0, "_fields"):
        return type(p0)(mean_params(list(c)) for c in zip(*ps))
    if hasattr(p0, "dtype"):  # numpy array / scalar, torch tensor
        tot = p0
        for p in ps[1:]:
            tot = tot + p
        m = tot / len(ps)
        if m.dtype != p0.dtype:  # an int array (BatchNorm's num_batches_tracked) must stay an int array
            m = m.to(p0.dtype) if hasattr(m, "to") else m.astype(p0.dtype)
        return m
    return sum(ps) / len(ps)


_pcpy: dict[str, list[str]] = {}


def pc_python(refresh: bool = False) -> list[str]:
    """Ask the PCs which Python their `python3` is: runs a tiny job, returns the sorted versions (['3.9.6', '3.12.4']; a 't' suffix
    marks a free-threaded build). Then ck.config(pc_python=<the oldest>) checks your code's syntax before shipping and picks the pickle protocol."""
    h = _cfg["head"]
    if refresh or h not in _pcpy:
        pcs = _cluster_view(_opts({}))[0]
        n = min(64, max(2, 2 * sum(th for th, _ in pcs)))
        got = _run(_benchfn.pyver, "call", list(range(n)), _opts(dict(local="never", placement="spread", batch=False, history=False)), learn=False)
        _pcpy[h] = sorted(set(got), key=lambda v: tuple(int(x) for x in v.rstrip("t").split(".")))
    return _pcpy[h]


def train(step: Callable[[M, S], M], init: M, shards: Iterable[S], rounds: int = 10, average: Callable[[list[M]], M] = mean_params, *,
          placement: Placement | None = None, priority: Priority | None = None, local: LocalMode | None = None, **kw: Any) -> M:
    """Model averaging (local SGD): each round every shard runs step(params, shard) on a PC, then results are averaged."""
    shards, o = list(shards), _opts(_lit(kw, placement, priority, local))
    params = init
    pl = _plan(getattr(step, "_ck_fn", step), _prep([(init, s) for s in shards], o), "star", o) if shards else None
    for r in range(rounds):
        outs = _run(step, "star", [(params, s) for s in shards], o, pl, learn=r == 0)
        params = average(outs)
    return params
