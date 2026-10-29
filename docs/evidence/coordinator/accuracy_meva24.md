# Accuracy check on MEVA school CCTV (2026-10-29)

**What this is:** a small check, not a validation. Labels: `vision/eval/labels/meva_sample24.json`.

- Footage: the first ~110 s of four MEVA cameras (G419, G420, G421, G423, 7 March 2018), as indexed by the live pipeline.
- Model and pipeline: `yolo26s.onnx` on Core ML, confidence 0.35, motion-gated detection (balanced settings, 960 px).
- Sampling: 24 frames the pipeline indexed, picked 3 with and 3 without a person detection per camera. This over-represents frames with people, so it measures false alarms and misses, not how common people are.
- Labels: people visible in each frame, by one annotator (Claude), not human-verified. 5 frames were too ambiguous to call (partial figures at the frame edge or through glass) and are excluded.

| Result (19 labelled frames) | Count |
| --- | --- |
| Correct detections (TP) | 13 |
| False detections (FP) | 0 |
| Missed people (FN) | 4 |
| Precision | 1.00 |
| Recall | 0.76 |

Where it missed: two people only partly in frame at the left edge of G421, and two very small figures far down the G423 hallway. Everything it found was a real person, at confidences 0.59–0.95 (0.45 on one ambiguous frame that was excluded).

Frames on which the detector did not run (static scene) count as "no person". That matched the labels in every sampled static frame, but only a small number were sampled.

**Reading:** with 17 labelled people the uncertainty is large (one miss is about 6 points of recall). The picture agrees with the earlier 30-frame check: few false alarms, with misses on small or partial people. Raising the decode width (setup: 1280) or lowering the confidence to 0.25 would be the first things to try for the hallway camera.
