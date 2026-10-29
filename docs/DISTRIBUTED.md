# Distributed search design

Status: design (item 7 of docs/loop/STATE.md). Not built yet. Branch for the work: `feature/distributed-search`.

## Goal

Search and live alerts that use every computer in the cluster, keep working when a computer drops out, and say plainly what they could and could not cover.

## What exists today

- Workers decode and run the detector. The head's scheduler gives them cameras and archive segments (leases, generations, retries, failover from a checkpoint).
- Every worker posts its observations to the head, which keeps the single SQLite database. Search, review, alerts and the audit log all run on the head.
- This is simple and correct. It does not scale search past one computer's disk and CPU, and the head is a single point of failure for results.

## Decisions

1. **Shard by recording.** A recording (media) belongs to exactly one node, the node that ingested it. Observations, detections, relations and frame summaries for that media live in that node's local SQLite file. Reason: every result shape the engine produces (review windows, track counts, frame summaries, Lua scripts) is computed per recording, so shards combine without double counting.
2. **The head keeps the catalog and the human state:** sources, media records (including `node_id`), models, jobs/leases, query runs, review matches, reviews, alerts, audit. These are small. Query runs and review matches stay on the head so the review page needs no changes.
3. **The head is the only coordinator.** No peer-to-peer queries. A node answers only to the head, authenticated with its worker token.
4. **Same Store trait.** A `FederatedStore` implements `domain::Store` on the head. Reads for observations/detections/summaries/relations fan out to the owning nodes (or to the local shard when the head is also a node); writes of catalog data stay local. The query engine (`query::v1`) does not change.
5. **Scatter-gather with limits.** Each node call has a deadline (default 4 s per page), a row budget and a snapshot id. The head merges in the existing key order (media, time, id) with a k-way merge, so cursors stay stable: a cursor encodes the per-node position.
6. **Partial results are explicit.** The query response lists `nodes: [{id, state: ok|slow|down, rows, elapsed_ms}]`; the warnings say "2 of 3 computers answered; results may be incomplete". Zero results with a missing node are never shown as "none found".
7. **Snapshots.** Each node reports its commit sequence; a query run records the vector (node -> sequence) and reads at or below it. Pages of one run read the same snapshot.
8. **Media proxy.** Video, frames, thumbnails and sprites are served by the owning node; the head proxies Range requests (`/api/v1/media/{id}/...` -> `node/api/v1/shard/media/{id}/...`). If the node is down the media is "unavailable on its computer" (clear message, not a failure of the whole page).
9. **Realtime.** Each node evaluates alert rules next to its data (it already holds the FrameSummary) and sends `index` and `alert` events to the head, which republishes on `/api/v1/events` with resume cursors. Alert ids are deterministic, so a retry or a failover replay does not duplicate. Human decisions stay on the head.
10. **Failure model.** A lost node: its cameras fail over as today (new generation, resume from checkpoint, tracker ids reset); its historical shard is unavailable until it returns (flagged partial). Replicating a shard to a second node is out of scope for version 1; the file format (one SQLite file per node, WAL) makes a later copy straightforward.
11. **Collections.** A source group is a list of source ids. The planner maps ids to nodes through the catalog and calls only those nodes.
12. **Security.** Shard endpoints (`/api/v1/shard/*`) accept only worker tokens (never sessions), are read-only except ingest, and are rate limited. Media paths are never exposed.

## Shard API (node side)

- `POST /api/v1/shard/detections` {filter, page, snapshot} -> rows, next, snapshot
- `POST /api/v1/shard/summaries`, `/relations`, `/observations`, `/count`
- `GET /api/v1/shard/status` -> {node_id, snapshot, media: [...], rows}
- `GET /api/v1/shard/media/{id}/stream|frame|thumbnails|sprite` (Range)
- `GET /api/v1/shard/events?cursor=` (SSE: index, alert)

## Build order

1. Node-local store: workers write to their own `shard.db`; heartbeat reports `snapshot` and `media_ids`. The head keeps writing its own recordings to its shard (the head is a node).
2. Shard read API and `FederatedStore` with k-way merge, deadlines, partial flags, vector snapshots.
3. Media proxy and node-aware `/api/v1/media/*`.
4. Realtime: node rule evaluation and event relay.
5. Chaos test: head + 3 workers, kill one mid-query and mid-index; assert flagged-partial results, no duplicates, cursors resume, failover completes.
6. UI: node status chips on Search and Review ("3/3 computers"), a clear partial-results banner.

## Verification gates

- Federated results equal single-store results for the same data (same match windows, same counts) when all nodes answer.
- Killing a node mid-query returns within the deadline with `partial: true` and the surviving nodes' rows.
- A cursor from a page remains valid after one node restarts.
- Event stream resumes without loss or duplication after a head restart.
- Numbers: query latency with 1 and 3 nodes on this machine, labelled single-machine.

## Known limits (version 1)

- No replication of shards; a down node means partial results.
- Counting distinct values across nodes sums per-media counts (tracks are per media, so this is exact for track counts; `COUNT(DISTINCT class)` style values are merged by set union).
- Lua scripts run on the head over merged iterators (slow for huge scans); pushing scripts to nodes is a later optimisation.
