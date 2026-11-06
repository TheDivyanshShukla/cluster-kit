"""Bulk distribution (docs/BULK.md): blobs on the head, handles in items, the transfer part of the estimate. Standard library only."""
from __future__ import annotations
import hashlib, io, math, os, pickle, tempfile, time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Generic, TypeVar
from collections.abc import Iterator, Sequence
from . import _runner, _hist

MAGIC = _runner.MAGIC
T = TypeVar("T")
_by_sha: dict[str, Handle[Any]] = {}
_by_id: dict[int, tuple[Any, Handle[Any]]] = {}
_by_file: dict[tuple[Any, ...], str] = {}
_ok: dict[str, bool] = {}


class Handle(Generic[T]):
    """A blob (object or file) on the head. Use it in items or args: fn receives the object, or the local file path."""

    def __init__(self, sha: str, size: int, data: bytes | None = None, path: str | None = None, off: int = 0, ln: int | None = None) -> None:
        self._ck_sha, self.size, self._data, self._path, self._off, self._ln = sha, size, data, path, off, ln
        self.on: str | None = None  # head url this blob is known to be on
        self._tmp: str | None = None
        _runner.LOCAL[sha] = self

    @property
    def sha(self) -> str:
        return self._ck_sha

    def __reduce__(self) -> tuple[Any, ...]:  # on a PC it is a plain marker dict; no clusterkit needed there
        return (dict, ((("__ck_blob__", self._ck_sha),),))

    def __repr__(self) -> str:
        return f"Handle({self._ck_sha[:12]}, {self.size:,} bytes, {'file' if self._data is None or not self._data.startswith(MAGIC) else 'object'})"

    def local(self) -> T:
        if self._data is not None:
            return _runner._U(io.BytesIO(self._data[len(MAGIC):])).load() if self._data.startswith(MAGIC) else self._tmpfile()
        return self._path if self._off == 0 and self._ln is None else self._tmpfile()

    def _tmpfile(self) -> Any:
        if self._tmp is None or not os.path.exists(self._tmp):
            fd, self._tmp = tempfile.mkstemp(prefix="ck-" + self._ck_sha[:8] + "-")
            with os.fdopen(fd, "wb") as f:
                f.write(self._data) if self._data is not None else [f.write(b) for b in _chunks(self._path, self._off, self._ln)]
        return self._tmp

    def _body(self) -> Any:
        return self._data if self._data is not None else _Range(self._path, self._off, self._ln if self._ln is not None else self.size)


class _Range:
    def __init__(self, path: str, off: int, ln: int) -> None:
        self.f, self.left = open(path, "rb"), ln
        self.f.seek(off)

    def read(self, n: int | None = -1) -> bytes:
        if not self.left:
            return b""
        n = self.left if n is None or n < 0 else min(n, self.left)
        b = self.f.read(n)
        self.left -= len(b)
        if not self.left or not b:
            self.f.close()
        return b


def _chunks(path: str, off: int = 0, ln: int | None = None, step: int = 1 << 20) -> Iterator[bytes]:
    with open(path, "rb") as f:
        f.seek(off)
        while ln is None or ln > 0:
            b = f.read(step if ln is None else min(step, ln))
            if not b:
                return
            if ln is not None:
                ln -= len(b)
            yield b


def _remember(h: Handle[Any]) -> Handle[Any]:
    return _by_sha.setdefault(h._ck_sha, h)


def from_bytes(data: bytes) -> Handle[Any]:
    return _remember(Handle(hashlib.sha256(data).hexdigest(), len(data), data=data))


def from_obj(obj: Any) -> Handle[Any]:
    """Object handle; big objects are remembered by identity so train rounds / repeated maps do not re-pickle them."""
    hit = _by_id.get(id(obj))
    if hit and hit[0] is obj:
        return hit[1]
    h = from_bytes(MAGIC + pickle.dumps(obj, _runner.PROTO))
    if len(_by_id) < 64:
        _by_id[id(obj)] = (obj, h)
    return h


def from_file(path: str | os.PathLike[str], off: int = 0, ln: int | None = None) -> Handle[str]:
    path = os.fspath(path)
    st = os.stat(path)
    size = st.st_size - off if ln is None else ln
    key = (os.path.abspath(path), st.st_mtime_ns, st.st_size, off, ln)
    if key not in _by_file:
        s = hashlib.sha256()
        for b in _chunks(path, off, ln):
            s.update(b)
        _by_file[key] = s.hexdigest()
    h = _by_sha.get(_by_file[key]) or Handle(_by_file[key], size, path=os.path.abspath(path), off=off, ln=ln)
    return _remember(h)


def make(x: Any) -> Handle[Any]:
    """Handle for an object, or for a file when x is a path (PathLike, or a str naming an existing file)."""
    if isinstance(x, Handle):
        return x
    if isinstance(x, os.PathLike) or (isinstance(x, str) and os.path.isfile(x)):
        return from_file(x)
    return from_obj(x)


def shard(src: Any, parts: int) -> list[Handle[Any]]:
    """k handles: line-aligned byte ranges of a file (each hashed and uploaded as its own blob), or k slices of a list."""
    parts = max(1, int(parts))
    if isinstance(src, (str, os.PathLike)):
        path, size = os.fspath(src), os.path.getsize(src)
        cuts, f = [0], open(path, "rb")
        for i in range(1, parts):
            f.seek(max(cuts[-1], size * i // parts))
            f.readline()  # shards end on a line boundary
            cuts.append(min(f.tell(), size))
        f.close()
        cuts.append(size)
        return [from_file(path, a, b - a) for a, b in zip(cuts, cuts[1:]) if b > a]
    src = list(src)
    q, r = divmod(len(src), parts)
    out, i = [], 0
    for k in range(parts):
        j = i + q + (k < r)
        if j > i:
            out.append(from_obj(src[i:j]))
        i = j
    return out


def find(x: Any, d: int = 2) -> list[Handle[Any]]:
    """Handles inside an item (two levels, small containers)."""
    if isinstance(x, Handle):
        return [x]
    if d and type(x) in (tuple, list) and len(x) <= 64:
        return [h for y in x for h in find(y, d - 1)]
    if d and type(x) is dict and len(x) <= 64:
        return [h for y in x.values() for h in find(y, d - 1)]
    return []


def _size(x: Any) -> int:
    return len(pickle.dumps(x, _runner.PROTO))


def autoblob(x: Any, thr: int, d: int = 1) -> Any:
    """x with every argument whose pickle is over thr bytes replaced by a handle (small containers are opened one level)."""
    if isinstance(x, Handle) or type(x) in (int, float, bool, type(None)) or _size(x) <= thr:
        return x
    if d and type(x) in (tuple, list) and len(x) <= 64:
        y = [autoblob(v, thr, d - 1) for v in x]
        y = tuple(y) if type(x) is tuple else y
    elif d and type(x) is dict and len(x) <= 64:
        y = {k: autoblob(v, thr, d - 1) for k, v in x.items()}
    else:
        return from_obj(x)
    return y if _size(y) <= thr else from_obj(x)


def prepare(items: Sequence[Any], thr: int) -> list[Any]:
    return [autoblob(x, thr) for x in items]


# ---------------------------------------------------------------- head side
def supported(ck: Any) -> bool:
    h = ck._cfg["head"]
    if h not in _ok:
        try:
            ck._http("GET", "/api/v1/blobs", timeout=10)
            _ok[h] = True
        except ck.ClusterError as e:
            if "cannot reach" in str(e):
                return False  # not cached: the head may come back
            _ok[h] = False
    return _ok[h]


def upload(hs: Sequence[Handle[Any]], ck: Any, workers: int = 4) -> tuple[int, float]:
    """Upload what the head does not have yet (HEAD first). Returns (bytes sent, wall seconds); the speed goes into the history."""
    head = ck._cfg["head"]
    todo = {h.sha: h for h in hs if h.on != head}.values()

    def one(h: Handle[Any]) -> int:
        try:
            ck._http("HEAD", f"/api/v1/blobs/{h.sha}", timeout=30)
            h.on = head
            return 0
        except ck.ClusterError as e:
            if "HTTP 404" not in str(e):
                raise
        r = ck._http("PUT", "/api/v1/blobs", h._body(), length=h.size, timeout=3600)
        if r.get("sha256") != h.sha:
            raise ck.ClusterError(f"the head stored {r.get('sha256')} for a blob with sha256 {h.sha}")
        h.on = head
        return h.size

    t0 = time.perf_counter()
    with ThreadPoolExecutor(min(workers, max(1, len(todo)))) as ex:
        sent = sum(ex.map(one, todo))
    dt = time.perf_counter() - t0
    if sent >= 256 << 10:
        _hist.record({"where": "link", "head": head, "dir": "up", "bytes": sent, "s": round(dt, 4)})
    return sent, dt


def fetch_all(shas: Sequence[str], ck: Any, workers: int = 8) -> dict[str, bytes]:
    """{sha: bytes}, downloaded in parallel; the speed goes into the history."""
    t0 = time.perf_counter()
    with ThreadPoolExecutor(min(workers, max(1, len(shas)))) as ex:
        got = dict(zip(shas, ex.map(lambda s: ck._http("GET", f"/api/v1/blobs/{s}", raw=True, timeout=600), shas)))
    dt, n = time.perf_counter() - t0, sum(map(len, got.values()))
    if n >= 256 << 10:
        _hist.record({"where": "link", "head": ck._cfg["head"], "dir": "down", "bytes": n, "s": round(dt, 4)})
    return got


# ---------------------------------------------------------------- estimates
DEFAULT_BPS = 100e6  # about 1 Gbit/s; used (and said so) until a link speed is measured


def holders(ck: Any) -> dict[str, int]:
    """{sha: number of PCs caching it} from the head, {} when it cannot say."""
    try:
        return {b["sha256"]: (h if isinstance(h, int) else len(h or [])) for b in ck._http("GET", "/api/v1/blobs", timeout=10).get("blobs", [])
                for h in [b.get("holders", 0)]}
    except (ck.ClusterError, AttributeError, TypeError, KeyError):
        return {}


def account(items: Sequence[Any], extra: Sequence[Handle[Any]], held: dict[str, int], per_chunk: bool = False) -> tuple[int, int, int]:
    """(shared bytes, per-item bytes, distinct blobs). A head from before the `@refs:` items made every PC that gets a chunk hold ALL the
    job's blobs, so all bytes are shared (per_chunk=False). A head that fetches only what a chunk's items name (per_chunk=True, the default): blobs used by several
    items (and code/data blobs) go to every PC that runs something, a blob used by one item goes to the PC that gets it.
    Blobs some PC already caches count as moved already."""
    cnt, hs = Counter(), {}
    for x in items:
        for h in {h.sha: h for h in find(x)}.values():
            cnt[h.sha] += 1
            hs[h.sha] = h
    shared = sum(h.size for h in extra if not held.get(h.sha))
    per = 0
    for sha, h in hs.items():
        if held.get(sha):
            continue
        if cnt[sha] > 1 or not per_chunk:
            shared += h.size
        else:
            per += h.size
    return shared, per, len({*hs, *(h.sha for h in extra)})


class Xfer:
    """Transfer seconds for k PCs: the head's estimate API when it has the new form, else the BULK.md formulas on measured speeds."""

    def __init__(self, ck: Any, o: dict[str, Any], shared: float, per_item: float, work_s: float, n: int, kmax: int) -> None:
        self.ck, self.o, self.shared, self.per, self.work_s, self.n, self.kmax = ck, o, shared, per_item, work_s, n, kmax
        self.memo: dict[int, tuple[float, str, dict[str, float]]] = {}
        self.assumed: list[str] = []
        self.source, self.labs, self.fan = "formula", 1, 4
        L = _hist.link(ck._cfg["head"])
        up: float | None = None
        link: float | None = None
        try:
            net = ck._http("GET", "/api/v1/net", timeout=10)
            up = (net.get("head_uplink_mbps") or 0) * 125000 or None
            speeds = sorted(x["net_mbps"] for x in net.get("nodes", []) if x.get("net_mbps"))
            link = speeds[len(speeds) // 2] * 125000 if speeds else None
            self.labs = max(1, len(net.get("labs") or []))
        except (ck.ClusterError, AttributeError, TypeError, KeyError):
            pass
        self.up: float = up or max(L.get("up", 0), L.get("down", 0)) or 0.0
        if not self.up:
            self.up = DEFAULT_BPS
            self.assumed.append("link speed")
        self.link: float = link or self.up

    def formula(self, b: float, k: int) -> dict[str, float]:
        direct = b * k / min(self.up, k * self.link)
        per_lab = max(1, math.ceil(k / self.labs))
        peer = b / self.link * (1 + math.ceil(math.log(per_lab, self.fan))) + b * min(self.labs, k) / self.up if k > 1 else direct
        return {"direct": direct, "peer": peer}

    def at(self, k: int) -> tuple[float, str, dict[str, float]]:
        """(seconds, strategy, {strategy: seconds}) to move the data for k PCs."""
        key = min(1 << max(0, math.ceil(math.log2(max(k, 1)))), max(self.kmax, 1))
        if key not in self.memo:
            self.memo[key] = self._calc(key)
        t, strat, alt = self.memo[key]
        return t + (self.per / min(self.up, k * self.link) if self.per else 0.0), strat, alt

    def _calc(self, k: int) -> tuple[float, str, dict[str, float]]:
        if not self.shared:
            return 0.0, "direct" if self.per else "none", {}
        try:
            r = self.ck._http("GET", f"/api/v1/estimate?bytes={int(self.shared)}&work_s={self.work_s:.3f}&items={self.n}&max_pcs={k}"
                              + (f"&selector={self.ck.urllib.parse.quote(self.o['selector'])}" if self.o["selector"] else ""), timeout=10)
            self.source = "head"
            self.assumed = list(r.get("assumed") or [])
            # `best` may use fewer PCs than asked: take the options at exactly k PCs (the largest count the reply has, up to k)
            alts = [x for x in r.get("alternatives") or [] if x.get("pcs", 0) <= k]
            if alts:
                at = max(x["pcs"] for x in alts)
                alt = {x["strategy"]: float(x["transfer_s"]) for x in alts if x["pcs"] == at}
                best = min(alt, key=lambda z: alt[z])
                return alt[best], best, alt
            best, ts = r["best"]["strategy"], r["transfer_s"]
            return float(ts[best]), best, {s: float(v) for s, v in ts.items()}
        except (self.ck.ClusterError, KeyError, TypeError, AttributeError):
            alt = self.formula(self.shared, k)
            s = min(alt, key=alt.get)
            return alt[s], s, alt
