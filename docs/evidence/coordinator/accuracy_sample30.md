# First accuracy measurement: 30 sampled frames (2026-10-29)

**What this is:** a small first measurement, not a validation. Labels: `vision/eval/labels/sample30.json`. Contact sheets: `sheet_people.jpg`, `sheet_person.jpg` (frames 00–14, left to right, top to bottom).

- Footage: `vision/footage/people-detection.mp4` (indoor, high corner) and `person-bicycle-car-detection.mp4` (aerial parking lot), 15 frames each, evenly spaced over the indexed observations.
- Model: `yolov5n.onnx` (COCO), confidence 0.4, CPU, 640 input, accuracy preset (every frame inferred).
- Labels: visible-object counts per frame, by visual inspection. Single annotator (Claude), not human-verified. One ambiguous tiny figure (person_12) is excluded.
- Metric: per-frame count matching (TP = min(label, model), FP = excess model, FN = missing). This is a count-level measurement, not box IoU.

| Class | TP | FP | FN | Precision | Recall |
| --- | --- | --- | --- | --- | --- |
| person | 10 | 0 | 3 | 1.00 | 0.77 |
| bicycle | 1 | 0 | 1 | 1.00 | 0.50 |
| car | 0 | 0 | 3 | n/a | 0.00 |

- Per clip, person: indoor 8/9 found (the miss is a person partly cut off at the frame edge); aerial 2/4 found (the misses are a cyclist and a small figure).
- No false detections on the 21 empty or object-free sampled frames.
- Cars seen from directly above were missed in all 3 sampled frames, although the full run detected `car` 53 times at other moments.
- Over the full index the model also emitted 8 `cell phone` detections (mean confidence 0.44) on footage that has no phones, and 1 `truck` and 1 `motorcycle`. Those are false positives outside this sample.

**Reading:** with 13 people and 3 cars labelled, the uncertainty is wide. Indoor person detection looks usable. Small or overhead objects (aerial cars, cyclists) are weak for yolov5n at 640, and the low-confidence `cell phone` output on empty frames argues for a higher per-class threshold (e.g. 0.6) before phone rules are trusted. The module stays `validation: unvalidated` until a human-labelled set of representative footage exists (procedure: `vision/eval/README.md`).
