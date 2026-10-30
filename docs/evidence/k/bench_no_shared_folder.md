# Benchmark with no shared folder (2026-10-30)

Command (one Mac, so the workers share its CPU):

```
NO_SHARED_FOLDER=1 CW=rust/target-c2/release/cw bash scripts/cluster_bench.sh -k 2 -s 60 -t 900 \
  vision/footage/virat/09162008flight1tape1_1.mp4 vision/footage/virat/09152008flight2tape3_1.mp4
```

Two aerial recordings (300 s each, 720x480), cut into 60 s segments. Every worker had **no footage folder at all** (`NO_SHARED_FOLDER=1`): each recording was streamed to it from the head over HTTP with range requests. Only the head had the files.

| Run | Workers | Time | Frames sent to the detector | Speed-up | Efficiency | Share of work |
| --- | --- | --- | --- | --- | --- | --- |
| 1 worker | 1 | 227.0 s | 8,425 | 1.00x | 100% | w1 100% |
| 2 workers | 2 | 203.8 s | 8,425 | 1.11x | 56% | w1 60%, w2 40% |

- Both runs analysed exactly the same 8,425 frames, so the second worker did real work (40% of it) and nothing was duplicated or lost.
- The second worker could not have read the recordings from disk. Its data folder had none, so the segments it processed were streamed from the head.
- **This is not a cluster result.** Both workers ran on one Mac and shared its CPU, so a 1.11x gain only means the first worker was not using the whole machine. The detector also runs on the Neural Engine, which is shared. A real speed-up needs the workers on separate PCs.
