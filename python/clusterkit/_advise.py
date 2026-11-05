"""ck.advise(fn, items): run a small sample, then say what would make the work faster (or safer) on the cluster.
Standard library only. Every rule is a small function that reads the facts gathered once in `_facts`."""
from __future__ import annotations
import ast, cProfile, inspect, math, os, pickle, pstats, statistics, sys, time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from types import ModuleType
from typing import Any, Callable

# ponytail: used on Python 3.9 where sys.stdlib_module_names does not exist; unknown names count as third-party
_STD = getattr(sys, "stdlib_module_names", None) or frozenset(
    "abc argparse array ast asyncio base64 bisect builtins bz2 calendar collections concurrent contextlib copy csv ctypes dataclasses "
    "datetime decimal difflib email enum errno fnmatch fractions functools gc getpass glob gzip hashlib heapq hmac html http importlib "
    "inspect io itertools json logging lzma math mmap multiprocessing operator os pathlib pickle platform pprint queue random re "
    "resource secrets select shelve shutil signal socket sqlite3 ssl stat statistics string struct subprocess sys tempfile textwrap "
    "threading time timeit traceback types typing unicodedata unittest urllib uuid warnings weakref xml zipfile zlib".split())
_SEV = {"blocker": 0, "warn": 1, "gain": 2, "info": 3}


@dataclass(repr=False)
class Suggestion:
    """One advisor finding: id, one-line title, why (evidence), what to try, severity (blocker|warn|gain|info), gain text and seconds."""
    id: str
    title: str
    why: str
    hint: str
    severity: str = "gain"
    gain: str | None = None
    gain_s: float | None = None
    evidence: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.evidence = self.evidence or {}

    def __str__(self) -> str:
        out = [f"[{self.id}] {self.title}", f"    why:  {self.why}"]
        if self.gain:
            out.append(f"    gain: {self.gain}")
        out.append("    try:  " + self.hint.replace("\n", "\n          "))
        return "\n".join(out)

    __repr__ = __str__


class Advice(list[Suggestion]):
    """The ranked suggestions (a list of Suggestion); .facts has what the sample measured."""
    facts: dict[str, Any]

    def ids(self) -> list[str]:
        return [s.id for s in self]

    def __str__(self) -> str:
        if not self:
            return "no suggestions: nothing in the sample stands out"
        return "\n\n".join(f"{i}. {s}" for i, s in enumerate(self, 1))


def _t(s: float) -> str:
    return f"{s * 1000:.1f} ms" if s < 1 else f"{s:.1f} s" if s < 120 else f"{s / 60:.1f} min"


def _mb(b: float) -> str:
    return f"{b / 1048576:.1f} MB" if b >= 1 << 20 else f"{b / 1024:.1f} KB"


# ---------------------------------------------------------------- facts
def _same(a: Any, b: Any) -> bool:
    try:
        return a == b or pickle.dumps(a, 4) == pickle.dumps(b, 4)
    except Exception:
        return False


def _fingerprint(mod: ModuleType) -> dict[str, Any]:
    """What the module's mutable globals look like right now (to see whether fn changed them)."""
    out = {}
    for k, v in vars(mod).items():
        if not k.startswith("__") and isinstance(v, (list, dict, set, bytearray)):
            try:
                out[k] = repr(v)[:20000]
            except Exception:
                out[k] = "?"
        elif not k.startswith("__") and isinstance(v, (int, float, str, bool)):
            out[k] = repr(v)
    return out


def _called_names(node: ast.AST) -> set[str]:
    return {n.func.id for n in ast.walk(node) if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)}


def _static(fn: Callable[..., Any], mod: ModuleType) -> dict[str, Any]:
    """AST facts about fn and the same-module functions it calls (3 levels): side effects, randomness, imports."""
    f = {"globals_written": set(), "writes": [], "random": [], "imports": set(), "calls": set()}
    try:
        with open(mod.__file__, encoding="utf-8") as fh:
            tree = ast.parse(fh.read())
    except (OSError, SyntaxError, TypeError, AttributeError):
        return f
    defs = {n.name: n for n in tree.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}
    optional = {id(c) for t in ast.walk(tree) if isinstance(t, ast.Try) and any(
        h.type is not None and "ImportError" in ast.dump(h.type) or "NotFoundError" in ast.dump(h.type or ast.Pass()) for h in t.handlers)
        for b in t.body for c in ast.walk(b)}  # try: import x / except ImportError: the module copes without it
    for n in ast.walk(tree):  # imports anywhere in the file: the PCs run the whole module
        if id(n) in optional:
            continue
        if isinstance(n, ast.Import):
            f["imports"] |= {a.name.split(".")[0] for a in n.names}
        elif isinstance(n, ast.ImportFrom) and n.level == 0 and n.module:
            f["imports"].add(n.module.split(".")[0])
    top = {t.id for n in tree.body if isinstance(n, (ast.Assign, ast.AnnAssign)) for t in (n.targets if isinstance(n, ast.Assign) else [n.target])
           if isinstance(t, ast.Name)}
    seen, todo = set(), [fn.__name__ if "." not in fn.__qualname__ else None]
    while todo:
        name = todo.pop()
        if not name or name in seen or name not in defs:
            continue
        seen.add(name)
        node = defs[name]
        local = {a.arg for a in node.args.args + node.args.kwonlyargs} | {t.id for n in ast.walk(node) if isinstance(n, ast.Assign) for t in n.targets if isinstance(t, ast.Name)}
        for n in ast.walk(node):
            if isinstance(n, ast.Global):
                f["globals_written"] |= set(n.names)
            if isinstance(n, (ast.Assign, ast.AugAssign)):
                for t in (n.targets if isinstance(n, ast.Assign) else [n.target]):
                    base = t
                    while isinstance(base, (ast.Subscript, ast.Attribute)):
                        base = base.value
                    if isinstance(t, (ast.Subscript, ast.Attribute)) and isinstance(base, ast.Name) and base.id in top and base.id not in local:
                        f["globals_written"].add(base.id)
            if isinstance(n, ast.Call):
                fn_ = n.func
                if isinstance(fn_, ast.Name) and fn_.id == "open":
                    mode = n.args[1].value if len(n.args) > 1 and isinstance(n.args[1], ast.Constant) else next(
                        (k.value.value for k in n.keywords if k.arg == "mode" and isinstance(k.value, ast.Constant)), "r")
                    if isinstance(mode, str) and set(mode) & set("wax+"):
                        f["writes"].append(f"open(..., {mode!r})")
                elif isinstance(fn_, ast.Attribute):
                    root = fn_.value.id if isinstance(fn_.value, ast.Name) else None
                    if root in ("os", "shutil", "subprocess", "requests") and fn_.attr in (
                            "remove", "unlink", "rename", "replace", "mkdir", "makedirs", "rmdir", "rmtree", "copy", "copyfile", "move", "system",
                            "run", "Popen", "call", "check_call", "check_output", "post", "put", "delete", "patch", "kill"):
                        f["writes"].append(f"{root}.{fn_.attr}()")
                    elif fn_.attr in ("append", "extend", "update", "add", "pop", "setdefault", "clear", "insert", "remove") and root in top and root not in local:
                        f["globals_written"].add(root)
                    if root == "random" and fn_.attr not in ("Random", "seed", "SystemRandom"):
                        f["random"].append(f"random.{fn_.attr}()")
                    elif (root, fn_.attr) in (("time", "time"), ("time", "monotonic"), ("os", "urandom"), ("uuid", "uuid4"), ("uuid", "uuid1"),
                                              ("secrets", "token_hex"), ("secrets", "token_bytes")) or fn_.attr in ("now", "utcnow", "today"):
                        f["random"].append(ast.unparse(fn_) + "()" if hasattr(ast, "unparse") else fn_.attr + "()")
                    elif isinstance(fn_.value, ast.Attribute) and isinstance(fn_.value.value, ast.Name) and fn_.value.value.id == "np" and fn_.value.attr == "random" \
                            and fn_.attr not in ("seed", "default_rng", "RandomState"):
                        f["random"].append(f"np.random.{fn_.attr}()")
        todo += [c for c in _called_names(node) if c in defs]
        f["calls"] |= seen
    return f


def _facts(fn: Callable[..., Any], items: list[Any], sample: int, ck: Any, o: dict[str, Any]) -> dict[str, Any]:
    n, k = len(items), max(1, min(sample, len(items)))
    idx = sorted({int(i * n / k) for i in range(k)})
    xs = [items[i] for i in idx]
    mod = sys.modules.get(fn.__module__)
    F = {"fn": fn, "items": items, "n": n, "xs": xs, "mod": mod, "o": o, "ck": ck, "name": getattr(fn, "__qualname__", repr(fn)), "error": None}
    try:
        ck._describe(fn)
    except ValueError as e:
        F["unpicklable_fn"] = str(e)
    for what, vals in (("item", xs),):
        for i, x in enumerate(vals):
            try:
                pickle.dumps(x, 4)
            except Exception as e:
                F["unpicklable_" + what] = f"item {idx[i]} ({type(x).__name__}): {e}"
                break
    before = _fingerprint(mod) if mod else {}
    wall, cpu, res = [], [], []
    t_all = time.perf_counter()
    for x in xs:
        t0, c0 = time.perf_counter(), time.process_time()
        try:
            res.append(ck._runner.apply("call", fn, x))
        except Exception as e:
            F["error"] = f"{type(e).__name__}: {e}"
            break
        wall.append(time.perf_counter() - t0)
        cpu.append(time.process_time() - c0)
        if time.perf_counter() - t_all > 5 and len(wall) >= 3:
            break
    F["ran"], F["res"] = len(wall), res
    F["per"] = statistics.median(wall) if wall else 0.0
    F["cpu_ratio"] = sum(cpu) / sum(wall) if wall and sum(wall) > 0 else 1.0
    after = _fingerprint(mod) if mod else {}
    F["globals_changed"] = sorted(k_ for k_ in after if before.get(k_) != after[k_] and k_ in before)
    F["res_bytes"] = [len(ck._runner.enc(r)) for r in res if _picklable(r)]
    F["bad_res"] = next((type(r).__name__ for r in res if not _picklable(r)), None)
    F["item_bytes"] = [len(ck._runner.enc(x)) for x in xs if _picklable(x)]
    if wall:
        again = []
        t0 = time.perf_counter()
        for x in xs[:min(5, len(res))]:
            if time.perf_counter() - t0 > 3:
                break
            try:
                again.append(ck._runner.apply("call", fn, x))
            except Exception:
                break
        F["differs"] = [i for i, r in enumerate(again) if not _same(r, res[i])]
        F["rerun"] = len(again)
        if F["per"] * len(xs) < 1.5 and len(xs) >= 4:  # does a thread pool help? (no: GIL-bound; yes: waiting on I/O or C code that lets go)
            t0 = time.perf_counter()
            try:
                with ThreadPoolExecutor(4) as ex:
                    list(ex.map(fn, xs))
                F["thread_speedup"] = sum(wall) / (time.perf_counter() - t0)
            except Exception:
                pass
        F["prof"] = _hot(fn, xs[:5], ck)
        F["peak_mb"] = ck._profile(fn, "call", xs)[1]
    F["static"] = _static(fn, mod) if mod else {}
    return F


def _picklable(x: Any) -> bool:
    try:
        pickle.dumps(x, 4)
        return True
    except Exception:
        return False


def _hot(fn: Callable[..., Any], xs: list[Any], ck: Any) -> dict[str, Any]:
    """cProfile of a few calls: share of the time spent in the user's own Python code, and the hottest function."""
    pr, t0 = cProfile.Profile(), time.perf_counter()
    pr.enable()
    try:
        for x in xs:
            ck._runner.apply("call", fn, x)
            if time.perf_counter() - t0 > 3:
                break
    finally:
        pr.disable()
    rows = pstats.Stats(pr).stats
    total = sum(v[2] for v in rows.values()) or 1e-12
    try:
        mine = os.path.realpath(inspect.getsourcefile(fn) or "")
    except TypeError:
        mine = ""
    py = {k: v[2] for k, v in rows.items() if os.path.realpath(k[0]) == mine}
    builtin = sum(v[2] for k, v in rows.items() if k[0] == "~")
    top = max(py, key=py.get) if py else None
    return {"py_share": sum(py.values()) / total, "builtin_share": builtin / total, "top": top[2] if top else None,
            "top_share": py[top] / total if top else 0.0}


# ---------------------------------------------------------------- rules (each returns a Suggestion or None)
def _unpicklable(F: dict[str, Any]) -> Suggestion | None:
    if F.get("unpicklable_fn") or F.get("unpicklable_item") or F.get("bad_res"):
        why = F.get("unpicklable_fn") or ("an argument cannot be pickled: " + F["unpicklable_item"] if F.get("unpicklable_item")
                                           else f"a result of type {F['bad_res']} cannot be pickled")
        return Suggestion("unpicklable", "Move the function to module level / make arguments picklable", why,
                          "def work(x): ...   # top level of your script, not a lambda or nested function\n"
                          "pass plain data (numbers, strings, lists, dicts, dataclasses), not open files, locks or generators",
                          "blocker", "the cluster cannot run this at all until fixed (it runs locally now)", None, {"error": why})


def _failing(F: dict[str, Any]) -> Suggestion | None:
    if F["error"]:
        return Suggestion("raises", "fn raised on a sample item", F["error"], "run fn(items[0]) by hand and fix the error first", "blocker",
                          evidence={"error": F["error"]})


def _tiny(F: dict[str, Any]) -> Suggestion | None:
    ck, o = F["ck"], F["o"]
    if F["ran"] and F["per"] < 0.005 and F["n"] >= 50:
        slots = max(F.get("slots") or 2, 2)
        B = ck._batch_size(F["per"], F["n"], slots, o)
        if B > 1:
            lines = math.ceil(F["n"] / B)
            gain_s = max(0.0, F["n"] * o["start_s"] / slots * (1 - 1 / B))
            return Suggestion("tiny_items", f"Items are tiny ({_t(F['per'])} each): batch them", f"each remote call has a fixed cost of about "
                              f"{_t(o['start_s'])} (python start); {F['n']:,} one-item calls would spend more time starting than working. "
                              f"ck.map groups them automatically: {B} items per call, {lines} calls",
                              "ck.map(fn, all_items)   # one call with all items; never ck.map per few items in a loop\n"
                              "ck.config(batch_s=0.5)  # seconds of work per remote call (0.2-1 is the sweet spot)",
                              "gain", f"{_t(gain_s)} of start-up cost avoided (already applied automatically)", gain_s,
                              {"per_item_s": F["per"], "n": F["n"], "batch": B, "calls": lines})


def _big(F: dict[str, Any]) -> list[Suggestion]:
    _ck, lim = F["ck"], F["ck"]._cfg["max_item_bytes"]
    out = []
    if F["item_bytes"]:
        b = statistics.mean(F["item_bytes"])
        if b > lim / 2:
            out.append(Suggestion("big_args", f"Arguments are big ({_mb(b)} each encoded; the limit is {_mb(lim)})",
                                  f"{F['n']:,} items x {_mb(b)} = {_mb(b * F['n'])} must travel to the head and on to the PCs",
                                  "ck.put(data) or ck.shard(path, parts=k) sends it once as a blob (cached on the PCs), or send a path or a seed and load/generate the data inside fn:\n"
                                  "ck.map(load_and_process, paths)   # paths every PC can read, or shard ids / random seeds\n"
                                  f"or raise the limit: ck admin tune max_item_bytes {int(b * 2)} and ck.config(max_item_bytes=...)", "warn",
                                  f"{_t(b * F['n'] / 20e6)} of transfer at 20 MB/s avoided", b * F["n"] / 20e6, {"item_bytes": b, "limit": lim}))
    if F["res_bytes"]:
        b = statistics.mean(F["res_bytes"])
        if b > 64 * 1024:
            out.append(Suggestion("big_results", f"Results are big ({_mb(b)} each)", f"{F['n']:,} results = {_mb(b * F['n'])} come back through the head "
                                  "as text (base64), and the head keeps them in its store",
                                  "return a summary (a score, a path to a file you wrote on shared storage) instead of the data;\n"
                                  "use reduce= so only the combined value matters: ck.map(fn, items, reduce=sum)", "warn",
                                  f"{_t(b * F['n'] / 20e6)} of transfer at 20 MB/s avoided", b * F["n"] / 20e6, {"result_bytes": b}))
    return out


def _hot_loop(F: dict[str, Any]) -> Suggestion | None:
    p = F.get("prof")
    if p and F["per"] >= 0.0002 and p["py_share"] >= 0.5:
        total = F["n"] * F["per"]
        gain_s = total * p["py_share"] * 0.9
        return Suggestion("hot_loop", f"Pure-Python hot loop: {p['py_share']:.0%} of the time is in your own Python code",
                          f"cProfile of {len(F['xs'][:5])} calls: {p['top_share']:.0%} in {p['top']}() itself, {p['builtin_share']:.0%} in builtins. "
                          "A Python loop runs ~50x slower than the same work vectorised",
                          "import numpy as np   # numpy only has to exist on the PCs that run it\n"
                          "x = np.arange(N); s = (np.sin(a * x) * np.cos(b * x)).sum()   # instead of: for i in range(N): s += ...\n"
                          "or math/itertools/sum() over a generator, or move the loop to a C-backed function",
                          "gain", f"if vectorised ~10x: saves {_t(gain_s)} of {_t(total)} of work (estimate; measure it)", gain_s,
                          {"py_share": p["py_share"], "builtin_share": p["builtin_share"], "hot_function": p["top"]})


def gil_enabled() -> bool:
    """False only on a free-threaded build (3.13t / 3.14t) running with the GIL off; every other Python has one."""
    return getattr(sys, "_is_gil_enabled", lambda: True)()


def _threads(F: dict[str, Any]) -> Suggestion | None:
    r, sp = F["cpu_ratio"], F.get("thread_speedup")
    if not F["ran"] or F["per"] < 0.001:
        return None
    if not gil_enabled() and r >= 0.5 and sp and sp >= 1.8:  # free-threaded Python: Python threads run in parallel
        total = F["n"] * F["per"]
        return Suggestion("threads_ok", f"CPU-bound, and threads scale here (4 threads give {sp:.1f}x on this free-threaded Python)",
                          "this Python has no GIL, so threads run Python code in parallel and share memory (no pickling, one copy of big data); "
                          "this only holds on the PCs if their python3 is also a free-threaded build (python3 -c 'import sys; print(sys._is_gil_enabled())')",
                          "from concurrent.futures import ThreadPoolExecutor   # inside fn, over its own sub-items\n"
                          "ck.map(fn, items, mem_mb=...)   # or leave it: the cluster runs one process per slot, which also scales",
                          "info", f"up to {sp:.1f}x per slot on threads (estimate)", total * (1 - 1 / sp), {"cpu_ratio": r, "thread_speedup": sp, "gil": False})
    if r < 0.5 or (sp and sp >= 2.5):
        total = F["n"] * F["per"]
        return Suggestion("io_bound", f"Mostly waiting, not computing (CPU {r:.0%} of wall time" + (f", 4 threads give {sp:.1f}x" if sp else "") + ")",
                          "the function sleeps or waits on disk/network; a PC can overlap many such calls, so slots should not be limited to its cores",
                          "from concurrent.futures import ThreadPoolExecutor   # inside fn, over its own sub-items\n"
                          "ck.map(fn, items, mem_mb=50)   # small mem_mb lets each PC run more copies at once",
                          "gain", f"up to 4x per slot on threads: saves about {_t(total * 0.75)} of {_t(total)} (estimate)", total * 0.75,
                          {"cpu_ratio": r, "thread_speedup": sp})
    if sp is not None and sp < 1.3 and r > 0.8 and gil_enabled():  # with no GIL a flat speed-up is not the GIL's doing
        return Suggestion("gil_bound", f"CPU-bound and GIL-bound (4 threads give only {sp:.1f}x)",
                          "threads share one interpreter lock, so they cannot speed this up; separate processes can, and that is what the cluster uses",
                          "ck.map(fn, items)               # one process per slot on each PC (no threads)\n"
                          "ck.map(fn, items, local='always')   # or all cores of this PC, if the cluster does not pay off",
                          "info", "use processes, not threads (the SDK already does)", None, {"cpu_ratio": r, "thread_speedup": sp})


def _memory(F: dict[str, Any]) -> Suggestion | None:
    mb = F.get("peak_mb") or 0
    if mb >= 100:
        want = math.ceil(mb * 1.5)
        return Suggestion("high_memory", f"High memory per item (about {mb:.0f} MB)",
                          f"each running copy needs ~{mb:.0f} MB; an agent runs floor(free RAM x 0.8 / mem_mb) copies at once, so a PC with 8 GB free runs "
                          f"{int(8192 * 0.8 // want)} of them instead of one per thread",
                          f"ck.map(fn, items, mem_mb={want})   # the SDK already sets {want} (measured x 1.5); set it yourself if the sample under-measures\n"
                          "or process the data in a streaming way (generators, chunks) so peak memory stays small",
                          "warn", "avoids out-of-memory kills on small PCs", None, {"peak_mb": mb, "mem_mb": want})


def _side_effects(F: dict[str, Any]) -> Suggestion | None:
    s = F["static"] or {}
    g = sorted(set(s.get("globals_written", ())) | set(F.get("globals_changed", ())))
    if g or s.get("writes"):
        bits = ([f"writes module-level state: {', '.join(g)}"] if g else []) + ([f"calls {', '.join(sorted(set(s['writes'])))}"] if s.get("writes") else [])
        if F.get("globals_changed"):
            bits.append("the sample run changed " + ", ".join(F["globals_changed"]))
        return Suggestion("side_effects", "Global state or side effects: not safe to retry or run twice", "; ".join(bits) + ". On the cluster every PC has its own copy of "
                          "globals, and the head may run a task again after a PC is lost",
                          "return the value instead of mutating a global: results = ck.map(fn, items), then combine them on your PC\n"
                          "keep ck.config(retries=0) and write files to a per-item name so a repeat overwrites itself", "warn",
                          "correctness: results can differ from the local run", None, {"globals": g, "writes": sorted(set(s.get("writes", ())))})


def _nondet(F: dict[str, Any]) -> Suggestion | None:
    s = F["static"] or {}
    if F.get("differs"):
        return Suggestion("nondeterministic", "Output differs between two runs on the same input",
                          f"{len(F['differs'])} of {F['rerun']} sample items gave different results the second time" +
                          (f" ({', '.join(s['random'][:3])})" if s.get("random") else ""),
                          "seed inside fn, from the item: rnd = random.Random(item['seed'])  # not the global random module\n"
                          "then the cluster result equals the local one", "warn", "correctness: reproducible results", None,
                          {"differs": F["differs"], "sources": s.get("random", [])})
    if s.get("random"):
        return Suggestion("nondeterministic", "May be non-deterministic", f"uses {', '.join(sorted(set(s['random']))[:3])}; the sample happened to agree",
                          "pass a seed in each item and use random.Random(seed)", "info", "reproducible results", None, {"sources": s["random"]})


def _imports(F: dict[str, Any]) -> Suggestion | None:
    s, ck = F["static"] or {}, F["ck"]
    mod = F["mod"]
    here = os.path.dirname(os.path.abspath(getattr(mod, "__file__", "") or ""))
    shipped = {os.path.splitext(os.path.basename(p))[0] for p in ck._cfg["ship"]}
    ext, loc = [], []
    for name in sorted(s.get("imports", ())):
        if name in _STD or name in ("clusterkit", "__future__") or name == (mod.__name__ if mod else ""):
            continue
        if os.path.exists(os.path.join(here, name + ".py")) or os.path.isdir(os.path.join(here, name)):
            if name not in shipped:
                loc.append(name)
        else:
            ext.append(name)
    if ext or loc:
        bits = ([f"{', '.join(ext)} must be installed on every PC that runs the work (the lab PCs are offline: install from a local wheel folder)"] if ext else []) + \
               ([f"your own module(s) {', '.join(loc)} sit beside the script but are not shipped"] if loc else [])
        return Suggestion("missing_imports", "Imports the PCs may not have", "; ".join(bits),
                          (f"ck.config(ship=[{', '.join(repr(n + '.py') for n in loc)}])   # sends your own modules with the job\n" if loc else "") +
                          ("check first: ck.map(lambda_free_probe, range(6)) where probe does `import " + ext[0] + "`; or pip install --no-index on each PC" if ext else ""),
                          "warn" if ext or loc else "info", "avoids a failed job after the upload", None, {"third_party": ext, "local": loc})


def _transfer(F: dict[str, Any]) -> Suggestion | None:
    pl = F.get("plan")
    if not pl or pl.where != "cluster" or not pl.bytes or not pl.cluster_s or pl.transfer_s < 1.0 or pl.transfer_s < 0.4 * pl.cluster_s:
        return None
    alt = ", ".join(f"{k} {_t(v)}" for k, v in pl.transfer_alt.items())
    k1 = pl.finish_by_pcs.get(1)
    return Suggestion("transfer_dominates", f"Moving the data dominates: {_t(pl.transfer_s)} of {_t(pl.cluster_s)}",
                      f"{_mb(pl.bytes)} to move ({_mb(pl.shared_bytes)} to every PC, {_mb(pl.item_bytes)} split); strategy {pl.strategy} ({alt}); "
                      f"estimate on {pl.pcs} PCs {_t(pl.cluster_s)}" + (f", on 1 PC {_t(k1)}" if k1 else ""),
                      "shard it: ck.map(fn, ck.shard(path, parts=k)) so each PC gets only its part, once;\n"
                      "cache it: ck.put(data) once and pass the handle (a PC that has the blob never fetches it again; the second run moves 0 bytes);\n"
                      "or use fewer PCs: ck.map(fn, items, pcs=N), since every extra PC needs its own copy of shared data",
                      "warn", f"up to {_t(pl.transfer_s)} of transfer", pl.transfer_s,
                      {"bytes": pl.bytes, "shared_bytes": pl.shared_bytes, "transfer_s": pl.transfer_s, "cluster_s": pl.cluster_s, "strategy": pl.strategy})


def _cannot_help(F: dict[str, Any]) -> Suggestion | None:
    ck, o = F["ck"], F["o"]
    try:
        pl = ck._plan(F["fn"], F["items"], "call", {**o, "local": "auto"}, profile=(F["per"], F.get("peak_mb") or 0.0))
    except ck.ClusterError:
        return None
    F["plan"], F["slots"] = pl, pl.slots
    if pl.where == "cluster":
        return None
    m = pl.model or {"fixed_s": o["fixed_s"], "item_c": o["item_c"], "speed": 1.0}
    K = pl.all_slots or 12
    kf = 1 - 1.5 / K if K > 1.5 else 0.25
    need_w, need_t = 1.5 * m["fixed_s"] / kf, 1.5 * m["item_c"] / kf
    work = F["n"] * F["per"]
    return Suggestion("cluster_cannot_help", "The cluster cannot help this job", f"{F['n']:,} items x {_t(F['per'])} = {_t(work)} of work; "
                      f"estimate: cluster {_t(pl.cluster_s) if pl.cluster_s else 'n/a'}, local {_t(pl.local_s)}. Break-even (K={K} slots, fixed {_t(m['fixed_s'])}, {m['item_c'] * 1e6:.0f} us per item): cluster wins when total work > "
                      f"{_t(need_w)} and per-item work > {need_t * 1000:.2f} ms",
                      "keep it local: ck.map(fn, items) already does (plan: local); or add work per call (bigger items, more of them)",
                      "info", f"none: local takes about {_t(pl.local_s)}", 0.0,
                      {"total_work_s": work, "need_total_s": need_w, "per_item_s": F["per"], "need_per_item_s": need_t, "local_s": pl.local_s})


def _versions(F: dict[str, Any]) -> Suggestion | None:
    """The PCs' Pythons against this one: pickles, syntax and library wheels must suit the oldest PC. Needs a reachable cluster."""
    ck = F["ck"]
    try:
        vs = ck.pc_python()
    except Exception:
        return None
    mine = "{}.{}".format(*sys.version_info[:2])
    old = min(vs, key=lambda v: tuple(int(x) for x in v.rstrip("t").split(".")))
    if not vs or {v.rsplit(".", 1)[0] for v in vs} == {mine}:
        return None
    pin = f"ck.config(pc_python={'.'.join(old.rstrip('t').split('.')[:2])!r})"
    return Suggestion("python_versions", f"The PCs run Python {', '.join(vs)}; this machine runs {mine}",
                      "code is shipped as source and re-parsed by each PC's python3 (newer syntax fails there), pickles use the lower protocol of both sides, "
                      "and a library (numpy, torch, ...) pickled here may need the same major version there",
                      pin + "   # checks the syntax of your code against that version before shipping and picks the pickle protocol\n"
                      "install the same library versions on the PCs, or return plain Python data (lists, dicts, numbers) from fn",
                      "warn", "avoids a failed job or an unreadable result", None, {"pcs": vs, "local": mine})


def advise(fn: Callable[..., Any], items: list[Any], sample: int, o: dict[str, Any]) -> Advice:
    import clusterkit as ck
    if not items:
        return Advice()
    F = _facts(fn, items, sample, ck, o)
    cc = _cannot_help(F) if F["ran"] else None
    out = [r for r in (_unpicklable(F), _failing(F), _tiny(F), _hot_loop(F), _threads(F), _memory(F), _side_effects(F), _nondet(F), _imports(F), cc, _transfer(F) if F.get('plan') else None, _versions(F) if F['o']['local'] != 'always' else None) if r]
    out += _big(F)
    out.sort(key=lambda s: (_SEV[s.severity], -(s.gain_s or 0)))
    a = Advice(out)
    a.facts = {k: F.get(k) for k in ("n", "per", "cpu_ratio", "thread_speedup", "peak_mb", "prof")}
    return a
