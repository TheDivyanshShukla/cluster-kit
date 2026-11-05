# Runs on the PCs as a pushed list-job script: items arrive on stdin (one codec line each), one result line per item
# goes to stdout. A "PAYLOAD = '...'" line is put in front of this file before it is sent. Stdlib only.
import base64, json, pickle, sys, traceback, types, zlib


def enc(obj):
    raw = pickle.dumps(obj, 4)
    z = zlib.compress(raw, 6)
    return ("z" + base64.b64encode(z).decode()) if len(z) < len(raw) else ("p" + base64.b64encode(raw).decode())


class _U(pickle.Unpickler):
    def find_class(self, module, name):  # the shipped main script is loaded as __ck_user__; locally it is __main__
        if module == "__ck_user__" and "__ck_user__" not in sys.modules:
            module = "__main__"
        return super().find_class(module, name)


def dec(line):
    raw = base64.b64decode(line[1:])
    import io
    return _U(io.BytesIO(zlib.decompress(raw) if line[0] == "z" else raw)).load()


def load(payload):
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
    for part in payload["attr"].split("."):
        obj = getattr(obj, part)
    return getattr(obj, "_ck_fn", obj)


def apply(mode, fn, x):
    return fn(**x) if mode == "kw" else fn(*x) if mode == "star" else fn(x)


def _one(mode, fn, x):
    try:
        return ("ok", apply(mode, fn, x))
    except BaseException:  # one bad item must not lose the others
        return ("err", traceback.format_exc())


def run(payload, lines, out):
    fn = load(payload)
    batch = payload.get("batch")  # a line is a list of items; its result line is the list of their results
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            x = dec(line)
            res = [_one(payload["mode"], fn, y) for y in x] if batch else _one(payload["mode"], fn, x)
            s = enc(res)
        except BaseException:
            s = enc(("err", traceback.format_exc()))
        out.write(s + "\n")
    out.flush()


def main():
    payload = json.loads(zlib.decompress(base64.b64decode(PAYLOAD)))  # noqa: F821
    real, sys.stdout = sys.stdout, sys.stderr  # user print() must not corrupt the result stream
    run(payload, sys.stdin, real)


if __name__ == "__main__":
    main()
