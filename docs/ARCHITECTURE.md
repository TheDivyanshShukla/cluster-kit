# Architecture

One page on how Cluster Kit fits together. Details live in the docs named in each section.

## Components

One binary, `ck` (`rust/`), plays every role. The roles differ only by command.

| Piece | What it does | Where |
|---|---|---|
| Head (`ck head`) | Accepts jobs, cuts them into chunks, hands chunks to agents, keeps the database, serves the API and console. | `rust/src/head.rs`, `jobs.rs` |
| Agent (`ck agent`) | Runs on each PC. Finds the head, pulls chunks, runs them, reports results. `--virtual N` fakes N PCs for tests. | `rust/src/agent.rs`, `kernels.rs`, `identity.rs`, `presence.rs` |
| Client (`ck run`, `ck job`, `ck admin`, `ck logs`) | Command line over the head's TCP port (`run`) or HTTP API (the rest). | `rust/src/client.rs`, `jobcli.rs` |
| Protocol | Frames `[kind u8][len u32][payload]` over TCP, UDP beacon for discovery, heartbeats, JSON control messages. | `rust/src/proto.rs` |
| Auth | Node keys, HMAC-sealed frames, join codes (agents); users, sessions, roles, API tokens (people). | `rust/src/auth.rs`, `users.rs` |
| Store | One SQLite file (WAL) with numbered migrations: nodes, jobs, parts, events, users, settings. | `rust/src/store.rs` |
| Scheduler | Pure functions: which job a PC serves, chunk size, reserves, fair share, quotas, preemption. The simulator calls the same code. | `rust/src/sched.rs`, `select.rs`, `sim.rs` |
| Replication and HA | Standby heads, replicated change log, election, fencing, switchover. | `rust/src/repl.rs` |
| Blobs | Content-addressed files (code, data) cached on every PC, sent direct, peer to peer or pre-staged; network estimates. | `rust/src/blobs.rs`, [BULK.md](BULK.md) |
| Reducers | How the stored parts of a job become one result. | `rust/src/reduce.rs` |
| Log | One JSON line per event in `CK_DATA/logs/`, never blocks the caller. | `rust/src/log.rs`, [LOGGING.md](LOGGING.md) |
| Research tools | Time breakdown, alert rules, experiment upload, daily report. | `rust/src/research.rs` |
| Tunables | Every adjustable value in one registry: env, then database, then default; pushed to agents. | `rust/src/tune.rs` |
| API and console | HTTP API with SSE, plus the SvelteKit console compiled into the binary by `rust/build.rs`. | `rust/src/api.rs`, `dash.rs`, `console/`, [API.md](API.md) |
| Python SDK | Standard-library client over the HTTP API: `ck.map`, `ck.sweep`, `ck.train`, plans, falls back to local. | `python/clusterkit/`, [SDK.md](SDK.md) |
| Launcher and Dask engine | `kit` scripts, firewall handling, the separate Dask engine. | `kit`, `kit.ps1`, `python/cluster.py` |

Ports (all move with `CK_PORT`): 7700 TCP agents and `ck run`, 7701 UDP discovery, 7702 HTTP API and console, 7703 TCP blobs.

## One job, start to finish

1. A client submits a job (`ck run`, the console, or the SDK through `POST /api/v1/jobs`). The head checks the role, stores the job row, and logs `job.submitted`.
2. An agent that has nothing in flight asks for work. `sched::pick_job` picks the job (priority, then fair share, then age) among jobs whose selector, reserves, windows and quotas allow that PC.
3. The head cuts a chunk sized to that PC's measured speed and sends it. Each agent keeps two chunks: one running, one queued, so it never waits on the network.
4. If the job names blobs, the head tells the agent where to fetch them; the agent verifies each piece by sha256 and caches the blob.
5. The agent runs the chunk (built-in kernel, your command, or the SDK runner) and sends the result. The result also asks for the next chunk.
6. The head stores the result as a part (the same `(job, start, end)` twice keeps one row) and, when a standby is connected, waits for the standby to acknowledge it. Then it sends an ACK and the agent drops its copy.
7. When every range is covered, the reducer combines the parts. The state becomes `done`, and the client prints the result or the SDK returns it.

## Failure handling

- **Agent silent or gone.** Both sides send heartbeats. No frame for `lost_after_s` marks the peer lost. The head requeues the agent's chunks. A chunk that fails is retried up to `max_attempts` times, then the job fails with the error. Duplicate results are ignored.
- **Slow agent at the end of a job.** The head may run the last chunks a second time elsewhere; the first result wins (`chunk.speculate`).
- **Agent holds unacknowledged results.** It keeps them and resends them to whatever head it reaches next.
- **Head dies.** Standbys hold a full database copy, fed by the replicated change log (a snapshot first if they are far behind). A standby that hears nothing from the primary for `lost_after_s` plus 2 s per rank in the standby list promotes itself and bumps the epoch. It starts dispatching only when more than half of the recent agents have reconnected, so a network split leaves at most one working side. Agents find the new head by beacon and resend what was not acknowledged.
- **Fencing.** Every beacon carries the epoch, signed with a key stored in the replicated database. A head that sees a higher epoch stops dispatching. A forged or unsigned epoch beacon cannot make a head stop or a standby follow.
- **Planned move.** `ck admin switchover NODE` waits until the standby has every change, hands over, and the old head becomes a standby. Work pauses for about 4 s.
- **Restart.** The primary restarts from the stored sequence number; standbys resync by snapshot. Jobs that were running resume.
- **Logs and backups.** The log writer drops lines rather than block. `ck admin backup` copies the database safely while the head runs. See [OPERATIONS.md](OPERATIONS.md).

## Where to read next

- Wire and HA details: top comments of `rust/src/proto.rs` and `rust/src/repl.rs`.
- Every route and JSON shape: [API.md](API.md). Jobs and reducers: [JOBS.md](JOBS.md).
- Running it day to day, backups, rolling upgrade: [OPERATIONS.md](OPERATIONS.md).
- The console's layout and failover animation: [DESIGN.md](DESIGN.md).
