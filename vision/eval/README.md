# Model evaluation procedure

**Status: no labeled ground truth exists in this repo.** `vision/footage/*.mp4` are unlabeled. No precision, recall, count-error or false-alert numbers are reported anywhere, and none should be quoted until a label file below exists and the script has run. `models::validate` therefore reports `validation: "unvalidated"`.

## Measured so far (no labels needed)

`docs/evidence/c/latency.md`: p50/p95 per-frame latency on real footage frames, with CPU name, provider, input size, threads. Reproduce:

```
CARGO_TARGET_DIR=rust/target-c cargo test --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline bench_latency -- --ignored --nocapture
```

Extraction uses ffmpeg to raw RGB at test time (nothing is written to the repo except the report).

## Labeled set procedure (to do by a human)

1. Pick N >= 100 frames spread over each footage file: `ffmpeg -i X.mp4 -vf fps=0.5 vision/eval/frames/%04d.jpg` (frames dir is not committed).
2. A person (not the model) labels every frame in `vision/eval/labels.jsonl`, one line per frame: `{"frame":"0007.jpg","objects":[{"class":"person","bbox":[x0,y0,x1,y1]}, ...]}` with normalized coordinates; mark classes that were *not* labeled (e.g. `"unlabeled":["cell phone"]`) so they are excluded rather than counted as zero.
3. Run the model on the same frames at the production threshold and per-class thresholds; match predictions to labels with IoU >= 0.5, same class.
4. Report per class: TP, FP, FN, precision, recall; per-frame count error (|pred - true|, mean and max); false-alert rate for any rule using the module.
5. Write `vision/eval/results/<module id>.json` with those numbers, the label file hash, model hash and date. Its existence flips `validation` to `evaluated:<path>`.

No fixture is committed: labeling frames honestly requires viewing them, and an unchecked fixture would be invented ground truth.
