# Detector benchmark: speed and accuracy on this Mac (2026-10-29)

Machine: Apple M5 (4 performance + 6 efficiency cores), macOS, `cargo --release` (lto), ONNX Runtime **1.22** as bundled by `ort 2.0.0-rc.10`.
Harness: `models/tests.rs::bench_models` (ignored test; `MBENCH="name=path,..." MTHREADS=4`). It decodes the 30 labelled frames
(`vision/eval/labels/sample30.json`, 640x360, ffmpeg at the labelled `media_time_us`) and calls `Engine::run`, so the time is
**preprocess + ORT + decode/NMS** per frame (6 passes over 30 frames, first 2 discarded = 120 timed runs).

**Caveat on latency.** Other agents were building and running at the same time (load average 20-45 on 10 cores). p50/p95 are inflated and
noisy; `min` is the best proxy for an idle machine. Re-measure on an idle box before quoting an absolute number. Accuracy is unaffected.

## Latency (ms, threads = 4 performance cores)

| Model | Input | Layout | CPU p50 | CPU p95 | CPU min | CoreML p50 | CoreML p95 | CoreML min |
|---|---|---|---|---|---|---|---|---|
| yolov5n (shipped, fp16) | 640 | yolov5 `[1,N,85]` | 46 | 55 | 42 | 310 | 574 | 94 |
| yolo11n | 640 | yolov8 `[1,84,N]` | 67 | 187 | 54 | **17** | 24 | 15 |
| yolo11s | 640 | yolov8 | 254 | 926 | 169 | 25 | 34 | 23 |
| yolo26n (end-to-end) | 640 | e2e `[1,300,6]` | 586* | 1295* | 108 | **19** | 22 | 16 |
| yolo26s (end-to-end) | 640 | e2e | 465* | 1562* | 163 | 31 | 63 | 27 |
| rfdetr-nano | 384 | detr `[1,300,4]`+`[1,300,91]` | 1377 | 2273 | 692 | 376 | 848 | 201 |
| rfdetr-small | 512 | detr | 3321* | 9442* | 1401 | 519 | 1186 | 243 |

\* a load spike during that run (the same model measured 158 p50 / 107 min seconds earlier). CPU rows are the least reliable.

Reference outside the Rust build, same machine, python `onnxruntime` (yolo26n e2e, 4 threads, also under load):

| ORT version | Provider | yolo26n p50 / min | rfdetr-nano p50 / min |
|---|---|---|---|
| 1.22.0 (what `ort rc.10` bundles) | CPU | 153 / 125 | 1271 / 623 |
| 1.31.1 (current pip) | CPU | **28 / 16** | **115 / 82** |
| 1.22.0 | CoreML | 15 / 13 | 506 / 235 |
| 1.31.1 | CoreML | 12 / 9 | 177 / 123 |

Findings: (1) the bundled ORT 1.22 CPU kernels are 5-8x slower than ORT 1.31 on the M5, so upgrading the bundled runtime is worth
more than any model change (needs network for the new binaries; `ort rc.15` is in the cargo registry but its ORT binary is not cached,
so it cannot be done offline); (2) CoreML works in the Rust build, see below, and gives 10-25x on YOLO; (3) transformer models
(RF-DETR) partition badly onto CoreML (94 partitions) and are not helped much.

## Accuracy on the 30 labelled frames (18 labelled objects: 13 person, 3 car, 2 bicycle)

Per-frame count matching over person+bicycle+car (TP = min(label, model), FP = excess, FN = missing), same method as
`docs/evidence/coordinator/accuracy_sample30.md`. Model outputs are identical on CPU and CoreML (checked). Labels were written by one
annotator (Claude) from the frames; with 18 objects one object is 5.5 points of recall, so differences of 1-2 objects are **not** significant.

| Model | conf 0.25 P / R | conf 0.3 P / R | conf 0.4 P / R | conf 0.5 P / R | conf 0.6 P / R | chosen conf |
|---|---|---|---|---|---|---|
| yolov5n | 0.92 / 0.67 | 1.00 / 0.67 | 1.00 / 0.61 | 1.00 / 0.61 | 1.00 / 0.56 | 0.30 |
| yolo11n | 1.00 / 0.78 | 1.00 / 0.78 | 1.00 / 0.67 | 1.00 / 0.61 | 1.00 / 0.56 | 0.30 |
| yolo11s | 0.88 / 0.83 | 0.88 / 0.83 | 0.94 / 0.83 | 0.94 / 0.83 | 0.92 / 0.67 | 0.40 |
| yolo26n (e2e) | 1.00 / 0.78 | 1.00 / 0.72 | 1.00 / 0.61 | 1.00 / 0.61 | 1.00 / 0.61 | 0.25 |
| yolo26s (e2e) | 0.89 / 0.94 | 0.94 / 0.94 | 1.00 / 0.89 | 1.00 / 0.78 | 1.00 / 0.67 | 0.30-0.40 |
| rfdetr-nano | 0.90 / 1.00 | 0.95 / 1.00 | 0.94 / 0.94 | 0.94 / 0.89 | 1.00 / 0.83 | 0.30-0.40 |
| rfdetr-small | 0.95 / 1.00 | 0.95 / 1.00 | 0.95 / 1.00 | 0.94 / 0.94 | 0.94 / 0.94 | 0.40 |

Thresholds were picked on the same 30 frames, so P/R at the chosen conf is optimistic.

## Reproduce

```
CARGO_TARGET_DIR=rust/target-m cargo test --release --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline --no-run
MTHREADS=4 MBENCH="yolo26s=vision/models/yolo26s.onnx,rfdetr-nano=vision/models/rfdetr-nano.onnx" rust/target-m/release/deps/cw-<hash> bench_models --ignored --nocapture
```
CoreML: add `coreml = ["ort/coreml"]` to `[features]` and `"coreml"` to the `watch` feature list (or build with `--features watch,coreml`);
`yolo.rs` registers the provider under `#[cfg(feature = "coreml")]` and `models::effective_provider()` then reports `coreml`.
Verified by building a scratch copy with `ort` feature `coreml` fully offline (no new crates; the cached static `libonnxruntime.a` contains the CoreML provider).

## Recommendation

See `docs/MODELS.md` "Choosing a model".
