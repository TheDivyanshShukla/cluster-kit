# Detection pipeline benchmark: `detect_mode` full vs motion (Agent P)

Reproduce: `CW_BENCH_WIDE=<dir> CW_BENCH_OUT=out.md cargo test --release ... pipeline_bench -- --ignored --nocapture`
(`rust/src/watch/worker/pipe.rs`, module `bench`). Model yolov5n, conf 0.4, CPU, Apple M5. It runs the same `Pipe::plan` / `run_plan` the worker uses on decoded frames, with the worker's sampling (`every`). Blur/blank gates are not applied in either mode. yolo26s/CoreML was NOT rerun (time).

Rows "WIDE" are the same two clips shrunk to 384x216 inside a 1280x720 grey canvas (ffmpeg pad), i.e. small objects on a wide camera. This is a synthetic stand-in for a high-resolution camera; the shipped footage is only 768x432.

Recall = sum(min(predicted, true)) / sum(true) over the 30 hand-labelled frames in `vision/eval/labels/sample30.json` (9 labelled objects per clip, so each object is ~11 points; treat as coarse; labels are single-annotator, not human-verified). "indexed only" = nearest detector-pass observation within 100 ms, else unknown counted as missed. "+tracked/static" = nearest handled frame within 100 ms of any kind (tracker prediction, or static = known empty). "Extra" (false extra objects in the labelled classes) was 0 everywhere.

**Wall and detector seconds are NOT reliable**: other agents loaded the machine (load average 17-48 during these runs, see `uptime`), so identical configs varied 2-5x between runs. Use the operation counts (detector frames, model runs, pixels) as the comparison.

|  file|config|sampled|detector frames|model runs|static|tracked|keyframes|crop frames|full fallbacks|px inferred / px full|wall s|detector s|recall (indexed only)|recall (+tracked/static)|extra|exact frames|unknown  |
| ---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|--- |
|  people-detection.mp4|full / every 2 / 512 px (old balanced)|298|298|298|0|0|0|0|0|100 %|24.5|23.7|8/9 = 89 % (0 frames unknown)|8/9 = 89 %|0|14/15|0  |
|  people-detection.mp4|full / every 1 / 640 px (old accuracy)|596|596|596|0|0|0|0|0|100 %|48.1|47.2|8/9 = 89 % (0 frames unknown)|8/9 = 89 %|0|14/15|0  |
|  people-detection.mp4|motion / balanced preset|298|121|121|126|51|25|0|96|41 %|11.7|10.0|8/9 = 89 % (6 frames unknown)|8/9 = 89 %|0|14/15|0  |
|  people-detection.mp4|motion / accuracy preset|596|326|326|270|0|50|0|276|55 %|21.9|20.6|8/9 = 89 % (5 frames unknown)|8/9 = 89 %|0|14/15|0  |
|  people-detection.mp4|motion / speed preset|199|69|69|55|75|17|0|52|35 %|7.6|6.5|4/9 = 44 % (8 frames unknown)|8/9 = 89 %|0|14/15|0  |
|  person-bicycle-car-detection.mp4|full / every 2 / 512 px (old balanced)|324|324|324|0|0|0|0|0|100 %|18.3|17.6|3/9 = 33 % (0 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  person-bicycle-car-detection.mp4|full / every 1 / 640 px (old accuracy)|647|647|647|0|0|0|0|0|100 %|34.9|34.1|3/9 = 33 % (0 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  person-bicycle-car-detection.mp4|motion / balanced preset|324|268|268|34|22|27|0|241|83 %|14.3|12.9|3/9 = 33 % (2 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  person-bicycle-car-detection.mp4|motion / accuracy preset|647|631|631|16|0|54|0|577|98 %|36.8|34.8|3/9 = 33 % (0 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  person-bicycle-car-detection.mp4|motion / speed preset|216|138|138|41|37|18|0|120|64 %|8.7|7.4|3/9 = 33 % (6 frames unknown)|4/9 = 44 %|0|12/15|0  |
|  WIDE people-detection.mp4|full / every 2 / 512 px (old balanced)|298|298|298|0|0|0|0|0|100 %|14.3|13.7|2/9 = 22 % (0 frames unknown)|2/9 = 22 %|0|11/15|0  |
|  WIDE people-detection.mp4|full / every 1 / 640 px (old accuracy)|596|596|596|0|0|0|0|0|100 %|32.5|31.7|2/9 = 22 % (0 frames unknown)|2/9 = 22 %|0|11/15|0  |
|  WIDE people-detection.mp4|motion / balanced preset|298|110|115|147|41|25|85|0|11 %|11.3|8.7|0/9 = 0 % (9 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  WIDE people-detection.mp4|motion / accuracy preset|596|305|313|291|0|50|250|5|13 %|23.2|20.7|5/9 = 56 % (6 frames unknown)|5/9 = 56 %|0|11/15|0  |
|  WIDE people-detection.mp4|motion / speed preset|199|60|60|85|54|17|0|43|30 %|10.3|8.1|0/9 = 0 % (11 frames unknown)|3/9 = 33 %|0|11/15|0  |
|  WIDE people-detection.mp4|full / every 2 / 1280 px|298|298|298|0|0|0|0|0|100 %|30.5|28.8|2/9 = 22 % (0 frames unknown)|2/9 = 22 %|0|11/15|0  |
|  WIDE person-bicycle-car-detection.mp4|full / every 2 / 512 px (old balanced)|324|324|324|0|0|0|0|0|100 %|37.6|36.7|2/9 = 22 % (0 frames unknown)|2/9 = 22 %|0|11/15|0  |
|  WIDE person-bicycle-car-detection.mp4|full / every 1 / 640 px (old accuracy)|647|647|647|0|0|0|0|0|100 %|42.1|41.1|0/9 = 0 % (0 frames unknown)|0/9 = 0 %|0|9/15|0  |
|  WIDE person-bicycle-car-detection.mp4|motion / balanced preset|324|117|121|182|25|27|90|0|10 %|7.4|5.5|1/9 = 11 % (9 frames unknown)|2/9 = 22 %|0|10/15|0  |
|  WIDE person-bicycle-car-detection.mp4|motion / accuracy preset|647|377|404|270|0|54|323|0|12 %|21.5|19.2|0/9 = 0 % (6 frames unknown)|0/9 = 0 %|0|9/15|0  |
|  WIDE person-bicycle-car-detection.mp4|motion / speed preset|216|47|47|125|44|18|0|29|22 %|3.4|2.1|0/9 = 0 % (12 frames unknown)|2/9 = 22 %|0|10/15|0  |
|  WIDE person-bicycle-car-detection.mp4|full / every 2 / 1280 px|324|324|324|0|0|0|0|0|100 %|18.3|16.8|2/9 = 22 % (0 frames unknown)|2/9 = 22 %|0|10/15|0  |

## Reading it

* Counts are exact. On people-detection (native 768x432), motion/balanced runs the detector on 121 of 298 sampled frames (-59 %) with the same recall as full (8/9, 14/15 exact frames) once tracker/static frames are counted; "indexed only" recall is also 8/9 for accuracy preset but the balanced/speed presets index fewer frames so more labelled times have no observation within 100 ms (6 and 8 of 15) and are only answered by the tracker state.
* person-bicycle-car (native): traffic never stops, so only 10-20 % of frames are skipped (balanced 268/324 detector frames; accuracy 631/647). Recall equals full (3/9, the model itself misses small cars/bikes). Savings are small on busy scenes; this is expected.
* Crops never ran on native footage (rule: source long side <= 960 px runs the whole picture because a crop costs the same one model run and adds no detail). Crops ran only on the WIDE rows: balanced 85-90 crop frames, 11 % / 10 % of full-picture pixels, 115-121 model runs vs 298-324. Recall on WIDE: people accuracy preset 5/9 vs 2/9 for full (more accuracy, fewer runs); people balanced 3/9 vs 2/9; street wide balanced 2/9 vs 2/9, accuracy 0/9 vs 0/9 (model cannot see small cars either way).
* Recall loss vs full: none found on native footage when tracker/static frames count. The speed preset is the exception to watch: indexed-only recall drops to 4/9 (people) because it samples every 3rd frame with interval 3 (one detector frame per ~0.75 s); with tracked state it is 8/9.
* Model runs: each crop is one run. On crowded high-res frames more crops than a full pass would cost more, hence `max_regions` and the fallback to one full picture (crops covering >= 50 % of the picture).
