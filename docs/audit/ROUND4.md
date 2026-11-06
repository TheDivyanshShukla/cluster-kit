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
