"""scikit-learn: ck.sweep over hyperparameters with cross-validation, and ck.map over model fits. Needs: scikit-learn, numpy.
python3 examples/sdk/ml_sklearn.py"""
import os
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import clusterkit as ck
from sklearn.datasets import make_classification
from sklearn.ensemble import RandomForestClassifier
from sklearn.model_selection import cross_val_score

X, y = make_classification(n_samples=600, n_features=20, random_state=0)  # module-level data is rebuilt on each PC from the same source


def cv(n_estimators: int, max_depth: int) -> float:
    m = RandomForestClassifier(n_estimators=n_estimators, max_depth=max_depth, random_state=0, n_jobs=1)
    return float(cross_val_score(m, X, y, cv=3).mean())


def fit_seed(seed: int) -> float:
    return float(RandomForestClassifier(n_estimators=30, random_state=seed).fit(X[:400], y[:400]).score(X[400:], y[400:]))


if __name__ == "__main__":
    ck.config(local=os.environ.get("CK_LOCAL", "auto"))  # CK_LOCAL=never forces the cluster even for this small demo
    rows = ck.sweep(cv, {"n_estimators": [10, 30, 60], "max_depth": [3, 6, None]}, key=lambda s: s, top=3)
    for r in rows:
        print(r["params"], "cv accuracy {:.3f}".format(r["result"]))
    best = rows[0]["params"]
    assert abs(cv(**best) - rows[0]["result"]) < 1e-9  # deterministic: same as a local run
    scores = ck.map(fit_seed, range(8))
    print("sklearn ok: 8 fits, mean holdout %.3f" % (sum(scores) / len(scores)))
