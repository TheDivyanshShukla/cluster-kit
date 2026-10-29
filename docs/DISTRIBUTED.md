# Distributed search design

Status: built behind `CW_SHARDS=1` (item 7 of docs/loop/STATE.md), branch `feature/distributed-search`. Default is OFF: with it off the single-store behaviour and the demo are unchanged. What was built, the API, the integration points and the limits are in the sections after "Known limits". Evidence: `docs/evidence/d/chaos.md`, `docs/evidence/d/latency.md`.

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

## What was built (version 1)

Code lives in new modules under `rust/src/watch/shard/` so merges with other work stay small:

| File | Role |
| --- | --- |
| `mod.rs` | `CW_SHARDS` switch, wire types (`ShardReq`, `Rows`, `NodeStatus`), `NodeClient` trait, node `Registry` (state ok/slow/down, media sets, persisted in `vision/data/shard_nodes.json`), per-run node report (`RunCtx`), `wrap_store`, `route` |
| `node.rs` | `LocalShard` (a SQLite file with the same schema and migrations as the head store, reused from `storage::sqlite`), the shard read API on every worker (tiny_http, Bearer secret, constant-time compare), `commit_and_ack`, `HttpClient` |
| `federated.rs` | `FederatedStore` (implements `domain::Store`): scatter-gather with a per-call deadline, k-way merge, vector snapshots, `POST /api/v1/federated/*` |
| `merge.rs` | k-way merge on (media, time, id), the opaque cursor (per-node positions) |
| `relay.rs` | head: commit receipt (`/shard/ack`), event relay (`/shard/events`), media proxy; node: the event pusher |
| `tests.rs` | federated == single-store test (match windows, counts, distinct tracks, Lua, DISTINCT; page sizes 2000 and 7), kill/partial/cursor/snapshot tests |
| `ui/nodes.js`, `ui/nodes.css`, `ui/nodes.html` | node status strip (`Watch.nodes.render(el, result.nodes)`), and a Computers page at `/nodes` |

### How it works

- **Switch.** `CW_SHARDS=1` on the head and on each worker that should hold a shard. With it off on a worker, that worker posts to `/api/v1/index` as before and the head keeps those rows in its own store (the federated store also reads the head catalog as a node named `head`, so a mixed cluster works).
- **Ingest.** A sharded worker commits each batch to `vision/data/shard.db` (override `CW_SHARD_DB`) first, then sends the head a small receipt (`POST /api/v1/shard/ack`): the head checks the lease (stale generation -> 409 `stale_lease`, same contract as `/api/v1/index`), saves the thumbnail JPEG (the head keeps thumbnails, nodes keep detections), advances the job checkpoint, sets `media.node_id`, and updates the live overlay. Coverage and status still travel through the existing stats route. The checkpoint never gets ahead of durable rows because the local commit comes first; a replayed batch is harmless (content-derived ids).
- **Discovery.** The worker's heartbeat carries `shard: {node_id, url, secret}`. The secret is a random bearer generated at node start and sent over the worker-authenticated heartbeat; the head presents it on shard calls. A user session can never call a shard endpoint, and the node compares in constant time. Port `CW_SHARD_PORT` (default 7712; if busy the next free port is used), advertised URL `CW_SHARD_ADVERTISE` or `http://<primary ip>:<port>`; node id `CW_NODE_ID` or the worker id.
- **Reads.** The query engine (`query::v1`) is unchanged and still reads through `&dyn Store`. `FederatedStore` sends each read to the nodes that can hold the requested recordings (planned from `media_ids` / `source_ids` and each node's media set; a node with an unknown media set is always asked), in parallel threads, each under `CW_SHARD_DEADLINE_MS` (default 4000), retries off. Rows are merged with a k-way merge on (media_id, media_time_us, id); equal keys from two nodes (the same frame indexed twice after a failover) are emitted once.
- **Cursor.** A federated cursor is hex JSON `{v: vector snapshot, p: per-node position, d: finished nodes, x: given-up nodes}`. Positions are the key of the last row consumed from each node, so pages are stable and a cursor survives a node restart.
- **Snapshots.** `FederatedStore::snapshot()` polls each node's commit sequence and fixes a vector (node -> sequence) for the run; the scalar returned (sum) is what the run stores in `QueryRun.snapshot`. Nodes hide rows above their sequence, so one run, and every later page of one traversal (the vector rides in the cursor), reads one consistent snapshot. The vector is appended to `vision/data/shard_snapshots.jsonl` and returned as `snapshot_vector`. (The head's own catalog node ignores snapshots.)
- **Partial results.** `POST /api/v1/queries` gains `nodes: [{id, state: ok|slow|down, rows, elapsed_ms, error}]`, `partial: true|false` and `snapshot_vector`; existing fields are unchanged. A partial run also gets the warning `N of M computers answered; results may be incomplete` in `run.warnings` (stored with the run). The same report is returned by `POST /api/v1/federated/{detections|summaries|relations}`. A node that fails during a run is given up for the rest of that run so the engine never waits twice.
- **Node state.** `ok` (heartbeat or status within 6 s), `slow` (6-15 s), `down` (over 15 s, equal to the lease time, or no secret yet after a head restart). A `down` node is not called at all.
- **Media proxy.** `GET /api/v1/media/{id}/(stream|frame|thumbnails|sprite/..|observations)` is forwarded to the owning node (`media.node_id`, else the registry's owner, else local file) with the Range header and a 30 s timeout; the node serves it with its own `/api/v1/media/*` handlers over its shard catalog (the node learns each recording's path from its camera). Node down -> HTTP 503 `{"error":{"code":"unavailable","message":"unavailable on its computer (<node>)"}}`. Local media is unchanged. The proxy buffers one answer (the existing handlers cap an answer at 16 MiB), it does not stream chunked.
- **Realtime.** The node evaluates the alert rules next to its data (`alerts::Engine`, rules from the head's `/api/work`) and writes `index` and `alert` events to a `shard_events` table in `shard.db` with a per-node sequence. A pusher thread sends them to `POST /api/v1/shard/events`; the head applies each sequence once, remembers the last applied sequence per node in `vision/data/shard_cursor.json`, republishes on the existing `/api/v1/events` SSE, and upserts alerts (deterministic alert ids, a later node update keeps a human decision made on the head). The node asks the head for its cursor at start and after errors, so a head restart, a node restart or a retry resumes without loss or duplicate. Human decisions stay on the head (`/api/v1/alerts/{id}/decision`).
- **Failure model.** A lost node's cameras fail over exactly as today (new lease generation, resume from the head-held checkpoint, tracker ids reset). Its older shard is unavailable until it returns and every run that needs it is `partial` (the registry remembers each node's media set across a head restart, so a missing node is reported, not silently skipped). When the node returns it is queried again with no re-sync. **Shard replication is not in version 1**: a down node means its history is unavailable, and a node with a destroyed disk loses that history (the head still has the catalog, jobs, reviews and alerts).

## API

Head (user routes, existing auth): `POST /api/v1/queries` (extended), `GET /api/v1/nodes` (RO), `POST /api/v1/federated/{detections|summaries|relations}` (RO), `GET /nodes` (page). Head (worker routes): `POST /api/v1/heartbeat` (now also announces `shard`), `POST /api/v1/shard/ack`, `POST /api/v1/shard/events`.

Node (`CW_SHARD_PORT`, bearer = the secret announced in the heartbeat): `POST /api/v1/shard/{detections,summaries,relations,observations,count}`, `GET /api/v1/shard/status`, `GET /api/v1/shard/media/{id}/{stream|frame|thumbnails|sprite/..|observations}`. Read-only. `GET /api/v1/shard/events` (SSE) from the original sketch was replaced by the push direction (node -> head) because nodes sit behind the head's authentication.

## Integration points (for the coordinator, to re-apply on top of other changes)

All edits are small and marked with a comment naming `shard`.

1. `rust/src/watch/mod.rs`: `#[allow(dead_code)] mod shard;`
2. `rust/src/watch/domain.rs`: `MediaAsset.node_id: Option<String>` with `#[serde(default)]`; add `node_id: None` to every `MediaAsset { .. }` literal (matches.rs, media_catalog.rs x2, api.rs x3, storage/legacy.rs, storage/tests.rs, query/v1/tests.rs, domain.rs test).
3. `rust/src/watch/storage/mod.rs`: `mod sqlite;` -> `pub(crate) mod sqlite;` (the shard store reuses it).
4. `rust/src/watch/head.rs` `run()`: after `storage::open`, `let store = crate::watch::shard::wrap_store(store, &data_path(""));`. Static files: add `/ui/nodes.js`, `/ui/nodes.css`, `/nodes` to the route table and `include_str!` for `nodes.js`, `nodes.css`, `nodes.html`.
5. `rust/src/watch/api.rs`: (a) first statement of `handle()` after `parts`: `if let Some(r) = crate::watch::shard::route(api, method, url, body, range) { return Some(r); }`; (b) `Beat` gets `#[serde(default)] shard: Option<shard::Announce>` and `heartbeat()` calls `shard::registry().announce(a)`; (c) `run_query()`: `let mut o = ...; let fed = shard::finish_run(&*api.store, &mut o.run, &o.matches);` and after building the JSON `shard::decorate(&mut out, fed.as_ref())`; (d) `Api.latest` -> `pub(crate)`, `save_thumbnail` -> `pub(crate)`.
6. `rust/src/watch/worker.rs`: (a) in `run()` after the head is found: `crate::watch::shard::node::start(&id, &base);` (b) where `/api/work` rules are stored: `crate::watch::shard::node::set_rules(&r);` (c) heartbeat body: add `"shard": crate::watch::shard::node::announce()`; (d) before building `legacy` in the camera loop: `shard::node::note_media(&batch.observation.media_id, &id, &resolved);` (e) top of `try_once`: `if let Some(p) = shard::node::commit_and_ack(batch, agent, base) { return match p { Ok => Ok(()), Stale(e) => Err(Posted::Stale(e)), Permanent(e) => Err(Posted::Permanent(e)), Unreachable(m) => Err(Posted::Unreachable(m)) }; }`.
7. `rust/src/watch/auth/mod.rs` POLICY: `("GET","/nodes",Public)`, `("POST","/api/v1/heartbeat",Worker)`, `("POST","/api/v1/shard/ack",Worker)`, `("POST","/api/v1/shard/events",Worker)`, `("GET","/api/v1/nodes",RO)`, `("POST","/api/v1/federated",RO)`. Note: `/api/v1/heartbeat` was not in the policy, so a worker's heartbeat got 403 before; if another branch adds it, keep one entry.
8. UI (`find.js`, `review.js` are owned elsewhere; do not edit here). One-line integration: include `<link rel="stylesheet" href="/ui/nodes.css"><script src="/ui/nodes.js"></script>`, add `<div id="nodes"></div>` under the search bar, and after `POST /api/v1/queries` call `Watch.nodes.render(document.getElementById('nodes'), result.nodes);`. It renders nothing when `nodes` is absent, shows "3/3 computers" with one chip per computer, and a banner "N of M computers answered; results may be incomplete" when a computer is down. The Review page can call the same function with `GET /api/v1/queries/{id}` once that response carries nodes (not yet; today only the run response does).

## Decisions taken while building

- The head's local worker is a separate OS process (spawned by `cw head`), so "the head as a node" is that worker process; the head process itself only reads its catalog (as node `head`). With `CW_SHARDS=1` the head's local worker also writes `vision/data/shard.db` and serves shards on 7712 unless `CW_SHARD_PORT` says otherwise.
- Nodes authenticate the head with a per-node secret (announced over the worker-authenticated heartbeat) rather than the worker's own token, because the head stores only hashes of worker tokens (and the head's local worker has an in-memory token).
- Relations carry no media or time of their own; a node looks the key up in its own `relations` row so the merge key stays (media, time, id).
- `FederatedStore::commit_batch` still writes to the head catalog, so non-sharded workers keep working.
- The thumbnail JPEG goes to the head with the receipt (about 10 KB per indexed frame) so review thumbnails and the live overlay need no node round trip; detections do not.
- Pooled HTTP agent per node (connection reuse); a first version opened a socket per call and exhausted ephemeral ports when a test hammered the head with thousands of queries per second.

## Limits (version 1, additional to the list above)

- No shard replication (see Failure model). A node whose `shard.db` is deleted restarts its event sequence at 1; delete `vision/data/shard_cursor.json` on the head for that node (or the head ignores its new events as already applied).
- The vector of a run is kept on the thread that started it and in `shard_snapshots.jsonl`; `GET /api/v1/queries/{id}` does not yet return `nodes`.
- Snapshot visibility is enforced on detections, summaries, relations and counts, not on `observations` (review drill-down) or on the head catalog node.
- SSE is a notification channel with an in-memory ring: events applied by the head just before a crash are not replayed to clients (the DB, including alerts, is the durable record).
- The alert engine state on a node is not persisted (the head persists its own in `alerts_state.json`); a node restart loses pending hold windows, as a head restart did before.
- Security: shard endpoints are plain HTTP like the rest of the cluster; the bearer secret travels in the heartbeat. Use a trusted LAN or a tunnel.
- Measured on one machine only. Real network latency, partitions and disk speed across physical computers were not measured (`docs/evidence/d/`).
