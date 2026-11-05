# Runs on the PCs as a pushed list-job script: items arrive on stdin (one codec line each), one result line per item
# goes to stdout. A "PAYLOAD = '...'" line is put in front of this file before it is sent. Stdlib only.
# ruff: noqa: FA102  (no __future__ import: the SDK puts BATCH/PAYLOAD lines in front of this file, and annotations here are strings)
import base64, json, os, pickle, sys, traceback, types, zlib
from typing import Any, Callable, Optional  # noqa: F401  (annotations are strings: nothing newer than typing is needed on a PC)

MAGIC = b"CKOBJ1\n"  # first bytes of a blob that holds a pickled object (any other blob is a plain file: you get its path)
LOCAL = {}  # type: "dict[str, Any]"  # sha -> Handle, for runs on the machine that made the handles


def _read(path: str) -> Any:
    with open(path, "rb") as f:
        if f.read(len(MAGIC)) != MAGIC:
            return path
        return _U(f).load()


def blob(sha: str) -> Any:
    h = LOCAL.get(sha)
    return h.local() if h is not None else _read(os.path.join(os.environ["CK_BLOBS"], sha))


def resolve(x: Any, d: int = 2) -> Any:
    """Handles (client side) or their markers (on a PC) -> the object or the local file path. Looks two levels deep, small containers only."""
    s = getattr(x, "_ck_sha", None)
    if s is None and type(x) is dict and len(x) == 1 and "__ck_blob__" in x:
        s = x["__ck_blob__"]
    if s is not None:
        return blob(s)
    if d and type(x) in (tuple, list) and len(x) <= 64:
        r = [resolve(y, d - 1) for y in x]
        return tuple(r) if type(x) is tuple else r
    if d and type(x) is dict and len(x) <= 64:
        return {k: resolve(v, d - 1) for k, v in x.items()}
    return x


def enc(obj: Any) -> str:
    raw = pickle.dumps(obj, 4)
    z = zlib.compress(raw, 6)
    return ("z" + base64.b64encode(z).decode()) if len(z) < len(raw) else ("p" + base64.b64encode(raw).decode())


class _U(pickle.Unpickler):
    def find_class(self, module: str, name: str) -> Any:  # the shipped main script is loaded as __ck_user__; locally it is __main__
        if module == "__ck_user__" and "__ck_user__" not in sys.modules:
            module = "__main__"
        return super().find_class(module, name)


def dec(line: str) -> Any:
    raw = base64.b64decode(line[1:])
    import io
    return _U(io.BytesIO(zlib.decompress(raw) if line[0] == "z" else raw)).load()


def load(payload: dict[str, Any]) -> Callable[..., Any]:
    try:
        import clusterkit  # noqa: F401
    except ImportError:  # PCs need no install: a stub makes `import clusterkit as ck` + @ck.remote harmless
        m = types.ModuleType("clusterkit")
        m.remote = lambda f=None, **k: f if f is not None else (lambda g: g)
        m.connect = m.config = lambda *a, **k: None
        sys.modules["clusterkit"] = m
    mod = None
    for name, src in payload["mods"]:
        mod = types.ModuleType(name)
        mod.__file__ = name + ".py"
        sys.modules[name] = mod
        if name == "__ck_user__":
            sys.modules["__main__"] = mod  # pickled instances of the user's own classes load
        exec(compile(src, mod.__file__, "exec"), mod.__dict__)
    obj = sys.modules[payload["main"]]
    for n, sha in (payload.get("gl") or {}).items():  # big module-level data arrives as a blob, not from the source
        obj.__dict__[n] = blob(sha)
    for part in payload["attr"].split("."):
        obj = getattr(obj, part)
    return getattr(obj, "_ck_fn", obj)


def apply(mode: str, fn: Callable[..., Any], x: Any) -> Any:
    x = resolve(x)
    return fn(**x) if mode == "kw" else fn(*x) if mode == "star" else fn(x)


def _put(v: Any, rb: dict[str, Any]) -> tuple[str, Any]:
    """A result over rb['kb'] goes to the head as a blob; the line carries only its sha. Any failure: send it inline."""
    if len(pickle.dumps(v, 4)) <= rb["kb"] * 1024:
        return ("ok", v)
    try:
        import urllib.request
        rq = urllib.request.Request(rb["url"] + "/api/v1/blobs", data=enc(v).encode(), method="PUT")
        if rb.get("token"):
            rq.add_header("Authorization", "Bearer " + rb["token"])
        with urllib.request.urlopen(rq, timeout=300) as r:
            return ("blob", json.loads(r.read())["sha256"])
    except Exception:
        return ("ok", v)


def _one(mode: str, fn: Callable[..., Any], x: Any, rb: Optional[dict[str, Any]] = None) -> tuple[str, Any]:
    try:
        v = apply(mode, fn, x)
        return _put(v, rb) if rb else ("ok", v)
    except BaseException:  # one bad item must not lose the others
        return ("err", traceback.format_exc())


def run(payload: dict[str, Any], lines: Any, out: Any) -> None:
    fn = load(payload)
    rb = payload.get("rb")  # {url, token, kb}: where to send big results
    batch = payload.get("batch") or globals().get("BATCH")  # a line is a list of items; its result line is the list of their results
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            if os.path.isabs(line.split("\t")[0]):  # the head turned `@blob:<sha>[:off:len]` into a local path
                p = line.split("\t")
                x = _read(p[0]) if len(p) == 1 else (p[0], int(p[1]), int(p[2]))
            else:
                x = dec(line)
            res = [_one(payload["mode"], fn, y, rb) for y in x] if batch else _one(payload["mode"], fn, x, rb)
            s = enc(res)
        except BaseException:
            s = enc(("err", traceback.format_exc()))
        out.write(s + "\n")
    out.flush()


def main() -> None:
    g = globals().get("PAYLOAD")
    if not g:  # code shipped as a blob: its sha is PAYLOAD_BLOB, the file is in the cache directory
        with open(os.path.join(os.environ["CK_BLOBS"], PAYLOAD_BLOB)) as f:  # noqa: F821
            g = f.read()
    payload = json.loads(zlib.decompress(base64.b64decode(g)))
    real, sys.stdout = sys.stdout, sys.stderr  # user print() must not corrupt the result stream
    run(payload, sys.stdin, real)


if __name__ == "__main__":
    main()
