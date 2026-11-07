# Audit round 4: real workloads and lab heads (2026-11-06)

Method: real Python jobs through the SDK on a secured fleet of virtual PCs, with and without faults, then the same for lab heads. Everything ran on one Mac: 8 virtual PCs (4 agent processes x 2 virtual PCs, 1 thread each), a head, a standby, and for lab-head runs two lab heads. Every PASS compares an exact result (a digest of the whole output, a SHA hit count, or a counted total) against a run on one process. Numbers from one machine are labelled as such; the Mac was often busy with other test agents (load average 17 to 47 during some runs), so compare exactness and bytes, not seconds, unless the text says the machine was quiet.

Scripts and workloads:

| What | Where | How to run |
| --- | --- | --- |
| Five real jobs: `train` (CNN, `ck.train`), `sweep` (240 SVMs, `ck.sweep`), `etl` (5 M rows, `ck.shard`), `cv` (5,000 images, `$CK_OUT` PNGs), `text` (1 GB, `ck.shard`) | `examples/real/*.py` | `python3 examples/real/NAME.py local` or `cluster`; each prints one JSON line with a digest |
| Harness: fleet, local reference, cluster run, digest check, faults | `scripts/stress/workloads.py` | `CK_BIN=... STRESS_DIR=... WL_PY=python-with-numpy-torch... python3 -I scripts/stress/workloads.py run [--fault none\|agent9\|agentstop\|head9\|cancel\|multi] [wl ...]` |
| Lab-head experiments | `scripts/stress/hier_bench.py` | `hier_bench.py scale\|labkill\|rootstop\|rootkill\|switch\|auto` ([HIERARCHY.md](../HIERARCHY.md) section 6) |

## 1. Real workloads: local against cluster

Seconds, 8 virtual PCs on one Mac, no faults (cluster time includes upload, planning, chunking and the final merge):

| Workload | Local (1 process) | Cluster | Result |
| --- | --- | --- | --- |
| train | 33 s | 69 s on the first run; 33 s once the planner had learned the item cost | digests equal (accuracy 0.9945 local) |
| sweep | 61 s | 32 s | best configuration and top 5 identical |
| etl | 1.3 s | 13 s | exact to the cent |
| cv | 17 s | 32 s, then 15 s | feature hash and PNG hash identical, 5,000 PNGs stored |
| text | 39 s | 42 s | word counts and distinct-line count identical |

Reading it: eight virtual PCs share one Mac's cores, so no job can run faster than one process by more than its parallel share, and short jobs (etl, cv, text) pay for upload, planning and merge. Only sweep, which is compute-heavy and splits into 240 independent tasks, beats local here. The planner's behaviour (one run of train taking 69 s, then 33 s) is the self-optimising SDK learning from the first run. These numbers say the cluster path is correct and its overhead is bounded; they do not say how fast a real lab is.

### Faults

Each workload was run with each fault and compared with the local digest. All runs were exact.

| Fault | Result |
| --- | --- |
| `kill -9` an agent process mid-job | exact |
| `SIGSTOP` an agent (frozen PC) | exact |
| Cancel mid-job | PASS |
| Kill the head with a standby (`head9`) | exact after the SDK fix 2 below; before it the SDK died on the first refused connection |
| 5 users submitting at once (`multi`) | every job exact |

## 2. Lab heads (hier_bench.py, one Mac, virtual PCs)

| Experiment | Result |
| --- | --- |
| Root frozen (`SIGSTOP`) 30 s during a 90 s job | lab heads off: +31 s. Lab heads on: -0.01 s (labs hold one to two leases, 30 to 60 s of work) |
| `kill -9` of a lab head mid-job | +0.6 s, exact |
| `kill -9` of the root with a standby, labs follow it | exact, 11.9 s (about 10 s ideal) |
| Live mode switches (off, manual, auto, off) during a job | exact; PCs change owner within about 3 s |
| 600 virtual PCs, 0.1 s chunks | root CPU about 3 % with lab heads against 10 to 25 % without. No speed gain: one Mac cannot saturate a root at 600 PCs |

The lab-head runs of the real workloads (`etl`, `cv`, `sweep` on a quiet machine, 2 lab heads, 8 virtual PCs):

| Workload | Wall time lab heads off / on | Exact | Root blob egress off / on |
| --- | --- | --- | --- |
| etl | 5.7 s / 12.8 s | yes | 329 MB / 329 MB |
| cv | 35.1 s / 43.2 s | yes | 0 |
| sweep | 52.3 s / 52.7 s | yes | 0 |

Lab heads add a hop, so on one machine they cost a few seconds on short jobs and nothing on long ones. Killing lab head A at 10 s during etl and cv (machine loaded) gave exact digests as well. Under heavy load (load average 17 to 47) train took 79.6 s off and 179.9 s on, still exact; those are load, not lab-head, effects and are not used for any claim.

### The L2 shard fix

Items that held shard handles inside a tuple, a dict or a batch were pickles, so the head treated their blobs as job-wide and every PC fetched every shard. With 8 shard handles of a 329 MB file and 4 agent processes:

| | Agents fetched | Root sent |
| --- | --- | --- |
| Before | 1,317 MB (4x the file) | 1,152 MB |
| After | 329 MB (1x) | 329 MB |

The SDK now writes `@refs:<sha>,..|item` and the head places only those shards on the PC that gets the chunk. Items that were bare handles were already per chunk.

## 3. Bugs found and fixed

| # | Where | Bug | Fix |
| --- | --- | --- | --- |
| 1 | SDK (count) | Any run with results over 64 KB crashed: the SDK's byte count called `sum(map(len, ...))`, but `map` in that module is `ck.map` | a generator expression |
| 2 | SDK (`_http`) | The SDK died with "connection refused" when the head failed over mid-job, although the standby kept the job | GET and HEAD are retried for up to `CK_FAILOVER_SECS` (60 s) while a cluster run is in flight |
| 3 | SDK (blobs) | Blobs were not uploaded again after the SDK reconnected | re-upload after reconnect |
| 4 | SDK (profile window) | The first item was profiled under memory tracing together with its lazy imports, so item cost looked 10x too high and the plan was wrong | profile excludes import time; the examples import at the top of the file |
| 5 | SDK (planning) | With lab heads on the planner saw each lab head as one PC | `/api/v1/cluster` lists `lab_pcs`; the planner expands them (a first lab-head cv run took 77.8 s for this reason) |
| 6 | Head (blobs) | Virtual agents in one process shared one "fetching" set, so a second agent asking for the same blob returned early and never reported it; the head waited for it forever (train hung) | per-agent state. It showed up because virtual agents share a process; a real PC runs one agent per process |
| 7 | Head (speculation) | A speculative copy of a chunk could be handed to a PC that lacked the chunk's blobs, storing an error result | speculation only to PCs that hold the blobs; test added |
| 8 | SDK (warm runner) | The warm worker key ignored the runner version: an old warm worker served new `@refs:` lines | runner version is part of the key |
| 9 | Head (shards) | Per-item shard placement only recognised bare `@blob:` items (see the table above) | `@refs:` lines, `blobs::item_blobs` |
| 10 | Head (new) | A chunk waiting for blobs that never come could wait forever | `stage_timeout_s`: re-stage from the head, then reassign, then fail with a message ([HIERARCHY.md](../HIERARCHY.md), [OPERATIONS.md](../OPERATIONS.md) section 5d) |
| 11 | Head (candidates) | `/api/v1/ha/candidates` did not count PCs behind a lab head as connected | counted, test added |

## 4. Functional checks with lab heads on and off (identical)

`CK_OUT` files (20 of 20 with the right chunk ranges and contents); list results; a poison item (same message, "chunk 17..18 failed: exit 3: boom"); exit 75 retry; a selector that matches nothing (waits, then cancels); selector plus `max_pcs 3`; cancel mid-job.

## 5. Gate

On commit 6786be3: 236 Rust tests pass; `scripts/chaos.sh` 5 of 5; `robust.py` 120 of 120; `lifecycle.py` double failover, switch and flap PASS; SDK unit tests (56) and ruff clean.

## 6. What one Mac cannot show

- **Real network bandwidth.** Every transfer is loopback, so the benefit of fetching a blob once per lab (instead of once per PC across a switch uplink) shows as bytes counted, not as seconds saved. The bytes are right (329 MB instead of 1,317 MB); the seconds on a real network are not measured.
- **A real 200-PC lab.** The largest runs are 600 virtual PCs with synthetic tasks, which show root CPU but cannot saturate it, and 8 virtual PCs with real workloads. Per-PC memory, thermal limits, disk speed, Wi-Fi, people using the PCs, clock skew and real network splits are untested.
- **Speedups of real jobs.** With eight PCs on one machine, a cluster run cannot beat what the machine's cores allow; the cluster-against-local table above checks correctness and overhead only.
- **Python on every PC.** The workloads need numpy, pandas, scikit-learn, torch, OpenCV on each PC; the harness puts one environment first on the PATH of the virtual agents.

## 7. Known limits found or kept

- `--only` jobs still pause lab heads. A lab head has no standby. Leases are sized by the whole lab's speed, so a selector matching a few PCs of a big lab makes long leases.
- A lab head cuts leases with its own chunk size; the root's chunk setting is not forwarded.

## 8. Trips: frames and requests per flow, before and after (rt1)

Counted with the new `GET /api/v1/wire` (frames and bytes per kind, HTTP requests per route; always on, one relaxed add per frame) and `scripts/stress/trips.py`. Fleet: 1 head + 3 agent processes x 4 virtual PCs (open fleet); hierarchy: root + 2 lab heads, 2 x 4 virtual PCs. Before = commit defad7a, after = this work. Counts are frames the head(s) sent, per run; all results exact. Wall times are from a Mac under load 12 to 40 (other agents were building), so seconds that did not move by more than noise are shown as "same".

| Flow | Before | After | Wall |
| --- | --- | --- | --- |
| (a) chunk loop, flat, 589 small chunks | 1,784 frames out (ACK 592, TASK 595, PART 589); 3.0 per chunk | 1,207 (TASK2 563, PART 589, ACKS 9); 2.05 per chunk, -32 % | 1.77 s, same |
| (a) 283 bigger chunks | 867 frames; 3.06 per chunk | 587; 2.07 per chunk, -32 % | 1.08 s, same |
| (a) heartbeats while a job runs (60 chunks, 10 s, 12 PCs) | 120 out, 120 in | 100 out, 100 in | same |
| (b) `ck run`, one chunk | 5 frames out | 5 (ACK becomes ACKS) | same |
| (b) `ck run`, many chunks | one write per PART | PARTs queued for a client leave in one write | same |
| (c) root + 2 labs, 400-number job, all three heads | 1,593 frames out (ACK 765, TASK 737) | 947 (TASK2 731, ACK 46, ACKS 27), -41 % | 2.80 s to 2.78 s |
| (c) root + 2 labs, longer chunks | 1,086 | 661, -39 % | 1.71 s, same |
| (d) SDK run with 300 KB of shared blobs (first / repeat) | 48 / 8 HTTP requests, 88 / 84 frames | 19 / 9 HTTP, 52 / 41 frames | 10.0 / 9.1 s to 11.7 / 10.5 s (noise) |
| (e) `ck.map`, 400 items (first / same call again) | 23 / 16 HTTP requests | 7 / 5 | 5.1 / 3.8 s to 6.7 / 5.2 s (noise; remote Python start dominates) |
| (f) console first load | 1 page + 69 static files + 16 API GETs + 1 SSE; `ha/settings` and `select` fetched 3 times each; 823 KB of uncompressed JS/CSS | 1 page + 3 static files (one JS bundle, one CSS, one font) + 12 API GETs + 1 SSE; 247 KB on the wire (pre-compressed gzip) | commits 7ebccd1, 2278315 |

What changed:

- **Protocol v4.** An agent whose hello says version 4 or higher gets the ACK for a stored result folded into the next TASK (`TASK2`), or batched with other waiting ACKs into one `ACKS` frame when no TASK follows (the feeder flushes before it waits for work, a result that earns no credit flushes at once, and the heartbeat thread flushes within one heartbeat). v3 agents and lab heads keep the old `ACK` and `TASK`. A v4 agent talking to a v3 head never sees the new kinds. Mixed fleets work in both directions; no refusal is needed.
- **Heartbeats are skipped** when another frame left the same connection in the last half heartbeat, on head, agent and lab head. The longest silence a peer can see is 1.5 heartbeats.
- **`GET /api/v1/jobs/{id}?wait=S&result=1`** holds the request until the job finishes (12 held at once) and returns a small result inline. The SDK waits with it instead of polling every 0.03 to 0.3 s, skips the `/result` fetch and the `/stream` connection, and reads the asleep count from `/cluster` (`asleep`) instead of `/nodes`. It remembers a head that ignores `wait` and polls as before. The SDK also reuses the profile of the same function on the same first items for 10 minutes.
- **PART frames** for an attached client are written together when several are queued.

Already minimal, left alone (measured): `ck run` already gets parts in the submit stream with no second fetch; the agent already holds 2 chunks (`pipeline`), so every RESULT is also the next request and no chunk waits on the network. Batching small results would delay the refill, so it was not done. Not done: have-list in the hello (saves one CTRL per connection, not per chunk), lab status folded into the parts frame (one CTRL per second per lab), parallel `/estimate` calls.

Freeze tests for the folded ACK path (`scripts/stress/trips_freeze.py`, 12 PCs, ~20 s of work, bound 55 s): SIGSTOP one agent process for 9 s (23.9 s, exact), kill -9 one agent process with 2 chunks per PC prefetched (27.7 s, exact), SIGSTOP the head for 6 s (25.1 s, exact). Slow link through the netfault proxy: `mode_slow` 22.2 s and `mode_latency` 30.8 s, exact.

Gate on the final binary: 242 Rust tests, `chaos.sh` 5 of 5, `robust.py all` 120 of 120, `lifecycle.py double` PASS, `hier_bench.py standby` and `comeback` ALL PASSED, SDK tests 60 of 60 (3 new), ruff clean. One timing-based SDK test (`test_advise_hot_loop_and_gil`, thread scaling) fails now and then when the Mac is loaded: 2 of 5 suite runs on the unchanged code, 2 of 6 on the new code, and it passes alone.

## 9. Freeze hunt (fz1)

`scripts/stress/freeze.py` pauses (SIGSTOP for 2, 8 or 40 s, then SIGCONT) or kills (-9) every role at a random moment (fixed seed, printed): head, standby, lab head, lab-head standby, agent, `ck run` client and an SDK caller. It also covers two roles at once and a pause during a failover, across synth, cmd, list, SDK-with-blob and cancel jobs, on a plain fleet and on a lab-head fleet. A case passes when the job ends exact (or cleanly failed or cancelled) within 3x its no-fault time plus the fault time plus 30 s, with no leases, in-flight chunks or orphan processes left. Each fleet has 12 virtual PCs, and at most 2 fleets run at once.

The matrix ran 112 cases with seed 1 and part of seed 2 before a reboot, then 41 cases on the fixed build afterwards. All of the 41 pass.

| # | Freeze or bug | Cause | Fix |
| --- | --- | --- | --- |
| F1 | When the blob port was taken, the job stalled at 0 forever | `blobs::start_server` left the bound port at 0, so the PCs were told to fetch from `ip:0` | bind any free port and announce that one |
| F2 | Every `ck.shard` SDK job failed in about 11 s ("blob ... is missing or was corrupt") | `check_job` (added this round) demanded every blob of the job, but a PC holds only those its chunks use | a blob this process never verified counts as not needed here, not as lost |
| F3 | After a root failover, finished leases stayed on a lab head for good | a lease that finished while the uplink was down was never announced, and the root ignores the ACK of a lease it was never told about | resend `lease_done` until it is sent |
| F4 | A lab head that came back after its standby took over deadlocked the root | `h()` held across an `if let` body | fixed in defad7a; the hier_bench `comeback` case covers it |

Still open: with seed 2, two lab-head cases with an agent paused finished at 3-6x the no-fault time, including a 43 s window with no progress while 2 PCs sat idle. They recovered without help, so this is slow recovery, not a freeze; the cause is not yet known.
