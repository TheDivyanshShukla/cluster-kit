# Watch build loop: state

Rule: no questions to the user. Decide with sensible defaults, record the decision here, keep going until every item is Done.
Each iteration: pick the first item not Done, run it (agent or direct), verify (build, test suite, real browser or two-process run), fix, commit on its branch, update this table.

Defaults chosen (no answer from the user yet): the demo cluster is a mix of machines. macOS on Apple silicon gets a prebuilt worker bundle from the head; every other OS builds from a source bundle served by the head (needs Rust). Workers join only on request (a command or a pairing code), never by being pushed to.

| # | Item | Branch | Status | Evidence |
| --- | --- | --- | --- | --- |
| 1 | Editor autocomplete (SQL + Lua) and Search wording fixes | feature/watch-vision-platform | Done: verified in a real browser (SQL keywords/tables/columns/values, Lua db./row fields/counts.), 1,278 of 1,291 suggestions accepted by the server (the rest intentionally flagged), rehearsal 15/15 | docs/evidence/e2/ |
| 2 | Cluster join (running, agent K): `cw join`, `/join` page, pairing, prebuilt/source bundle, models + config + footage served by the head, `cw deploy user@host` | feature/watch-vision-platform | Not started | docs/evidence/k/ |
| 3 | Cluster work split: archive segments across workers, no duplicates, failover per segment | same | Not started | docs/evidence/k/ |
| 4 | Cluster benchmark: run, per-worker live progress, 1/2/3/4 PC results, speed-up and efficiency, honest same-machine warning | same | Not started | docs/evidence/k/ |
| 5 | Realtime view: per-camera lag and keeping-up, camera to worker map, capacity versus demand, phone camera helper (test URL, add as live source) | same | Not started | docs/evidence/k/ |
| 6 | `docs/CLUSTER_DEMO.md` runbook for the multi-PC demo | same | Not started | |
| 7 | Distributed search: per-node shards, fan-out and merge, partial results, realtime events, media proxy | feature/distributed-search | Not started | docs/DISTRIBUTED.md |
| 8 | Final pass: full test suite, acceptance, demo rehearsal, docs refresh, commit | each branch | Not started | |

Verification gates for "Done": `cargo test` green; the acceptance script has no FAIL; the real-browser rehearsal passes with no console errors; for cluster items a real run with a head and at least two worker processes (killing one mid-run); anything that needs several physical PCs for a number is labelled as measured on one machine.
