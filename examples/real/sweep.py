"""Hyperparameter sweep: 240 scikit-learn SVM configurations, 5-fold cross-validated, spread over the cluster.

  python3 examples/real/sweep.py local     # a plain loop
  python3 examples/real/sweep.py cluster   # ck.sweep: one configuration per task
The best configuration (and the top 5) must be identical in both. Needs numpy and scikit-learn here and on every PC.
"""
import hashlib
import json
import sys
import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import clusterkit as ck
from sklearn.datasets import load_digits  # imported at the top so a PC's warm Python worker has it ready for every task
from sklearn.model_selection import cross_val_score
from sklearn.svm import SVC

GRID = {"kernel": ["rbf", "poly", "sigmoid"], "C": [0.1, 0.3, 1, 3, 10, 30, 100, 300],
        "gamma": [0.0003, 0.0006, 0.001, 0.002, 0.003, 0.005, 0.008, 0.012, 0.02, 0.03]}  # 3 x 8 x 10 = 240 configurations


def score(kernel: str, C: float, gamma: float) -> dict:
    """Mean 5-fold accuracy of one SVM on the bundled digits. Runs on a PC."""
    d = load_digits()
    s = cross_val_score(SVC(kernel=kernel, C=C, gamma=gamma), d.data, d.target, cv=5)
    return {"score": round(float(s.mean()), 6), "cfg": f"{kernel} C={C} gamma={gamma}"}


def rank(res: dict) -> tuple:  # best score first; ties broken by the settings so both runs agree
    return (res["score"], res["cfg"])


def main(mode: str) -> None:
    t0 = time.time()
    if mode == "local":
        import itertools
        rows = [{"params": dict(zip(GRID, v)), "result": score(*v)} for v in itertools.product(*GRID.values())]
        rows.sort(key=lambda r: rank(r["result"]), reverse=True)
        top = rows[:5]
    else:
        ck.config(local="never", name="real-sweep")
        top = ck.sweep(score, grid=GRID, key=rank, top=5)
    text = json.dumps([[r["params"], r["result"]] for r in top], sort_keys=True)
    print(json.dumps({"digest": hashlib.sha256(text.encode()).hexdigest()[:16], "best": top[0]["params"], "secs": round(time.time() - t0, 2)}))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "local")
