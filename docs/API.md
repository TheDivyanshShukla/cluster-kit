# Head HTTP API v1

Served by `ck head` on the dashboard port (`CK_PORT` + 2, default 7702), together with the console. Contract between `rust/src/api.rs` and `console/src/lib/api/schema.ts`. Change both together.

## Conventions

- JSON in and out. Times are Unix milliseconds (`*_ms`). Big counts are plain numbers (all fit in 2^53).
- Errors: HTTP status plus `{"error": {"code": "...", "message": "..."}}`. Codes: `invalid_request` 400, `unauthorized` 401, `forbidden` 403, `not_found` 404, `conflict` 409, `too_many_requests` 429, `unavailable` 503.
- Pages: `?limit=` (default 50, max 500) and `?before=<id>`; the reply has `next` (the `before` value for the next page) or `null`.

## Authentication

- **Modes.** `CK_AUTH=off` gives open mode: everyone is an anonymous admin. Use it only for development on 127.0.0.1. Otherwise auth is required.
- **First run.** While no user exists, `POST /api/v1/bootstrap` creates the first admin. It is only accepted from 127.0.0.1.
- **Console.** The console uses a session cookie, `ck_session` (HttpOnly, SameSite=Strict), set by login or bootstrap.
- **CLI.** The CLI sends `Authorization: Bearer <token>`. Tokens come from `POST /api/v1/tokens` and are stored hashed.
- **Roles.** `viewer` < `researcher` < `operator` < `admin`. Each route below names the minimum role.

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/me` | none | | `{"user": {"name","role"} or null, "mode": "open" or "required", "needs_bootstrap": bool}` |
| POST `/api/v1/bootstrap` | none, 127.0.0.1 only, only while no users exist | `{"name","password"}` (password at least 12 characters) | `{"user"}` and the session cookie |
| POST `/api/v1/login` | none | `{"name","password"}` | `{"user"}` and the session cookie; 401 on a wrong name or password; 429 after 5 failures in 5 minutes |
| POST `/api/v1/logout` | none | | `{}` |
| POST `/api/v1/tokens` | viewer | | `{"token": "ckt_..."}`, shown once |
| GET `/api/v1/users` | admin | | `{"users": [{"name","role","created_ms"}]}` |
| POST `/api/v1/users` | admin | `{"name","password","role"}` | `{"user"}` |

## Cluster

| Method and path | Role | Reply |
| --- | --- | --- |
| GET `/api/v1/cluster` | viewer | `{"now_ms", "workers": [Worker], "job": JobRow or null, "asleep": n}`. `job` is the running job; `asleep` counts enrolled, active PCs that are not connected. |
| GET `/api/v1/wire` | viewer | `{"out": {KIND: n}, "in": {KIND: n}, "bytes_out", "bytes_in", "http": {"METHOD /route": n}}`: frames this process sent and received by kind since start (or since the last reset), and HTTP requests by route (numbers and long ids shown as `#`). Needs no flag. `POST /api/v1/wire/reset` zeroes it. `scripts/stress/trips.py` reads it. |
| GET `/api/v1/nodes` | viewer | `{"nodes": [NodeRow]}`: every PC ever seen, with `inventory` parsed as an object. A PC that works under a lab head has `via_lab` (the lab name) and `live`: `{"threads", "busy", "virtual", "host", "cpu" (percent), "mem_pct"}` as its lab head last reported it (about every second); both are null for a PC the root serves itself. |

Worker has the same fields as `/stats`: `id`, `addr`, `threads`, `chunks`, `numbers`, `secs`, `rate`, `busy`, `cpu`, `mem_used`, `mem_total`, `version`, `node_id`, `host`, `virtual`, `inv`, `presence`, `blocked`, `cap`, `energy_j` (CPU package joules since the agent started, from Linux RAPL; 0 when unknown or virtual).

## Jobs

JobRow fields:

| Field | Meaning |
| --- | --- |
| `id`, `name` | job number and label |
| `kind` | `sha`, `cmd` or `synth` |
| `from`, `end` | the range, as `[from, end)` |
| `chunk`, `total` | chunk size and total numbers |
| `done` | numbers completed so far |
| `state` | `queued`, `running`, `done`, `cancelled` or `failed` |
| `submitter` | who submitted it |
| `created_ms`, `started_ms`, `finished_ms` | timestamps; the last two are null until set |
| `error` | null, or the reason it failed |

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/jobs` | viewer | | `{"jobs": [JobRow], "next"}`, newest first |
| POST `/api/v1/jobs` | researcher | see below | `{"job": JobRow}` |
| GET `/api/v1/jobs/{id}` | viewer | | `{"job": JobRow, "parts": n}`. With `?wait=S` (0 to 10 s) the request is held until the job finishes or S seconds pass and the reply gains `"long": true`; with `&result=1` a finished job's reduced result (up to 1 MiB) comes along as `"result"` (text). At most 12 waits are held at once; beyond that the reply is immediate. Heads before round 4 ignore both and send no `long` field. |
| GET `/api/v1/jobs/{id}/result` | viewer | | `text/plain` result, described below |
| POST `/api/v1/jobs/{id}/cancel` | researcher for own jobs, operator for any | | `{"job": JobRow}`; 409 if the job already finished |

POST `/api/v1/jobs` body: `{"name"?: string, "kind": "sha"|"cmd"|"synth", "from"?: number = 1, "n": number, "chunk"?: number = 0, "sha"?: hex string up to 64 characters, "cmd"?: string, "synth_ns"?: number, "only"?: [addr]}`. Exactly the field for the chosen kind must be present. Submitting `cmd` jobs needs operator.

Result format by kind:

- `sha`: one `"<n> <sha256 hex>"` line per hit, sorted by n.
- `cmd`: each part's stdout, ordered by start.
- `synth`: the number of numbers counted.

## Fleet: nodes, labs, approvals (M1 wave 3)

NodeRow (GET `/api/v1/nodes`, `/api/v1/nodes/{id}`): `{node_id, host, addr, inventory (object), first_seen_ms, last_seen_ms, state, state_reason, state_by, lab_id, x, y, online (bool: connected now)}`.
States: `pending` (joined, waiting for approval, gets no tasks), `active`, `cordoned` (no new tasks), `draining` (finishes its tasks, then becomes `cordoned`), `maintenance`, `retired`.

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/nodes/{id}` | viewer | | `{"node"}` |
| POST `/api/v1/nodes/state` | operator | `{"node_ids": [..], "state", "reason"?}` | `{"nodes": [NodeRow]}`; also used to approve: state `active` on pending nodes |
| PUT `/api/v1/nodes/{id}/place` | operator | `{"lab_id", "x", "y"}` or `{"lab_id": null}` to unplace | `{"node"}`; 409 `seat taken` |
| GET `/api/v1/labs` | viewer | | `{"labs": [{id, name, w, h, created_ms}]}` |
| POST `/api/v1/labs` | operator | `{"name", "w", "h"}` (1..=64 each) | `{"lab"}` |
| PATCH `/api/v1/labs/{id}` | operator | any of `name`, `w`, `h` | `{"lab"}` |
| DELETE `/api/v1/labs/{id}` | operator | | `{}`; its nodes become unplaced |
| POST `/api/v1/join-codes` | operator | `{"uses"?: 1..=500 = 50, "ttl_min"?: 1..=10080 = 120, "auto_approve"?: false}` | `{"code": "XXXX-XXXX-XXXX", "expires_ms", "uses", "auto_approve"}`, shown once |

Agents join with `ck agent --code XXXX-XXXX-XXXX` (first time only; the PC then keeps its own key). With `CK_FLEET=open` on the head, any agent is admitted without a code.
The SSE stream also sends `node`: a NodeRow, whenever one changes (join, loss, state, placement).

## Organisation: selectors, labels, pools, caps, reserves (M2)

Selector language (rust/src/select.rs header): space-separated terms, all must hold. `key=value`, `key!=value`, `key>=N`, `key<=N`, `key>N`, `key<N`, `key` (present), `!key` (absent), `key=a,b` (any), `key=pre*` (prefix), quotes for spaces. Keys: node, host, addr, state, online, lab, x, y, pool, cores, threads, ram_gb, cpu, arch, os, class, virtual, plus free labels.

Jobs: POST `/api/v1/jobs` also takes `"selector"?: string` (validated; 400 with the parser's message) and `"priority"?: "urgent"|"high"|"normal"|"low"|"background"` (default normal). JobRow gains `selector` (string) and `priority` (0..4, 0 = urgent). Several jobs may run at once; GET `/api/v1/cluster` returns `"jobs": [JobRow]` (all running) in addition to `"job"` (the first, for old clients).

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/select?q=SELECTOR` | viewer | | `{"count", "node_ids": [..], "error": null or message}`: preview which PCs match |
| PUT `/api/v1/nodes/{id}/labels` | operator | `{"key": "value" or null, ...}` (null removes) | `{"node"}`; NodeRow gains `labels: {key: value}`, `pools: [name]`, `class` |
| GET `/api/v1/pools` | viewer | | `{"pools": [{name, members, note}]}` |
| PUT `/api/v1/pools/{name}` | operator | `{"members": [node_id], "note"?}` | `{"pool"}` |
| DELETE `/api/v1/pools/{name}` | operator | | `{}` |
| GET `/api/v1/caps` | viewer | | `{"caps": [{id, selector, max_threads, note}]}` |
| POST `/api/v1/caps` | operator | `{"selector", "max_threads", "note"?}` | `{"cap"}` |
| DELETE `/api/v1/caps/{id}` | operator | | `{}` |
| GET `/api/v1/reserves` | viewer | | `{"reserves": [{id, selector, keep_n, keep_pct, note, held: [node_id]}]}`; `held` = PCs kept free right now |
| POST `/api/v1/reserves` | operator | `{"selector", "keep_n"?, "keep_pct"?, "note"?}` | `{"reserve"}` |
| DELETE `/api/v1/reserves/{id}` | operator | | `{}` |
| GET `/api/v1/config` | admin | | the whole setup as JSON: `{"version": 1, labs, placements, labels, pools, caps, reserves}` (ORG-11) |
| POST `/api/v1/config?dry_run=1` | admin | that JSON | `{"changes": [human-readable lines]}`; without dry_run it applies them |

## Smart scheduling, time rules, list jobs (M3)

POST `/api/v1/jobs` also takes:
- `"policy"?: "adaptive" | "guided" | "fixed"` (default adaptive; "fixed" reproduces the paper's engine);
- `"reducer"?: "" | "concat" | "lines" | "sum" | "count" | "sort" | "top:N"` (how GET result combines parts);
- `"kind": "list"` with `"items": [string, ...]` (1..=1,000,000 items, each <= 4 KiB) and `"cmd"`: each task runs the command with its items on stdin, one per line (operator role, like cmd). `from`/`n` are ignored for list jobs.

JobRow gains `policy`, `reducer`, `eta_ms` (estimated finish time, null when unknown or not running); `kind` may be `list`.
GET `/api/v1/jobs/{id}/result?reducer=sum` reduces on the fly with another reducer.
NodeRow gains `calib_mhs` (benchmark at join) and `presence` (`"busy"` while someone uses the PC, else `"idle"`).
Worker (in /cluster and /stats) gains `presence`, `blocked` (a window or reservation forbids work now), `cap` (effective thread cap).

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/windows` | viewer | | `{"windows": [{id, selector, days, start, end, action, cap_threads, note, active_now}]}` |
| POST `/api/v1/windows` | operator | `{"selector", "days": "mon-fri", "start": "09:00", "end": "17:00", "action": "block"|"cap", "cap_threads"?, "note"?}` | `{"window"}` |
| DELETE `/api/v1/windows/{id}` | operator | | `{}` |
| GET `/api/v1/reservations` | viewer | | `{"reservations": [{id, selector, start_ms, end_ms, note, active_now}]}` (current and future) |
| POST `/api/v1/reservations` | operator | `{"selector", "start_ms", "end_ms", "note"?}` | `{"reservation"}` |
| DELETE `/api/v1/reservations/{id}` | operator | | `{}` |
| GET `/api/v1/estimate?n=&kind=&selector=` | viewer | | `{"matching_pcs", "fleet_rate", "eta_secs" or null}`: how long a job of n numbers would take now (SCH-03) |

Times of day use the head's `CK_TZ_OFFSET_MIN` (default 330, India).

## High availability (M4)

| Method and path | Role | Reply |
| --- | --- | --- |
| GET `/api/v1/ha` | viewer | `{"role": "primary"|"standby"|"candidate", "epoch", "last_seq", "durable_seq", "standbys": [{"node_id", "acked_seq", "lag"}], "membership", "connected", "dispatching"}` |
| POST `/api/v1/ha/switchover` | admin | `{"to": STANDBY_NODE_ID}` -> 202 `{"to": addr}`; 409 if it is not a connected standby or does not confirm within 5 s. HA-07 planned switchover: this head stops dispatching, waits until the standby applied a "promote" entry, then re-runs itself as that head's standby; agents and `ck run` follow the higher epoch. CLI: `ck admin switchover NODE` |

The SSE stream also sends `ha` (the same JSON) when the role, epoch or the standby set changes. A standby head serves only `/api/v1/ha`, `/api/v1/me`, login and the console (read-only "standby" banner); everything else answers 503 `{"error": {"code": "unavailable", "message": "this head is a standby; the primary is <addr>"}}`.

## Events and audit

| Method and path | Role | Reply |
| --- | --- | --- |
| GET `/api/v1/events` | viewer | `{"events": [EventRow], "next"}`, newest first; EventRow = `{"id","ts_ms","kind","actor","detail"}` |
| GET `/api/v1/stream` | viewer | Server-Sent Events, described below |

`/api/v1/stream` sends three event types:

- `cluster`: the GET `/api/v1/cluster` JSON, every second.
- `job`: a JobRow, whenever a job changes.
- `event`: an EventRow, whenever one is logged.

A `: ping` comment line goes out every 15 s.

## Legacy

GET `/stats` (and `/stats?detail=1`) stays unchanged for the old dashboard (`/legacy`) and existing scripts.

## Power and dynamic heads (wave 7: NET-06, HA-10)

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| POST `/api/v1/nodes/power` | operator | `{"node_ids": [..], "action": "sleep"\|"wake"}` | `{"nodes": [NodeRow]}`. sleep: CTRL `{"power":"sleep"}` to each connected PC, which is then remembered as asleep. wake: Wake-on-LAN magic packets for each PC's MACs (inventory `macs`), sent by the head and by one connected agent on the same /24 (CTRL `{"wol": [...]}`), state "waking" until it reconnects. 409 for a PC with no known MAC (wake) or not connected (sleep). |
| GET `/api/v1/power` | viewer | | `{"power": PowerSettings, "asleep": [node_id]}` |
| PUT `/api/v1/power` | operator | PowerSettings | same as GET. mode `off` or `eco`; idle_min 1-1440; keep_awake 0-10000 |
| GET `/api/v1/ha/settings` | viewer | | `{"settings": HaSettings}` |
| PUT `/api/v1/ha/settings` | admin | HaSettings | same as GET. target_standbys 0-5; selector parses (ORG-04) |

PowerSettings `{"mode": "off"|"eco", "idle_min": 15, "keep_awake": 4, "wake_on_demand": true}`; HaSettings `{"target_standbys": 2, "selector": "!virtual"}` (rust/src/jobs.rs). NodeRow gains `power`: `awake` | `asleep` | `waking`. Events: `power.sleep`, `power.wake`, `ha.recruit`. Eco mode never sleeps the head PC, a standby, a PC with work in flight, or below keep_awake awake PCs; with wake_on_demand, queued work that has no free PC wakes asleep PCs that match its selector. Dynamic heads: every 10 s the primary checks its standby count and, if short, sends one matching agent (not virtual, not already a standby, a lab without a standby first) CTRL `{"standby_of": "<head ip:port>"}`; that agent starts `ck head --standby` beside itself.

## Every value adjustable, and head choice (wave 9)

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/tunables` | viewer | | `{"tunables": [Tunable]}` in registry order (rust/src/tune.rs `KNOBS`) |
| PUT `/api/v1/tunables` | admin | `{"key": number or null, ...}` (null = back to default) | same as GET. All or nothing: 400 names the first bad key or range. Stored in meta "tune" (replicated); agent-scope values go to every agent at once (CTRL `{"tune": {...}}`). Event `tune.changed` with the old and new values |
| GET `/api/v1/ha/candidates` | viewer | | `{"candidates": [Candidate]}`, best first |

Tunable = `{"key", "group", "value", "default", "min", "max", "unit", "scope": "head"|"agent"|"both", "help", "source": "default"|"db"|"env"|"head"}`. A value set by env `CK_TUNE_<KEY>` wins over the database and shows `source: "env"` (the console shows it read-only).

Candidate = `{"node_id", "host", "lab": name or null, "score": 0-100, "parts": {"speed", "memory", "stability", "free"} (each 0-1), "role": "head"|"standby"|"candidate", "eligible": bool, "why": "" or the reason it cannot be a standby (virtual, not connected, not active, no selector match)}`. Score = `sched::head_score` with the weights head_w_*; inputs: calibration MH/s, cores, RAM, `node.lost` events in the last 24 h, presence. HA-10 recruiting takes the best eligible candidate in a lab without a standby (any lab if all have one); the planned switchover target in the console defaults to the best standby.

## Research features (wave 12): time breakdown, alerts, experiments, daily report

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| GET `/api/v1/jobs/{id}/breakdown` | viewer | | `{"breakdown": Breakdown}` |
| GET `/api/v1/alerts` | viewer | `?active=1` | `{"alerts": [Alert]}` newest first |
| POST `/api/v1/alerts/{id}/ack` | operator | | `{"alert": Alert}` |
| POST `/api/v1/experiments` | researcher | `{"name", "raw_csv": text, "runs_jsonl": text, "sim": optional JSON}` (the files scripts/experiment.py writes; max 16 MB) | `{"experiment": ExperimentRow}` |
| GET `/api/v1/experiments` | viewer | | `{"experiments": [ExperimentRow]}` (no file bodies) |
| GET `/api/v1/experiments/{id}` | viewer | | `{"experiment": ExperimentRow, "raw_csv", "runs_jsonl", "sim"}` |
| GET `/api/v1/report?day=YYYY-MM-DD` | viewer | | `{"report": DayReport}` (local day, CK_TZ_OFFSET_MIN) |

Breakdown = `{"job", "wall_s", "pc_seconds": {"compute", "dispatch_wait", "tail_idle", "recompute", "failover_pause", "other"}, "per_pc": [{"node_id", "host", "chunks", "numbers", "rate", "busy_s"}], "chunk_s": {"p50", "p90", "max"}, "stragglers": [node_id], "efficiency": 0-1}`. pc_seconds splits (number of PCs that held the job) x wall time: compute = the sum of chunk compute times; dispatch_wait = time a PC was idle with no chunk while the job still had unassigned work; tail_idle = idle while only in-flight work remained; recompute = compute spent on chunks later discarded (lost PC, losing speculative copy); failover_pause = wall time when no head dispatched, x PCs; other = the remainder (never negative).

Alert = `{"id", "kind": "slow_pc"|"flaky_pc"|"head_hot"|"standby_behind"|"reserve_at_risk"|"no_standby", "severity": "info"|"warn"|"bad", "node_id" or null, "detail", "since_ms", "last_ms", "active": bool, "acked_by" or null}`. Rules (thresholds are tunables, group "Alerts"):
- slow_pc: rate below fleet median − alert_slow_mad x MAD for alert_slow_min_s;
- flaky_pc: more than alert_flaky_losses node.lost in 24 h;
- head_hot: head CPU above alert_head_cpu_pct for 60 s;
- standby_behind: lag above alert_standby_lag changes for 30 s;
- reserve_at_risk: a reserve holds fewer PCs than asked;
- no_standby: fewer standbys than target for alert_no_standby_s.

An alert opens (event `alert.open`), stays active while the condition holds, closes (event `alert.close`) after it clears for 60 s, and is never duplicated while active. SSE event `alert` sends an Alert whenever one changes.

ExperimentRow = `{"id", "name", "submitter", "created_ms", "configs", "repeats", "virtual": bool, "commit", "pcs_max"}` (parsed from the uploaded files).

DayReport = `{"day", "throughput_peak", "throughput_mean", "work_done": {"numbers", "items", "jobs_done", "jobs_failed"}, "pcs": {"seen", "max_online", "mean_online"}, "failures": {"pcs_lost", "recovered_chunks", "head_failovers"}, "energy_kwh" or null, "busy_share", "top_jobs": [{"id", "name", "pc_hours"}], "alerts": n}`, built from the store (jobs, parts, events) and per-minute samples the head keeps for 48 h (new table `samples`: ts_ms, online, busy, rate, energy_j).

## Bulk distribution (wave 13)

Design and acceptance: docs/BULK.md. Blobs are content-addressed byte strings (sha256, lower-case hex), stored once on the head and cached on the PCs.

| Method and path | Role | Body | Reply |
| --- | --- | --- | --- |
| PUT `/api/v1/blobs` | researcher | raw bytes (limit tunable `blob_max_mb`, default 4096; streamed to disk, not held in memory) | `{"sha256", "size", "existed": bool}`. Same bytes twice = one blob. |
| GET `/api/v1/blobs` | viewer | | `{"blobs": [{"sha256", "size", "refs", "holders", "created_ms", "last_used_ms"}], "store_bytes", "limit_bytes"}` newest first. `holders` = connected PCs that cache it. |
| HEAD / GET `/api/v1/blobs/{sha}` | researcher | | 200 with `Content-Length` (GET: raw bytes) or 404 |
| GET `/api/v1/net` | viewer | | `{"head_uplink_mbps", "nodes": [{"node_id", "host", "net_mbps" or null, "rtt_ms" or null, "probed_ms" or null, "lab"}], "labs": [{"lab" (the /24, e.g. "10.0.3"), "pcs", "mbps" (median)}]}` |
| POST `/api/v1/net/probe` | operator | `{"node_ids": [..]}` (empty = all connected idle PCs) | `{"queued": n}`; results appear in GET `/net` within seconds |
| GET `/api/v1/estimate` | viewer | `?n=` (old, numbers) or new form `?bytes=&work_s=&items=&selector=&max_pcs=&sha=` | old reply unchanged. New reply below. |

Submitting: `JobSpec` (POST `/api/v1/jobs`, SUBMIT_JSON) gains `"blobs": ["<sha>", ..]` (all must exist on the head, else 400) and `"blob_mode": "auto"|"direct"|"peer"` (default auto). Fetching is per chunk, not per job: a PC that takes a chunk first fetches only the job-wide blobs (those named in `blobs` but not by an item) plus the shards named by items inside that chunk: an item `@blob:<sha>[:off:len]` (the head turns it into a path) or `@refs:<sha>,<sha>|<payload>` (the payload is opaque to the head; the SDK uses it for items that hold handles inside a pickle, so tuples, `ck.train` rounds and batches are placed per chunk too). The chunk is held for that PC (no other PC gets it) until they are cached; after `stage_timeout_s` (120 s plus 1 s per 2 MB missing) the fetch is repeated from the head alone, after the same time again the chunk goes to another PC and this one is kept off the job for a while (`blob.restage`, `blob.stage_timeout` events); a blob that is gone from the head fails the job with a message saying so. When a PC picks its next chunk the head prefers one whose blobs it already caches, and `max_pcs` / `placement` are respected (only PCs allowed to take a chunk fetch anything). The job JSON shows `blobs` and `blob_mode`. Blobs are refcounted by jobs (the store never evicts a blob a queued or running job names; above `blob_store_gb` the least recently used unreferenced ones go).

Runner contract (cmd, list and push jobs, on the PC): env `CK_BLOBS` = the cache directory (files named `<sha>`, read-only use), `CK_BLOB_<i>` = full path of `blobs[i]` (i from 0, in the order of the JobSpec). In list jobs an item that is exactly `@blob:<sha>` or `@blob:<sha>:<offset>:<len>` (decimal bytes) is replaced before the runner sees it by `<path>` or `<path>\t<offset>\t<len>`; the head adds those shas to the job's blobs automatically. A job whose items name a sha the head does not have is rejected (400).

New-form estimate reply: `{"pcs", "matching_pcs", "bytes", "cached_pcs", "work_s", "compute_s", "overhead_s", "transfer_s": {"direct", "peer"}, "finish_s": {"direct", "peer"}, "best": {"strategy": "direct"|"peer", "pcs", "finish_s"}, "alternatives": [{"strategy", "pcs", "transfer_s", "finish_s"}], "why", "head_uplink_mbps", "assumed": [names of inputs that were guesses, e.g. "link_speed"]}`. `work_s` = total compute seconds on an average PC (1.0 relative speed); `compute_s = work_s / sum(relative speed of the chosen PCs)`. `sha=` marks PCs that already cache that blob (transfer 0 for them). CLI: `ck admin estimate` unchanged; `ck admin net`, `ck blob put FILE`, `ck blob ls`, `ck run --blob FILE` (repeatable; uploads then lists it in `blobs`).

Knobs (tunables, group "Network"): blob_store_gb 50, blob_cache_gb 20, blob_peer_fanout 4, blob_piece_mb 4, net_probe_mb 16, net_probe_every_s 3600, blob_auto_kb 64, head_uplink_mbps_cap 0 (0 = learned), blob_max_mb 4096. Agent env: `CK_BLOB_PORT` (agent and head blob port; default `CK_PORT+3`), `CK_NET_LIMIT_MBPS` (test aid: per-connection send rate cap on the blob port, both head and agent), `CK_DATA` (agent cache is `<CK_DATA>/cache`). Events: `blob.put`, `blob.fetch` (log: bytes, source, ms), `net.probe`.

Result upload capability (for bulk results, never the user token): for every cmd and list job the head sends each PC, with its first chunk of the job, a job-scoped token. The runner sees env `CK_BLOB_PUT_URL` (`http://<head>:<dashboard port>/api/v1/blobs`) and `CK_BLOB_PUT_TOKEN`. Use `PUT $CK_BLOB_PUT_URL` with header `Authorization: Bearer $CK_BLOB_PUT_TOKEN` and the raw bytes; the reply is the normal `{"sha256", "size", "existed"}`. The token is `cbp.<job>.<hmac>`: it is accepted only for `PUT /api/v1/blobs`, only for that job, only while the job is queued or running (it dies when the job ends), and is invalid after a head restart (a resumed job gets a fresh one with its next chunk). It is not stored in the job args, the database or the logs. The uploaded blob is then fetched with `GET /api/v1/blobs/{sha}` by the submitter.

`POST /api/v1/net/probe` body `{"node_ids": [..], "burst": bool}`: without `burst` every listed PC (all if empty) is re-measured, two at a time, and its own link speed is stored; with `burst` all PCs transfer at once only so the head learns its uplink (per-PC speeds are not stored). The uplink is also learned from every parallel blob transfer (the best aggregate rate seen). Extra test env: `CK_NET_NIC_MBPS` (shared send rate of one process, like a PC's network card), `CK_NODE_ID` (distinct identity for several agents on one machine), `CK_BLOB_MODE` for `ck run --blob`. Extra knobs: `blob_seeds` (2: PCs per /24 the head seeds first), `blob_max_mb`.

## Wave 14: head identity, power, SSE changed/log, simulation, fair share, preemption

- `GET /api/v1/ha` adds `"head": {"node_id", "addr", "since_ms"}` (primary now; a standby reports the primary it follows, `node_id` null; `since_ms` = epoch start as this process saw it) and `"mixed": [text]` (peers on older builds without signed beacons; empty when none).
- Node JSON keeps `power` as `awake|asleep|waking` and adds `power_since_ms` (epoch ms the PC entered asleep/waking/awake; null if this head has not seen it change). The console accepts this or `{state, since_ms}`.
- `GET /api/v1/power` adds `"labs": [{"lab", "watts", "awake", "asleep", "waking"}]`. Watts use the day-mode model (sim.rs `W_*`: idle 25, busy 65, sleep 2, boot 65, someone at the PC 45); an awake PC that is not connected is not counted.
- SSE `changed` `{"what": "reserves"|"bookings"|"timetable"|"candidates"|"pools"|"caps"|"labels"|"windows"}` after each successful write of that list (reservations -> bookings + timetable, windows -> windows + timetable, PUT ha/settings -> candidates); `candidates` also when the ranking changes (checked every 5 s, shared by all viewers).
- SSE `log` `{"lines": [log JSON]}`: batched every 500 ms, at most 200 lines (the newest), only while a viewer is connected (one shared reader of the log files). Secrets are already hashed by the logger.
- `POST /api/v1/sim` (researcher): body = the `ck sim` scenario JSON; reply `{"summary": {...}, "series": "<timeline.csv text>"}`. 400 if nodes > 20,000, hours > 48 or a day run is too large (> 3e8 PC-ticks); 409 `busy` while another run is going; runs on its own thread.
- Fair share (SCH-04): at equal priority PCs split by user first (equal by default), then by job; PCs rebalance at chunk boundaries (a PC moves only when that makes the split strictly more even). Users are the job submitter.
- Preemption (SCH-09): a strictly higher priority job takes PCs at their next chunk boundary (nothing is killed). POST /jobs `"preempt": true` also takes back the outstanding chunks of lower-priority jobs at once (CANCEL_RANGE, requeued; a result that still arrives is stored once). Not kept across a head restart.

## Wave 15: templates, output files, identify, user quotas
- Templates (JOB-12). `GET /api/v1/templates` -> `{"templates": [{"name","spec","note","owner","created_ms"}]}` (viewer). `POST /api/v1/templates` `{"name","spec","note"}` (researcher; `spec` is a POST /jobs body and is validated as the caller; the same name replaces, only its owner or an admin may; a researcher cannot save a `cmd` or `list` spec). `DELETE /api/v1/templates/{name}` (owner or admin). `POST /api/v1/jobs {"template": "name", ...}` lays the other body keys over the stored spec; the result is checked as the submitter (a researcher still cannot run a `cmd` job). SSE `changed` what `templates`. CLI: `ck job template ls|save NAME FILE.json|rm NAME`, `ck job submit --template NAME`. Templates live in the head database (migration 9); a standby has them after a snapshot, not by live replication.
- Output files (JOB-08). Each cmd or list chunk gets an empty directory in env `CK_OUT`. After exit 0 the agent uploads every regular file at the top of it (names 1 to 200 characters, no leading dot; at most 1000 files, together at most the `out_files_mb` tunable, default 256; more fails the chunk) as blobs, with the job-scoped upload token, and reports them with the result. Pieces of one chunk share the directory: name files by `$CK_START` so they do not collide (a later one replaces an earlier one). `GET /api/v1/jobs/{id}/files` -> `{"files": [{"chunk": [a, b], "name", "sha", "size"}]}` (viewer); download = `GET /api/v1/blobs/{sha}` (researcher). Job JSON gains `"files": N`. Blobs named by a job file are never LRU-evicted. CLI: `ck job files ID [--get DIR]` (writes DIR/A-B/NAME).
- Identify (FLT-07). `POST /api/v1/nodes/identify` `{"nodes": [id], "secs": 30}` (operator, secs 1 to 600) -> `{"identified": [...], "offline": [...], "secs"}`; 404 for an unknown node. CTRL `{"identify": secs}`. The agent logs it, rings the terminal bell, and on Linux writes a banner to /dev/tty1 when writable (no `say` on macOS). Node JSON gains `"identify_until_ms"` (null when off) and the node is published on SSE. CLI: `ck admin identify NODE... [SECS]`.
- Quotas (SCH-04). User JSON gains `"quota": {"max_pcs": 0, "max_running": 0}` (0 = no limit). `PUT /api/v1/users/{name}/quota` (admin; a missing key keeps its value; both 0 removes the quota; stored in meta `quotas`, replicated). A user's running jobs together never hold chunks on more than `max_pcs` PCs (a PC already working for the user may keep going, no new PC joins; speculative copies obey it too); the user's jobs beyond the oldest `max_running` take no chunks and show state `queued`. Job JSON gains `"wait_reason"`: null | `quota` | `priority` | `no_pcs` (a running job that holds no PC now). CLI: `ck admin quota USER MAX_PCS MAX_RUNNING`.
- `POST /api/v1/sim` is in the role table (researcher) and the CLI map (`ck sim` runs the same scenario locally).
- New tunables: `out_files_mb`, `blob_partial_ms` (500), `blob_stall_s` (20), `blob_offer_s` (5), `log_batch` (200), `cand_recheck_s` (5); they replace the wave 14 constants.

## Lab heads: the two-level hierarchy

Operator guide: [HIERARCHY.md](HIERARCHY.md). CLI: `ck admin hier`.

| Method and path | Role | Body | Reply |
|---|---|---|---|
| GET `/api/v1/hierarchy` | viewer | | the tree (below) |
| PUT `/api/v1/hierarchy/settings` | admin | `{"mode"?: "off"\|"manual"\|"auto", "lease_s"?, "lab_head_min_pcs"?, "lab_silence_s"?, "lab_head_standby"?: bool}` | the tree. Writes the tunables `hier_mode`, `lease_s`, ... (same store as `PUT /tunables`); audit event `hier.settings` |
| POST `/api/v1/hierarchy/labs/{lab}/head` | operator | `{"node": NODE_ID}` | `{"lab", "pinned"}`. Pin (promote, or move) the lab head of `lab` to that PC. 400 for an unknown node |
| DELETE `/api/v1/hierarchy/labs/{lab}/head` | operator | | `{"lab", "pinned": null}`; 404 if nothing was pinned |
| POST `/api/v1/hierarchy/labs/{lab}/drain` | operator | `{"on"?: true}` | `{"lab", "drain"}`; `false` undoes it |

Tree: `{"mode", "active" (leases are being granted), "paused" (reason or null), "settings", "root": {"node_id", "role", "epoch", "addr", "pcs_direct"}, "standbys": [node ids], "labs": [Lab], "orphans": [{"node_id", "lab", "leases", "silent_s", "expires_in_s"}], "unleasable_jobs": [job ids]}`. Lab = `{"lab", "pinned", "drain", "state": "active"|"partitioned"|"released"|"absent"|"none", "pcs_at_root", "pcs": [{"id", "host", "threads", "busy", "virtual", "cpu" (percent), "mem_used", "mem_total"}] (under the lab head), "standby": null | {"node_id" (the standby lab head, "PC/lab-NAME"), "pc" (the PC chosen to host it), "ready" (its lab head has connected), "host", "addr" ("ip:port", null until ready), "chosen_s"} (set when `lab_head_standby` is on), "silent_s", "orphaned_leases", "head": null | {"node_id", "host", "addr", "port", "api", "connected_s", "pcs", "threads", "leases" (as the lab head sees them: job, a, b, got, finished, age_s), "root_inflight" (as the root sees them), "granted_in_flight", "lab_granted", "lab_done", "leases_done", "lag_s" (age of its last status), "unacked", "partitioned", "released", "rate", "chunks_inflight"}}`.

SSE `event` carries `labhead.join`, `labhead.leave`, `labhead.partition`, `labhead.heal`, `labhead.lease` (live, not stored), `hier.*`. New tunables (group Hierarchy): `hier_mode`, `lease_s`, `lab_head_min_pcs`, `lab_silence_s`, `lab_head_standby`, `lab_head_port`, `lab_part_s` (group Network: `stage_timeout_s`). `PUT /hierarchy/settings` takes only the first five (as `mode` for `hier_mode`); the other two go through `PUT /tunables`.

Wire (agents and heads only): `HELLO_W`'s Hello gains `lab_head {lab, port, api, pcs}`; frame `PARTIAL` (31) = a RESULT-shaped finished piece of a lease; CTRL keys `move_to`, `start_lab_head`, `hier_job`, `lab_keys`, `lab_release`, `lab_adopt`, `lab_exit`, `lab_drain` (root to agent/lab head) and `lab_status`, `leases`, `lease_done` (lab head to root). Old agents ignore every new key and frame.

## Advisor

Suggestions next to the alerts; rules, options and the catalogue are in [ADVISOR.md](ADVISOR.md).

| Route | Role | Body | Reply |
|---|---|---|---|
| GET `/api/v1/advice` | viewer | | `{"enabled", "policy": {kind: "off"|"suggest"|"auto"}, "suggestions": [Suggestion], "accuracy": {kind: 0..1}}` |
| GET `/api/v1/advice/history` | viewer | | `{"history": [Applied], "accuracy"}` newest first |
| PUT `/api/v1/advice/settings` | admin | `{"enabled"?: bool, "policy"?: {kind: mode or null}}` | `{"enabled", "policy", "suggestions"}` |
| POST `/api/v1/advice/{id}/preview` | operator | `{"option"?: id}` (default: recommended) | `{"option", "preview": {"would_change", "valid", "error"}, "changes_nothing": true}`; 409 if the suggestion no longer applies |
| POST `/api/v1/advice/{id}/apply` | admin | `{"option"?: id}` | `{"applied": Applied, "suggestions"}`; re-checks against fresh data (409 if gone), 400 if the option has only steps; audit event `advice.applied` |
| POST `/api/v1/advice/{id}/undo` | admin | | `{"suggestions"}`; restores the old value; 404 if nothing to undo |
| POST `/api/v1/advice/{id}/dismiss` | operator | `{"snooze_h"?: 24}` | `{"suggestions"}` |

Suggestion = `{"id", "kind", "severity": "info"|"tip"|"warn", "title", "why", "options": [Option], "effect", "action" (the recommended option's), "since_ms", "policy", "accuracy"}`. Option = `{"id", "title", "action": {"type": "tune"|"node_state"|"ha_target"|"power"|"doc", ...} or null, "steps": [text], "expected_effect", "metric", "metric_from", "metric_to", "cost", "risk": "low"|"med"|"high", "reversible", "auto_safe", "time_to_effect", "confidence", "success_check", "recommended", "score"}`. Applied = `{"id", "kind", "option", "action", "by", "auto", "ts_ms", "metric", "before", "predicted", "follow_s", "undone", "outcome": {"observed", "predicted_gain", "observed_gain", "hit"} or null}`. SSE event `advice` sends `{"suggestions": [...]}` whenever the list changes. Events: `advice.applied`, `advice.auto`, `advice.undone`, `advice.dismissed`, `advice.settings`, `advice.outcome`.

Roles are enforced twice: `users::min_role` gives `PUT /advice/settings`, `POST /advice/{id}/apply` and `/undo` the admin role and the other `POST /advice/*` routes operator, and the handlers check again. Dismiss defaults to a 24 h snooze (`snooze_h` is clamped to 0 to 720); undo snoozes the suggestion for 1 h; apply snoozes it for the 10 min follow-up window and answers 409 when asked again inside it.

## Logs

`GET /api/v1/logs?since_ms=&kind=&level=&role=&limit=` (operator): `{"logs": [line]}`, newest matching JSON lines from this head's log files. `level` is `error|warn|info|debug` (default `debug`, i.e. everything), `limit` defaults to 200 (1 to 5000), `kind` is a prefix such as `node.` . CLI: `ck logs`. Details: [LOGGING.md](LOGGING.md).

## Limits on a chunk (memory, output, staging)

Agent-side tunables in group Agent limits, and one head-side tunable in group Network (full table in [OPERATIONS.md](OPERATIONS.md) section 8):
- `job_mem_pct` (default 80): a `cmd` program, with all its processes, is stopped when its group uses more than this share of the PC's RAM; the chunk fails with "memory over the N MB limit (job_mem_pct), program stopped". 0 turns the check off. The check runs about every 300 ms.
- `out_max_mb` (64): stdout kept per chunk; the program is stopped when it prints more ("output over the N MiB limit"). Put big results in `$CK_OUT` files instead.
- `out_files_mb` (256): total size of `$CK_OUT` files per chunk, checked while the program runs.
- `stage_timeout_s` (120): a chunk (or lab-head lease) reserved for a PC while its blobs download is re-staged from the head after this long plus 1 s per 2 MB of missing blobs, then given to another PC after the same time again (the slow PC is skipped for that job). If the blob is no longer on the head the job fails with a message naming it.
