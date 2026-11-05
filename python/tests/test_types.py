"""Every function and method of the package has resolvable, complete annotations (no mypy/pyright needed); py.typed ships."""
import inspect, os, sys, typing, unittest
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [HERE, os.path.join(HERE, "..")]
import clusterkit as ck
from clusterkit import _advise, _bench, _benchfn, _bulk, _hist, _runner, __main__ as cli


def callables(mod):
    for n, o in vars(mod).items():
        if getattr(o, "__module__", None) != mod.__name__:
            continue
        if inspect.isfunction(o):
            yield f"{mod.__name__}.{n}", o
        elif inspect.isclass(o):
            for m, f in vars(o).items():
                f = getattr(f, "fget", f)
                if inspect.isfunction(f) and f.__code__.co_filename != "<string>":  # not dataclass-generated
                    yield f"{mod.__name__}.{n}.{m}", f


class Types(unittest.TestCase):
    def test_hints_resolve_and_are_complete(self):
        n = 0
        for mod in (ck, _advise, _bench, _benchfn, _bulk, _hist, _runner, cli):
            for name, f in callables(mod):
                h = typing.get_type_hints(f)  # raises NameError/TypeError when an annotation cannot be resolved
                sig = inspect.signature(f)
                missing = [p for p in sig.parameters if p not in ("self", "cls") and p not in h]
                self.assertEqual(missing, [], f"{name}: unannotated parameters")
                if f.__name__ != "__init__":
                    self.assertIn("return", h, f"{name}: no return annotation")
                n += 1
        self.assertGreater(n, 100)

    def test_public_surface(self):
        for name in ck.__all__:
            o = getattr(ck, name)
            if inspect.isfunction(o):
                self.assertTrue(typing.get_overloads(o) or typing.get_type_hints(o) if hasattr(typing, "get_overloads") else typing.get_type_hints(o))
        self.assertEqual(typing.get_type_hints(ck.Plan)["where"], typing.Literal["local", "cluster"])
        self.assertEqual(typing.get_type_hints(ck.SweepRow), {"params": dict[str, typing.Any], "result": typing.Any})
        self.assertTrue(ck.Suggestion.__dataclass_fields__ and ck.Plan.__dataclass_fields__)

    def test_py_typed(self):
        self.assertTrue(os.path.exists(os.path.join(os.path.dirname(ck.__file__), "py.typed")))
        with open(os.path.join(os.path.dirname(ck.__file__), "pyproject.toml")) as f:
            self.assertIn("py.typed", f.read())


if __name__ == "__main__":
    unittest.main()
