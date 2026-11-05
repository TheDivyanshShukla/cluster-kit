# Bulk distribution and network-aware estimates (wave 13)

User (2026-11-05): "auto optimization with bulk distribution, so it actually helps on the network; it calculates the
estimates." Requirement IDs: JOB-05 (bundles), HA-08 (bundle relays), NET-04 (network use), SCH-05 (placement).

## Problem today

- Code ships inside every job (`cmd` push, about 1-60 KB) and data ships inside every list item (4 KB cap).
- Everything leaves the head; nothing is cached on the PCs.
- The SDK estimate counts compute and fixed overheads but not bytes: a 2 GB dataset to 200 PCs, sent from the head
  over 1 Gbit/s, is about 53 minutes of transfer that the estimate ignores.

## Design

### 1. Blobs: content-addressed, cached everywhere

- **Head store:** `PUT /api/v1/blobs` (researcher) with a raw body returns `{"sha256", "size"}`, stored at
  `<CK_DATA>/blobs/<sha>`. A blob is never stored twice. `HEAD` / `GET /api/v1/blobs/{sha}` read it. Deletion is
  automatic: refcounted by jobs, LRU above `blob_store_gb`.
- **Agent cache:** blobs land in `<CK_DATA or ck-data>/cache/<sha>`, are verified by sha256 after download, and are
  evicted LRU above `blob_cache_gb`. A cached blob is never fetched again.
- **Inventory:** agents report which blobs they hold (a compact list of sha prefixes in a CTRL `{"have": [...]}` on
  connect and on change), so the head knows where every blob is.
- **Job references:** a job may name blobs in `JobSpec.blobs: [sha]` (code, shared data). A list item may hold
  `@blob:<sha>[:offset:len]` instead of inline data. The runner gets local file paths: `CK_BLOB_<i>` env vars and a
  `CK_BLOBS` directory.

### 2. Distribution strategies (the head picks one per blob per job; all are measured, none guessed)

- **direct:** every PC fetches from the head. Good for small blobs or few PCs.
- **peer:** a PC that has the blob serves it to peers on the same /24. Agents open a small TCP blob port,
  `CK_PORT+3`, serving cached blobs only, read-only, with sealed frames in a secured fleet and the node key for
  admission. The head tells each fetcher where to get it (a CTRL `{"fetch": {"sha", "from": [addr...]}}`).
  Fan-out is a tree: the head seeds k PCs per lab, then each holder serves up to `blob_peer_fanout` others at once.
- **pre-stage:** blobs a queued job needs are pushed to its likely PCs before the job reaches them, so compute starts
  with data already local.
- **chunked transfer:** 4 MB pieces with per-piece sha256, so a broken transfer resumes and a PC can serve the
  pieces it already has (BitTorrent-like within a lab).
- **locality-aware placement:** when chunks of a job read specific blobs (shards), `sched::place` prefers PCs that
  already hold them (a cache-hit bonus in the placement score).

### 3. Measuring the network (inputs to every estimate)

- **Per-PC link speed:** at join (after calibration) and every `net_probe_every_s`, the head sends the agent a probe
  blob of `net_probe_mb` and measures throughput, both directions. The result is stored per node as `net_mbps` and
  `rtt_ms` (from heartbeats). Probes are skipped while the PC is busy with a job.
- **Head uplink:** the sum seen during parallel fetches gives `head_uplink_mbps` (learned, with a tunable cap).
- **Lab speed:** the median peer-to-peer throughput inside each /24.

### 4. Estimates: one model, used by the head, the SDK and the console

`estimate(job) = transfer + compute + overheads`, for each strategy and PC count:

- **transfer (direct)** = bytes × PCs / min(head_uplink, Σ link speeds of the chosen PCs).
- **transfer (peer tree)** ≈ bytes / link × (1 + ⌈log_fanout(PCs per lab)⌉) + bytes × labs / head_uplink.
- **transfer (cached)** = 0 for PCs that already hold the blob.
- **compute** = total work / Σ PC rate (from calibration or the job's measured rate).
- **overheads** = submit + per-task dispatch × tasks + per-piece process start (the learned SDK constants).

The estimator picks the strategy and the PC count that minimise the finish time, and reports the alternatives.

- **API:** `GET /api/v1/estimate?bytes=&work_s=&items=&selector=&max_pcs=`. The reply adds `transfer_s` per
  strategy, `best: {"strategy", "pcs", "finish_s"}`, and `why`.
- **Console:** the New job form and job detail show it ("data 2.1 GB: peer tree, 41 s; direct would take 6.3 min").
- **SDK:** `ck.plan()` / `ck.advise()` include transfer.

### 5. SDK

- `h = ck.put(obj_or_path)` uploads once and returns a handle. Pass handles in items or args; the runner gives back
  the object (or the local path for a file).
- **Automatic:** any argument over `blob_auto_kb`, and a big shared closure (module-level data used by fn), becomes a
  blob automatically. The code itself is a blob too, so repeated calls and train rounds never re-send it.
- **Data sharding:** `ck.map(fn, ck.shard(path_or_list, parts=k))` makes k shard blobs. Placement then prefers
  PCs that already have each shard.
- **Bulk results:** results over `blob_auto_kb` come back as blobs fetched in parallel, not through the stdout
  pipe of the job.

### 6. Knobs (all in tune.rs, group "Network")

`blob_store_gb` (50), `blob_cache_gb` (20), `blob_peer_fanout` (4), `blob_piece_mb` (4), `net_probe_mb` (16),
`net_probe_every_s` (3600), `blob_auto_kb` (64), `head_uplink_mbps_cap` (0 = learned).

### 7. Acceptance (one machine with virtual agents, then the lab)

- **Unit tests:** the estimator formulas, strategy choice, chunked transfer resume, cache eviction, sha
  verification, peer-port refusal without a valid key, and placement preferring cached PCs.
- **Live:** a 200 MB blob to 20 agents on one machine (separate CK_DATA each).
  - Direct vs peer: wall time and bytes the head sent. Peer must cut head bytes by at least 4×.
  - A second job with the same blob: 0 bytes moved.
  - The estimate must land within 25 % of the measured time.
- **SDK:** `ck.map` over 50 shards of a 500 MB file: transfer happens once per shard per PC, and the plan shows the
  transfer estimate and the strategy.

## Decisions made while building (wave 13, x1)

- **Admission:** the head gives the fetcher a ticket (id + 32-byte key) and the serving PC the same ticket; the blob port needs it, in open and secured fleets alike (peers never hold each other's node keys). Frames are sealed with the ticket key. A ticket names one sha; the server never takes a path.
- **Per chunk:** a PC fetches the blobs of the chunk it was given (job-wide ones plus the `@blob:` shards in the chunk) and keeps the chunk reserved until they arrive; it prefers chunks whose shards it caches. Pre-staging happens as soon as a PC asks for work.
- **Peer tree:** per /24 the head seeds `blob_seeds` PCs; every holder serves up to `blob_peer_fanout` at once. Estimate rounds are simulated exactly (not a log formula). Model assumes a holder can serve its fan-out at its measured link speed; on weak NICs lower `blob_peer_fanout`.
- **Uplink:** learned as the best aggregate rate seen with 2 or more transfers (`POST /net/probe {"burst": true}` does it on demand).
- **Open:** partial pieces are not served to peers yet; blobs are not replicated to standby heads; head restart forgets which PCs hold what until they reconnect.
