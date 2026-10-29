# Two real worker processes, one killed mid-run (Agent B)

Setup: `CW_ROOT=<tmpdir>` with a copy of `vision/` (both footage files as `archive` sources, `modules.json`, `yolov5n.onnx`), empty `vision/data`.
`cw head --no-local-worker --port 7795`, then two `cw worker --head 127.0.0.1 --port 7795` processes
(`CW_WORKER_ID=w1|w2` gives them distinct identities on one PC; w2 had `CW_THREADS=2`). Port 7790 was taken by another agent's head, which was left alone.
Logs and `/api/state` snapshots: `head.log`, `worker_w1.log`, `worker_w2.log`, `state_t*.json` (seconds after the kill; t0 is just before it).

## What happened

| time | `/api/state` cameras (id, indexed_frames, media position) | PCs listed |
| --- | --- | --- |
| t0 (before kill) | street 5 @14.7s, hall 5 @3.6s | 2 (work split 56% / 44%) |
| kill w1 | | |
| t5 | street 8 @15.3s, hall 13 @15.3s | 2 (still inside the head's 6 s liveness window) |
| t15 | street 4 @14.1s, hall 18 @16.9s | 1 |
| t30 | street 13 @16.0s, hall 27 @21.5s | 1 |

- After w1 died the head dropped it after about 6 s and `split::assign` gave its camera to w2. Both cameras then ran on w2 and kept indexing. No process was left behind (all started processes stopped).
- The reassigned camera (`street`) restarted from the beginning of the file (indexed count and position went down: 8 @15.3 s -> 4 @14.1 s). There is no checkpoint handoff yet.
- `POST /api/v1/index` on this head answers 404, so the worker fell back to the legacy `/api/index` body (as designed). No errors were raised.
- Consequence of restart plus the legacy route: `seen.jsonl` had 50 rows, 17 of which share (camera_id, seq, class) with an earlier row. The legacy dedupe key includes the batch timestamp, so a replay after failover is counted twice. This is the exact problem the v1 path fixes: with `/api/v1/index` the observation ID is derived from media position and model versions, so the replay is a duplicate (`BatchOutcome.duplicate`) and counts do not rise.

## What this run proves

- Two real worker processes pulled work from one head and a real worker loss was survived by the existing liveness/assignment logic, with live decoding and real ONNX inference.
- The new worker code (bounded queue, coverage counters, acknowledged archive posting, v1 -> legacy fallback, FFmpeg reaping) ran against a real head without breaking the old protocol.

## What it does not prove (needs the coordinator)

- The head does not use `scheduler::Scheduler` yet: no leases, generations, `stale_lease` rejection, retry budget, or checkpoint resume were exercised here. Those are covered only by unit tests (`scheduler::tests`).
- `/api/v1/index`, `/api/v1/heartbeat` are not wired, so the v1 batch was built but could not be accepted; duplicate prevention on failover is therefore unproven end to end.
- Throughput numbers from this run are not a speedup claim (both workers share one machine).
