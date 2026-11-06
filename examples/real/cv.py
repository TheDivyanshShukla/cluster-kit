"""Image pipeline over 5,000 generated pictures: resize, blur, find edges, histogram features, and save every edge map.

  python3 examples/real/cv.py local     # a loop in one process
  python3 examples/real/cv.py cluster   # 100 tasks of 50 images; each PC leaves its edge PNGs in $CK_OUT, the head stores them
Each image is drawn from its number (random shapes plus noise), so every PC makes exactly the same picture. The result is a hash of
all feature vectors and of all PNG files; both runs must agree. Needs numpy, pillow and opencv-python-headless here and on every PC.
"""
import hashlib
import json
import os
import sys
import time

try:
    import _path  # noqa: F401
except ImportError:
    pass
import cv2
import numpy as np
from PIL import Image, ImageDraw
import clusterkit as ck

N_IMAGES, PER_TASK = 5000, 50


def draw(i: int) -> "np.ndarray":
    """Picture number i: 384x384 RGB with a few random rectangles, circles and lines, plus noise."""
    rng = np.random.default_rng(i)
    im = Image.new("RGB", (384, 384), tuple(int(v) for v in rng.integers(0, 255, 3)))
    dr = ImageDraw.Draw(im)
    for _ in range(int(rng.integers(3, 9))):
        x0, x1 = sorted(int(v) for v in rng.integers(0, 384, 2))
        y0, y1 = sorted(int(v) for v in rng.integers(0, 384, 2))
        col = tuple(int(v) for v in rng.integers(0, 255, 3))
        kind = int(rng.integers(0, 3))
        (dr.rectangle if kind == 0 else dr.ellipse if kind == 1 else dr.line)([x0, y0, x1, y1], **({"fill": col} if kind < 2 else {"fill": col, "width": 3}))
    a = np.asarray(im).astype(np.int16) + rng.normal(0, 12, (384, 384, 3)).astype(np.int16)
    return a.clip(0, 255).astype(np.uint8)


def process(i: int) -> "tuple[str, bytes]":
    """resize -> blur -> edges -> 16-bin histograms. Returns (feature bytes hash, edge-png bytes)."""
    img = cv2.resize(draw(i), (192, 192), interpolation=cv2.INTER_AREA)
    blur = cv2.GaussianBlur(img, (7, 7), 1.6)
    edges = cv2.Canny(cv2.cvtColor(blur, cv2.COLOR_RGB2GRAY), 60, 140)
    feats = np.concatenate([np.histogram(blur[..., c], bins=16, range=(0, 256))[0] for c in range(3)] + [np.histogram(edges, bins=2, range=(0, 256))[0]])
    png = cv2.imencode(".png", edges)[1].tobytes()
    return hashlib.sha256(feats.astype(np.int64).tobytes()).hexdigest(), png


def batch(first: int) -> "list[tuple[int, str, str]]":
    """One task: images first .. first+49. Saves the PNGs in $CK_OUT (when run on the cluster) and returns the hashes."""
    out = os.environ.get("CK_OUT")
    res = []
    for i in range(first, min(first + PER_TASK, N_IMAGES)):
        f, png = process(i)
        if out:
            with open(os.path.join(out, f"edge_{i:05d}.png"), "wb") as fh:
                fh.write(png)
        res.append((i, f, hashlib.sha256(png).hexdigest()))
    return res


def main(mode: str) -> None:
    t0 = time.time()
    starts = list(range(0, N_IMAGES, PER_TASK))
    if mode == "local":
        rows = [r for s in starts for r in batch(s)]
    else:
        ck.config(local="never", name="real-cv", timeout_s=900)
        rows = [r for part in ck.map(batch, starts) for r in part]
    rows.sort()
    secs = round(time.time() - t0, 2)
    print(json.dumps({"digest": hashlib.sha256(json.dumps(rows).encode()).hexdigest()[:16], "images": len(rows),
                      "png_hashes": hashlib.sha256("".join(r[2] for r in rows).encode()).hexdigest()[:16], "secs": secs}))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "local")
