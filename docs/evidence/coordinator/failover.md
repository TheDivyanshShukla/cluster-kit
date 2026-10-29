# Two-worker failover on the lease scheduler (2026-10-29)

Script: scratchpad `failover.py` (isolated CW_ROOT, debug build, CW_THREADS=2). One head (`--no-local-worker`), workers `w1` and `w2` on one PC (`CW_WORKER_ID` distinguishes them), two archive sources (`vision/footage/*.mp4`).

| Step | Observed |
| --- | --- |
| Before kill | Both tasks leased to w1 at generation 1 (w1 polled before w2 registered); 158 observations |
| `kill -9 w1` | after the 15 s lease, `reap` requeued both tasks; w2 leased them at generation 2, attempt 2 |
| After ~2 min | w1 `alive=false`, w2 alive; 1006 observations, 1006 distinct IDs; 0 duplicate (media, time) rows |
| Checkpoints | advanced to 41.9 s / 41.8 s of ~50 s clips; tasks still `leased` (debug build is slow), so job completion was not observed in this run |

Not shown here: rebalancing an idle worker onto an already-leased task (not implemented; first poller takes the queue), and a release-build throughput figure.
