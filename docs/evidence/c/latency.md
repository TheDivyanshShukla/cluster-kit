# Inference latency (measured)

CPU: Apple M5
Provider (effective): cpu
Model: vision/models/yolov5n.onnx, input 640x640
Intra-op threads: 4
Warm-up: 5 runs discarded. Timing covers letterbox + ONNX run + decode/NMS (`Engine::run`); frame decode excluded.

| Footage | Frames | Frame size | p50 ms | p95 ms | mean ms | detections/frame (mean) |
|---|---|---|---|---|---|---|
| vision/footage/people-detection.mp4 | 30 | 640x360 | 116.5 | 388.5 | 164.2 | 0.4 |
| vision/footage/person-bicycle-car-detection.mp4 | 30 | 640x360 | 109.4 | 138.5 | 110.9 | 0.4 |

No accuracy is claimed here: there is no labeled ground truth in the repo (see vision/eval/README.md).
