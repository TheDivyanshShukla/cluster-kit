"""python -m clusterkit bench | advise path/script.py:func [--items EXPR]"""
import argparse, importlib.util, os, sys


def load(spec):
    path, _, name = spec.partition(":")
    if not name:
        sys.exit("give the function as path/script.py:func")
    path = os.path.abspath(path)
    sys.path.insert(0, os.path.dirname(path))
    stem = os.path.splitext(os.path.basename(path))[0]
    m = importlib.util.spec_from_file_location(stem, path)
    mod = importlib.util.module_from_spec(m)
    sys.modules[stem] = mod  # not __main__: the script's `if __name__ == "__main__":` block must not run
    m.loader.exec_module(mod)
    fn = mod
    for part in name.split("."):
        fn = getattr(fn, part)
    return mod, fn


def main(argv=None):
    ap = argparse.ArgumentParser(prog="python -m clusterkit")
    ap.add_argument("--head", help="head URL (default CK_HEAD or http://127.0.0.1:7702)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("bench", help="time every overhead step of a forced-cluster job with empty work")
    b.add_argument("--sizes", default="1,100,10000")
    b.add_argument("--reps", type=int, default=3)
    a = sub.add_parser("advise", help="suggest how to make a function faster on the cluster")
    a.add_argument("target", help="path/script.py:func")
    a.add_argument("--items", help="python expression for the items, evaluated in the script's namespace (default: ITEMS in the script)")
    a.add_argument("--sample", type=int, default=20)
    args = ap.parse_args(argv)
    import clusterkit as ck
    ck.connect(args.head)
    if args.cmd == "bench":
        from . import _bench
        _bench.main(tuple(int(x) for x in args.sizes.split(",")), args.reps)
        return
    mod, fn = load(args.target)
    expr = args.items or "ITEMS"
    try:
        items = eval(expr, dict(vars(mod)))
    except NameError:
        sys.exit("no items: pass --items 'range(1000)' (or define ITEMS in the script)")
    print(ck.advise(fn, items, sample=args.sample))


if __name__ == "__main__":
    main()
