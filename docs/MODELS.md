# Watch models: manifests, fusion, relations

Code: `rust/src/watch/models/` (validate, fuse, relate, support, summarize), `yolo.rs` (ONNX runtime), `track.rs`.

## Module manifest (`vision/modules.json`)

```json
{
  "id": "objects", "name": "Objects", "kind": "object",
  "model": "vision/models/yolov5n.onnx",
  "labels": ["person", "cell phone"],
  "conf": 0.4, "confirm": 3, "gflops": 4.5, "enabled": true
}
```

- `kind`: `object` (any detector layout in "Supported output layouts"), `weapon` (object detector whose classes come from its own model), `fight` (built-in heuristic, no model).
- `model`: path relative to the repo root (or absolute).
- `labels` has two meanings, decided by `models::validate` from the real output width:
  - **Stock COCO model** (output width 85 or 84, an end-to-end `[1,K,6]` head, or a DETR head with 80/91 classes): `labels` is only an *interest filter*; every name must be a COCO name. The ModelVersion carries the full canonical ordered COCO-80 list (`models/labels.rs`).
  - **Custom model**: `labels` must be the *full ordered class list*, one entry per model class (width 5+N for yolov5-style, 4+N for yolov8-style). A short or reordered list is rejected with `invalid_request` and a message naming the expected counts.
  - A model with no `model` file and `kind: weapon` is `unsupported`. Weapon classes are never invented; they come from the model's own label list.
- Validation facts (all measured, not declared): SHA-256 of the artifact, input size from the ONNX input, output layout from a real forward pass, effective execution provider (this build registers none, so `cpu`), `validation = "unvalidated"` until `vision/eval/results/<module id>.json` exists.

Validated result for the stock module (from `validate()`, test `validate_yolov5n_reports_real_layout_and_80_labels`):

```json
{ "id": "mv_objects_<hash8>", "version": "sha256:<hash16>", "artifact_hash": "<64 hex>",
  "adapter": "object", "output_schema": "yolov5", "labels": ["person", "bicycle", "... 80 ..."],
  "thresholds": {"*": 0.4}, "provider": "cpu", "input_size": 640, "validation": "unvalidated" }
```

## Supported output layouts (auto-detected from a real forward pass)

| Layout (`output_schema`) | Output tensor(s) | Preprocess | Decode |
|---|---|---|---|
| `yolov5` | `[1,N,85]` | letterbox (grey 114), /255 | objectness x class, per-class NMS 0.45 |
| `yolov8` (v8, 11, ultralytics RT-DETR rows) | `[1,84,N]` (transposed automatically) | letterbox, /255 | best class, per-class NMS 0.45 |
| `e2e` (YOLO26 / YOLOv10 NMS-free export) | `[1,K,6]` = x1,y1,x2,y2,score,class in input pixels, K <= 1000 | letterbox, /255 | threshold only, no NMS |
| `detr` (RF-DETR, D-FINE/DEIM raw heads) | boxes `[1,Q,4]` (cx,cy,w,h in 0..1) + logits `[1,Q,C]` | stretch-resize (no padding), ImageNet mean/std | sigmoid, best class per query; C=91 maps COCO category ids to COCO-80 (`yolo::COCO91_IDS`) |

fp16 inputs/outputs are handled. A one-class yolov5 (`[1,N,6]`, N in the thousands) is told apart from `e2e` by its row count.
Custom `e2e`/`detr` models need the full ordered `labels` list in the module (the class count is not in the output width).
`Engine::load(path, threads)` / `Engine::run(rgb, w, h, conf, labels, interest)` are the same for every layout.
Export recipes: `YOLO('yolo26s.pt').export(format='onnx', imgsz=640, opset=17, simplify=True, end2end=True)` and
`from rfdetr import RFDETRNano; RFDETRNano().export(output_dir=...)` (outputs `dets`, `labels`).

## Landscape (checked 2026-10-29) and what was exported

COCO val AP is 50:95 at the model's own input size. "CPU" is suitability for this product (a CPU-only worker, one or a few streams).

| Model | COCO AP | Params / FLOPs | License | ONNX | CPU suitability | Here |
|---|---|---|---|---|---|---|
| YOLO26 n / s / m ([docs](https://docs.ultralytics.com/models/yolo26/)) | 40.9 / 48.6 / 53.1 | 2.4M 5.5G / 9.5M 20.9G / 20.4M 68.4G | AGPL-3.0 (or paid Enterprise) | yes, NMS-free `[1,300,6]` | best of the YOLOs; Ultralytics quotes 38.9 / 87.2 / 220 ms CPU-ONNX on a Xeon | n, s exported and benchmarked |
| YOLO11 n / s ([docs](https://docs.ultralytics.com/compare/yolo11-vs-rtdetr)) | 39.5 / 47.0 | 2.6M 6.5G / 9.4M 21.5G | AGPL-3.0 | yes | good, needs NMS | n, s benchmarked |
| YOLOv5n (shipped) | 28.0 | 1.9M 4.5G | AGPL-3.0 | yes | fastest, least accurate | baseline |
| RF-DETR Nano / Small / Medium / Large ([repo](https://github.com/roboflow/rf-detr), [blog](https://blog.roboflow.com/rf-detr-nano-small-medium/)) | 48.4 / 53.0 / 54.7 / 56.5 (XLarge 58.6 is PML-1.0, not Apache) | 30.5M / 32.1M / 33.7M / 33.9M; 384 / 512 / 576 / 704 px | **Apache-2.0** (code and N-L weights) | yes, `export()` | transformer: slow on the bundled ORT 1.22 CPU kernels, usable with ORT 1.31 (115 ms nano) | nano, small exported and benchmarked |
| DEIMv2 Atto..M ([repo](https://github.com/Intellindust-AI-Lab/DEIMv2), [paper](https://arxiv.org/abs/2509.20787)) | 23.8 / 31.0 / 38.5 / 43.0 / 50.9 / 53.0 | N: 3.6M 6.8G; S: 9.7M 49.5G | "DEIMv2 License" (repo text; Apache-2.0 on a mirror, unverified) | export script in repo | N/Pico look CPU-friendly | not exported: licence unclear, checkpoints on Google Drive |
| D-FINE / DEIM (DETR family) | up to 54-57 for L/X | n/a for N here | Apache-2.0 (per third-party comparison, unverified) | yes | S/N variants plausible | not exported; the `detr` adapter handles the raw `[boxes, logits]` head, but the official ONNX takes `orig_target_sizes` and returns post-processed tensors (a different head, not implemented) |
| RT-DETR / RT-DETRv2 ([repo](https://github.com/lyuwenyu/RT-DETR)) | 46-48 (R18/S) | 20M 60G | Apache-2.0 (original); Ultralytics' packaging is AGPL | yes | 60 GFLOPs is too heavy for CPU | not exported |
| LW-DETR | n/a | n/a | Apache-2.0 | n/a | n/a | not researched beyond name: no source found in search |

Numbers copied from the cited pages; they were not re-measured except where the benchmark file says so.

## Choosing a model

Measured here (Apple M5, 30 labelled frames, details in `docs/evidence/m/models_bench.md`):

- **Speed, if you can turn on CoreML** (`--features coreml`): `yolo26n` 19 ms, `yolo26s` 31 ms p50 per frame. Without CoreML the bundled ORT 1.22 CPU path is 100-600 ms per frame for these.
- **Accuracy on the sample:** `yolo26s` (recall 0.89-0.94, precision 0.94-1.0) and both RF-DETRs (recall 0.94-1.0, precision 0.94-0.95) beat `yolov5n` (recall 0.61-0.67). Sample is 18 objects; treat gaps of one or two objects as noise.
- **Licence:** every YOLO here is AGPL-3.0 (same as the shipped yolov5n). RF-DETR Nano/Small are Apache-2.0 and the only permissively licensed option that was exported and measured.

Recommendation: default `yolo26s` with CoreML on at conf 0.35 (best speed/accuracy while the licence position is unchanged from yolov5n), `yolo26n` as the cheap tier;
switch to `rfdetr-small` (conf 0.40) when AGPL is a problem or when sources are analysed at <= 2 fps, and re-measure after the bundled ORT is upgraded.

Example module entries (not wired into `vision/modules.json`; the coordinator switches it):

```json
{ "id": "objects", "name": "Objects", "kind": "object", "model": "vision/models/yolo26s.onnx",
  "labels": ["person", "bicycle", "car", "motorcycle", "bus", "truck", "backpack", "handbag", "suitcase", "cell phone"],
  "conf": 0.35, "confirm": 3, "gflops": 20.9, "enabled": true }
```
```json
{ "id": "objects", "name": "Objects (Apache-2.0)", "kind": "object", "model": "vision/models/rfdetr-small.onnx",
  "labels": ["person", "bicycle", "car", "motorcycle", "bus", "truck", "backpack", "handbag", "suitcase", "cell phone"],
  "conf": 0.4, "confirm": 3, "gflops": 0, "enabled": true }
```

## CoreML (Apple GPU / Neural Engine)

Works offline with the cached ORT static library: in `rust/Cargo.toml` add `ort` feature `"coreml"` (or a crate feature `coreml = ["ort/coreml"]`).
`Engine::load` registers `CoreMLExecutionProvider` under `#[cfg(feature = "coreml")]`, unsupported ops fall back to CPU, and
`models::effective_provider()` reports `coreml`. First load compiles the model (seconds, longer for RF-DETR). Not helpful for transformer models.

## Licensing

YOLOv5 (Ultralytics) is licensed **AGPL-3.0**; YOLOv8, YOLO11 and YOLO26 likewise (Ultralytics also sells an Enterprise licence). RF-DETR Nano/Small/Medium/Large are Apache-2.0. The shipped `yolov5n.onnx` carries that license, which has network-use source-offer obligations if you offer the service to others. Review before distributing the product, or swap in a permissively licensed detector via the manifest below. This repo does not make a legal determination.

## Adding a custom model

1. Export to ONNX with a square float32/float16 input (e.g. `1x3x640x640`) and a single detection output (`[1,N,5+C]` yolov5 or `[1,4+C,N]` yolov8). End-to-end (NMS-free) `[1,K,6]` and DETR `[boxes, logits]` exports are supported.
2. Put it under `vision/models/` and add a module with `kind: object` (or `weapon`) and the **complete ordered** `labels`.
3. Run the head's model listing / `models::validate`; fix any `invalid_request` message. Check `artifact_hash`, `output_schema`, `input_size`.
4. Add labeled frames and run the procedure in `vision/eval/README.md` before treating it as anything but `unvalidated`.

## Fusion policy (`models::fuse`)

Two detections are one physical object when: different modules, same class, IoU >= threshold. The higher-confidence box survives; provenance becomes `fused:<module_a>,<module_b>` (all contributing module ids, sorted). Boxes from the same module are never fused (per-module NMS already ran). Different classes are never fused. Counting must run on fused output so one person seen by two modules counts once.

## Relations

| Predicate | State | Provider |
|---|---|---|
| `near` | Supported, **approximate**: object centre inside a person box, attached to the smallest containing person. Method `bbox-proximity`, module `mv_proximity`. Does not prove contact or holding. | built in |
| `holding` | **Unsupported** unless an installed ModelVersion has adapter `relation` and declares `holding`. None exists in this repo, so `support()` returns `unsupported` and rule/queries must say so. | none |

`summarize` counts distinct subject persons per `predicate:object_class` (by track id if present, else detection id). Classes not in `analyzed_classes` are absent, meaning Unknown (not zero). `near:<class>` is `Known(0)` when persons and that class are both analyzed.

## Tracker

`Tracker::scoped("<source>/<media>/<run>")` gives deterministic ids `"<scope>:t<n>"`. Greedy IoU association, class-matched, one-to-one, against a constant-velocity predicted box; tracks survive `max_age` (5) missed inference frames. Ids are short-lived and per source/media/run, never identities.

## Inference vs preview resolution

`Engine::input_size()` (640 for yolov5n) is what the detector sees: any frame is letterboxed to it. Preview width must not be tied to it. See integration notes in the Agent C report for the worker change.
