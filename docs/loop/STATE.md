# Watch build loop: state

Rule: no questions to the user. Decide with sensible defaults, record the decision here, keep going until every item is Done.
Each iteration: pick the first item not Done, run it (agent or direct), verify (build, test suite, real browser or two-process run), fix, commit on its branch, update this table.

Defaults chosen (no answer from the user yet): the demo cluster is a mix of machines. macOS on Apple silicon gets a prebuilt worker bundle from the head; every other OS builds from a source bundle served by the head (needs Rust). Workers join only on request (a command or a pairing code), never by being pushed to.

| # | Item | Branch | Status | Evidence |
| --- | --- | --- | --- | --- |
| 1 | Editor autocomplete (SQL + Lua) and Search wording fixes | feature/watch-vision-platform | Done: verified in a real browser (SQL keywords/tables/columns/values, Lua db./row fields/counts.), 1,278 of 1,291 suggestions accepted by the server (the rest intentionally flagged), rehearsal 15/15 | docs/evidence/e2/ |
| 2 | Cluster join: `cw join`, `/join` page, pairing, prebuilt/source bundle, models + config + footage served by the head, `cw deploy user@host` | feature/watch-vision-platform | Done on one machine (agent K): real join via install.sh (bundle + sha256), join with and without sign-in, remote footage, deploy dry-run. Not run: Linux/Windows installer, source build, real ssh, multi-PC | docs/evidence/k/join_enforced.md, docs/evidence/k/join-*.jpg |
| 3 | Cluster work split: archive segments across workers, no duplicates, failover per segment | same | Done (agent K): 2 workers, w2 without footage, identical observation/detection ids vs single run, w2 killed mid-run | docs/evidence/k/segments.md |
| 4 | Cluster benchmark: run, per-worker live progress, 1/2/3/4 PC results, speed-up and efficiency, honest same-machine warning | same | Done (agent K): API, panel, `scripts/cluster_bench.sh`; numbers are single-machine only | docs/evidence/k/bench_local.md, docs/evidence/k/cluster-*.jpg |
| 5 | Realtime view: per-camera lag and keeping-up, camera to worker map, capacity versus demand, phone camera helper | same | Done (agent K): panel, SSE, live rebalance, probe + add (MJPEG simulated phone) | docs/evidence/k/cluster-*.jpg |
| 6 | `docs/CLUSTER_DEMO.md` runbook for the multi-PC demo | same | Done (agent K) | docs/CLUSTER_DEMO.md |
| 7 | Distributed search: per-node shards, fan-out and merge, partial results, realtime events, media proxy | feature/distributed-search | Built behind `CW_SHARDS=1` (default off), pending merge: node shards, shard API, FederatedStore (deadline, partial flags, vector snapshot, cursors), media proxy, event relay, `ui/nodes.js`. Verified: unit equality test (1 store vs 3 shards, SQL + Lua), real-process chaos (head + 3 workers; kill -9 and SIGSTOP of a worker mid-query and mid-index, head restart) on one machine only; physical multi-PC not measured | docs/DISTRIBUTED.md, docs/evidence/d/chaos.md, docs/evidence/d/latency.md |
| 8 | Final pass: full test suite, acceptance, demo rehearsal, docs refresh, commit | each branch | Not started | |

Verification gates for "Done": `cargo test` green; the acceptance script has no FAIL; the real-browser rehearsal passes with no console errors; for cluster items a real run with a head and at least two worker processes (killing one mid-run); anything that needs several physical PCs for a number is labelled as measured on one machine.
