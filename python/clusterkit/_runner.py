# Runs on the PCs as a pushed list-job script: items arrive on stdin (one codec line each), one result line per item
# goes to stdout. A "PAYLOAD = '...'" line is put in front of this file before it is sent. Stdlib only.
# ruff: noqa: FA102  (no __future__ import: the SDK puts BATCH/PAYLOAD lines in front of this file, and annotations here are strings)
import base64, hashlib, json, os, pickle, sys, traceback, types, zlib
from typing import Any, Callable, Optional  # noqa: F401  (annotations are strings: nothing newer than typing is needed on a PC)

MAGIC = b"CKOBJ1\n"  # first bytes of a blob that holds a pickled object (any other blob is a plain file: you get its path)
LOCAL = {}  # type: "dict[str, Any]"  # sha -> Handle, for runs on the machine that made the handles
# Pickle protocol for everything that crosses between the user's machine and the PCs: the lower of both sides' HIGHEST_PROTOCOL
# (3.8+ is 5, 3.4-3.7 is 4). The SDK lowers it from ck.config(pc_python=...); the PC lowers it from the payload's "proto".
PROTO = min(pickle.HIGHEST_PROTOCOL, 5)  # capped at 5: 3.9 .. 3.15 all read protocol 5


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
    raw = pickle.dumps(obj, PROTO)
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
    global PROTO
    PROTO = min(PROTO, payload.get("proto") or PROTO)
    mod = None
    for name, src in payload["mods"]:
        mod = types.ModuleType(name)
        mod.__file__ = name + ".py"
        sys.modules[name] = mod
        if name == "__ck_user__":
            sys.modules["__main__"] = mod  # pickled instances of the user's own classes load
        try:
            code = compile(src, mod.__file__, "exec")
        except SyntaxError as e:  # the user's code is newer than this PC's python: say which python, not just "invalid syntax"
            v = sys.version_info
            raise SyntaxError(f"this PC runs Python {v[0]}.{v[1]}.{v[2]} and cannot parse {name}.py line {e.lineno}: {e.msg} "
                              f"(newer syntax; upgrade the PC's python3 or avoid the new syntax; ck.config(pc_python='{v[0]}.{v[1]}') checks this before shipping)") from None
        exec(code, mod.__dict__)
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
    if len(pickle.dumps(v, PROTO)) <= rb["kb"] * 1024:
        return ("ok", v)
    try:
        import urllib.request
        # the head gives each job its own upload capability (env CK_BLOB_PUT_URL / CK_BLOB_PUT_TOKEN: PUT /api/v1/blobs only, valid while
        # the job runs); an explicit rb url (ck.config(result_head=...)) wins for the address; rb token is only ever set on an open head
        url, tok = rb.get("url") or os.environ.get("CK_BLOB_PUT_URL"), os.environ.get("CK_BLOB_PUT_TOKEN") or rb.get("token")
        if not url:
            return ("ok", v)
        rq = urllib.request.Request(url.rstrip("/") + ("" if url.rstrip("/").endswith("/api/v1/blobs") else "/api/v1/blobs"), data=enc(v).encode(), method="PUT")
        if tok:
            rq.add_header("Authorization", "Bearer " + tok)
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


def run(payload: dict[str, Any], lines: Any, out: Any, fn: Optional[Callable[..., Any]] = None) -> None:
    fn = fn or load(payload)
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


# ---- warm worker (wave 14, T52): the first task of a job leaves a per-user worker that has already loaded the code (and so its
# imports, torch included); later tasks with the same code + python connect over a unix socket and the worker forks a child per
# task, so tasks stay isolated and parallel. Any failure on the client side falls back to the plain path below.
# Security: the socket lives in a 0700 directory owned by the user, and the worker also refuses any peer with another uid.
WARM_IDLE_S = 120.0  # exit after this long without a task (env CK_WARM_IDLE_S); CK_WARM=off disables; ponytail: make tunable (warm_idle_s)


def _warm_dir() -> Optional[str]:
    import stat, tempfile
    if os.name == "nt" or not hasattr(os, "fork") or os.environ.get("CK_WARM") == "off":
        return None
    d = os.path.join(tempfile.gettempdir(), "ck-warm-" + str(os.getuid()))
    try:
        os.makedirs(d, 0o700, exist_ok=True)
        st = os.lstat(d)
        return d if stat.S_ISDIR(st.st_mode) and st.st_uid == os.getuid() and not st.st_mode & 0o077 else None
    except OSError:
        return None


def _peer_uid(c: Any) -> Optional[int]:
    import socket, struct
    try:
        if hasattr(socket, "SO_PEERCRED"):  # Linux: struct ucred {pid, uid, gid}
            return struct.unpack("3i", c.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))[1]
        return struct.unpack("II", c.getsockopt(0, 1, 76)[:8])[1]  # macOS/BSD: LOCAL_PEERCRED -> struct xucred {version, uid, ...}
    except Exception:
        return None


def _warm_worker(sock_path: str) -> None:
    import signal, socket
    me = os.getuid()
    lock = sock_path[:-2] + ".lock"
    try:
        payload = json.loads(zlib.decompress(base64.b64decode(_code())))
        fn = load(payload)  # the slow part (imports) happens once, here
        signal.signal(signal.SIGCHLD, signal.SIG_IGN)
        srv = socket.socket(socket.AF_UNIX)
        old = os.umask(0o177)
        try:
            srv.bind(sock_path)
        finally:
            os.umask(old)
        srv.listen(16)
        idle = float(os.environ.get("CK_WARM_IDLE_S") or WARM_IDLE_S)
        srv.settimeout(idle)
        sys.stdout = sys.stderr
        try:
            os.unlink(lock)
        except OSError:
            pass
        while True:
            try:
                c, _ = srv.accept()
            except socket.timeout:
                break
            if _peer_uid(c) not in (None, me):  # None: no peer credentials here, the 0700 directory is the guard
                c.close()
                continue
            if os.fork() == 0:
                try:
                    srv.close()
                    c.settimeout(30)
                    hdr, fds, _f, _a = socket.recv_fds(c, 1 << 20, 1)
                    h = json.loads(hdr)
                    os.environ.update(h["env"])
                    os.chdir(h["cwd"])
                    if fds:
                        os.dup2(fds[0], 2)
                    c.sendall(b"K")
                    c.settimeout(None)
                    rf, wf = c.makefile("r", encoding="utf-8"), c.makefile("w", encoding="utf-8")
                    run(payload, rf, wf, fn)
                    wf.flush()
                    os._exit(0)
                except BaseException:
                    os._exit(1)
            c.close()
    finally:
        for p in (sock_path, lock, sock_path[:-2] + ".py"):
            try:
                os.unlink(p)
            except OSError:
                pass


def _code() -> str:
    g = globals().get("PAYLOAD")
    if not g:  # code shipped as a blob: its sha is PAYLOAD_BLOB, the file is in the cache directory
        with open(os.path.join(os.environ["CK_BLOBS"], PAYLOAD_BLOB)) as f:  # noqa: F821
            g = f.read()
    return g


def _warm_try(g: str, data: str, real: Any) -> int:
    """Run the items on the warm worker. Returns how many result lines it delivered (0 = nothing, caller runs everything cold)."""
    import socket, subprocess, shutil
    d = _warm_dir()
    if d is None:
        return 0
    key = hashlib.sha256((g + repr(globals().get("BATCH")) + "\0" + sys.executable + "\0" + sys.version).encode()).hexdigest()[:16]
    sp = os.path.join(d, key + ".s")
    got = 0
    try:
        c = socket.socket(socket.AF_UNIX)
        try:
            c.settimeout(5)
            c.connect(sp)
            socket.send_fds(c, [json.dumps({"env": dict(os.environ), "cwd": os.getcwd()}).encode()], [2])
            if c.recv(1) != b"K":
                raise OSError("worker refused")
            c.settimeout(None)
            c.sendall(data.encode("utf-8"))
            c.shutdown(socket.SHUT_WR)
            for line in c.makefile("r", encoding="utf-8"):
                if line.endswith("\n"):  # a torn last line (worker died mid-write) is dropped and redone cold
                    real.write(line)
                    got += 1
            real.flush()
        finally:
            c.close()
        return got
    except Exception:
        if got:
            return got
    # no worker yet (or it is stale): start one for the next task, at most one per key at a time, and run this one cold
    try:
        lock = os.path.join(d, key + ".lock")
        try:
            if os.path.exists(lock) and __import__("time").time() - os.path.getmtime(lock) > 600:
                os.unlink(lock)
            os.close(os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
        except OSError:
            return 0
        if os.path.exists(sp):
            os.unlink(sp)  # stale socket of a dead worker
        script = os.path.join(d, key + ".py")
        shutil.copyfile(__file__, script)  # the shipped file already starts with its BATCH / PAYLOAD lines
        subprocess.Popen([sys.executable, script, "--ck-warm", sp], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    except Exception:
        pass
    return 0


def main() -> None:
    if sys.argv[1:2] == ["--ck-warm"]:
        return _warm_worker(sys.argv[2])
    g = _code()
    real, sys.stdout = sys.stdout, sys.stderr  # user print() must not corrupt the result stream
    lines = sys.stdin.readlines()
    done = 0
    if "__file__" in globals():
        done = _warm_try(g, "".join(lines), real)
    todo = [x for x in lines if x.strip()]
    if done < len(todo):  # the warm worker delivered the first `done` results; everything else runs the plain way
        run(json.loads(zlib.decompress(base64.b64decode(g))), todo[done:], real)


if __name__ == "__main__":
    main()
