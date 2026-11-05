"""Run history (~/.clusterkit/history.jsonl, override with CK_SDK_HOME) and the model fitted from it. Standard library only."""
from __future__ import annotations
import json, os, statistics, time
from typing import Any

MIN_RUNS = 3


def path() -> str:
    return os.path.join(os.environ.get("CK_SDK_HOME") or os.path.expanduser("~/.clusterkit"), "history.jsonl")


def record(rec: dict[str, Any]) -> None:
    """Append one run (predicted vs actual). Never raises: history is a convenience."""
    try:
        p = path()
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "a", encoding="utf-8") as f:
            f.write(json.dumps({"ts": round(time.time(), 1), **rec}, separators=(",", ":")) + "\n")
    except OSError:
        pass


def load(head: str, last: int = 20, where: tuple[str, ...] = ("cluster", "replanned")) -> list[dict[str, Any]]:
    """The last `last` runs on this head that went to the cluster (oldest first)."""
    try:
        with open(path(), encoding="utf-8") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - (1 << 21)))  # ponytail: last 2 MB only; rotate the file if it ever matters
            lines = f.read().split("\n")
    except OSError:
        return []
    out: list[dict[str, Any]] = []
    for l in lines:
        try:
            r = json.loads(l)
        except ValueError:
            continue
        if isinstance(r, dict) and r.get("head") == head and r.get("where") in where:
            out.append(r)
    return out[-last:]


def fit(head: str, last: int = 20) -> dict[str, Any] | None:
    """Robust (median) overhead constants and PC speed from recent runs; None until MIN_RUNS usable runs exist.
    fixed_s: seconds of overhead that do not depend on n (submit, ship, dispatch, result detection);
    item_c: seconds per item (encode + result fetch + decode); speed: PC throughput per slot relative to this PC."""
    runs = load(head, last)
    fixed = [r["fixed_s"] for r in runs if r.get("fixed_s") is not None and not r.get("failed")]
    if len(fixed) < MIN_RUNS:
        return None
    med = statistics.median
    items = [r["item_c"] for r in runs if r.get("item_c") is not None]
    speeds = [r["speed"] for r in runs if r.get("speed") and r.get("run_s", 0) >= 1.0]
    return {"runs": len(fixed), "fixed_s": max(0.0, med(fixed)), "item_c": max(0.0, med(items)) if items else None,
            "speed": min(20.0, max(0.05, med(speeds))) if len(speeds) >= MIN_RUNS else None}


def link(head: str, last: int = 20) -> dict[str, float]:
    """Observed transfer speed to/from the head in bytes/s: {'up': .., 'down': ..} (a key is missing until seen)."""
    out: dict[str, float] = {}
    for d in ("up", "down"):
        v = [r["bytes"] / r["s"] for r in load(head, last * 3, ("link",)) if r.get("dir") == d and r.get("s", 0) > 0][-last:]
        if v:
            out[d] = statistics.median(v)
    return out
