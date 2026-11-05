# Swarm plan: wave 15, the last unbuilt requirements (JOB-08, JOB-12, FLT-07, SCH-04 quotas, UI-07, docs)
status: running        updated: 2026-11-05

## Contracts (lead-fixed; h1 implements, h2 consumes)
- Templates (JOB-12): migration 9 table `templates(name PK, spec_json, note, owner, created_ms)`. GET /api/v1/templates ->
  `{"templates": [{"name","spec","note","owner","created_ms"}]}`; POST /api/v1/templates `{"name","spec","note"}` (spec = the
  POST /jobs body; researcher; same name = replace, only owner or admin); DELETE /api/v1/templates/{name}; POST /api/v1/jobs
  `{"template": "name", ...overrides}` merges overrides over the stored spec. SSE `changed` what="templates". CLI: `ck job
  template ls|save NAME FILE.json|rm NAME`, `ck job submit --template NAME`.
- Output files (JOB-08): each cmd/list chunk gets an empty dir in env `CK_OUT`; after exit 0 the agent uploads every regular
  file there (total <= `out_files_mb` tunable, default 256; over = chunk error) as blobs (existing blob store, job-scoped
  token) and reports `[{name, sha, size}]` with the result. Table `job_files(job_id, start, end, name, sha, size)`.
  GET /api/v1/jobs/{id}/files -> `{"files": [{"chunk": [a, b], "name", "sha", "size"}]}`; download = GET /api/v1/blobs/{sha}.
  Job JSON gains `"files": N`. CLI `ck job files ID [--get DIR]`. Blobs referenced by job_files are not LRU-evicted while the job row exists.
- Identify (FLT-07): POST /api/v1/nodes/identify `{"nodes": [id], "secs": 30}` (operator; secs 1..600). CTRL `{"identify": secs}`.
  Agent: logs it, rings the terminal bell, on Linux writes a banner to /dev/tty1 if writable, on macOS `say` is NOT used; node JSON
  gains `"identify_until_ms"` (null when off). SSE node update as for any node change.
- Quotas (SCH-04 rest): user JSON gains `"quota": {"max_pcs": 0, "max_running": 0}` (0 = no limit). PUT /api/v1/users/{name}/quota
  (admin). Scheduler: a user's running jobs together never hold more than max_pcs PCs; more than max_running running jobs wait queued
  (state "queued", reason "quota"). Job JSON gains `"wait_reason"` (null | "quota" | "priority" | "no_pcs").
- Network page (UI-07): existing GET /api/v1/net and POST /api/v1/net/probe; h2 reads api.rs net_json for the shape.
- Every new route: in users::min_role and ROUTES and jobcli::CLI_MAP (the API-02 test). Also add POST /api/v1/sim (researcher) there.

## Tasks
| id | agent | owns (exclusive) | deliverable | acceptance |
|----|-------|------------------|-------------|------------|
| T60 | h1 | rust/src/** , docs/API.md, docs/JOBS.md | all contracts above; tests per piece; tunables for the wave-14 local consts g2/g5 left (`ponytail: make tunable` comments: blob partial holder delay, stall limit, offer interval, LOG_BATCH, candidate recheck) | cargo test green; live: a cmd job writing 2 files per chunk -> files listed and downloaded with matching sha; template run; identify sets the field; quota max_pcs 2 holds on 8 virtual agents; throughput `--synth 1` 200 virtual agents within 10 % of before |
| T61 | h2 | console/** | Templates (list, save from New job, run, delete), job detail Files tab (download links), Identify action on node menus + pulsing tile on canvas and floor map, user quota editor on Users, Network page /network (per-PC link speed, rtt, head uplink, per-lab table, probe button for operators) in nav; mock-api for all; then UI-14: viewer and researcher walkthrough (what is hidden/disabled), Users and Nodes, dark at 1440 | check/test/build + noscroll + JPEG q70 screenshots 1440/390 of /network, job Files, Users quota, as viewer and researcher |
| T62 | h3 | docs/ARCHITECTURE.md (new), README.md | ARCHITECTURE.md: components (ck roles, protocol, store, repl/HA, sched, blobs, log, research, sim, console embed, SDK), data flow of one job, failure handling, where each lives (file paths); README quick start (build, head, agent, first job, console, SDK) checked against main.rs/jobcli.rs usage. Plain short sentences. | every command in README run once against a local head (port 9720, CK_POWER_DRY=1) and works |

Rules: as wave 14 above. Ports: h1 9700, h2 dev 5205, h3 9720. Cargo target: target-h1 (h3 builds its own copy with target-h3 only if needed).

# Swarm plan: wave 14, close every gap that does not need the lab
status: done        updated: 2026-11-05
User (2026-11-05): "go decide on your own and do it all" (after the gap list: audit round 2, known bugs, bulk gaps, torch
speed, console polish, unbuilt items). Lab pilot stays for when the user has lab access.

## Contracts (lead-fixed; g5 implements, g4 consumes; names are final)
- GET /api/v1/ha adds `"head": {"node_id", "addr", "since_ms"}` (who is primary now, since when, epoch-start ms).
- Node JSON `power` object adds `"since_ms"` (when the PC entered its current power state; null if unknown).
- GET /api/v1/power adds `"labs": [{"lab", "watts", "awake", "asleep", "waking"}]` (watts from the same power model sim.rs/eco uses).
- SSE event `changed` `{"what": "reserves"|"bookings"|"timetable"|"candidates"|"pools"|"caps"|"labels"|"windows"}` sent after
  each write of that thing (and `candidates` when the ranking changes, at most every 5 s). Console refetches that list.
- SSE event `log` `{"lines": [log JSON objects]}`: batched every 500 ms, at most 200 lines per batch, only while a viewer
  is connected; secrets already hashed by log.rs.
- POST /api/v1/sim (researcher): body = the `ck sim` scenario JSON; reply = the same JSON `ck sim` writes (summary + series).
  Caps: nodes <= 20,000, sim hours <= 48, one run at a time (409 busy), runs on its own thread.
- New tunables go in rust/src/tune.rs, owned by g1. Others: give g1 nothing; add your knob as a local const with a
  `ponytail: make tunable` comment and put the key/default/range in your report; the lead moves it.

## Rules for every agent
- Touch ONLY the files you own. Read anything. No git commits, pushes, stashes or `git add`. No secrets in code or logs.
- Rust check: `cd rust && CARGO_TARGET_DIR=target-<you> cargo test --offline --release --no-default-features`. Shared tree:
  if a file you do not own breaks the build, wait a minute and retry; never fix it yourself.
- Every `ck` process: `CK_PORT=<your base>` (bases below; +1..+3 are used too), `CK_DATA`/`CK_KEY_DIR` in your scratchpad,
  `CK_FLEET=open` unless testing secured, `CK_PRESENCE=off`, `CK_POWER_DRY=1` (never sleep this Mac). Kill only PIDs you
  started; never `pkill -f`. curl is blocked: use python3 urllib.
- Scratchpad: `/private/tmp/claude-501/-Users-divyansh-Desktop-code-divy-cluster-kit/3503507e-9975-404d-a2ea-555606dce1c0/scratchpad/w14<you>/`.
- No new crates, npm packages or Python deps (SDK stdlib only, ruff clean, py39 target).
- Report <= 12 lines: built / files / check output (pasted) / measured numbers / open problems / notes for the lead.

## Tasks
| id | agent | owns (exclusive) | deliverable | acceptance |
|----|-------|------------------|-------------|------------|
| T50 | g1 | rust/src/{agent,kernels,store,tune,main,jobcli,log}.rs | (1) cmd/list output cap enforced WHILE reading (kernels.rs ~333 uses wait_with_output: stream stdout, kill the child at the cap, clear error). (2) Retention: events older than `events_keep_days` (30) and parts of jobs finished more than `parts_keep_days` (0 = forever, default 0) pruned by a background thread started from the head path in main.rs, batched deletes so dispatch never waits; job summary rows stay. (3) OPS-04 backups: `ck admin backup [PATH]` (VACUUM INTO, safe while the head runs) + scheduled `backup_every_h` (0 = off) keeping `backup_keep` (7) files in CK_DATA/backups. (4) Docs: OPERATIONS.md backup + restore section. | cargo test green; a test per piece; live: a cmd printing 1 GB stops at the cap within 1 s with bounded RSS |
| T51 | g2 | rust/src/{blobs,repl}.rs, docs/BULK.md | (1) Peers serve pieces they already hold of a blob still downloading (piece bitmap; fetchers pick any holder per piece). (2) Blobs reach standby heads (standby pulls missing blobs from the primary over the existing link or the blob port, verified by sha; after takeover the new head serves them). (3) After a head restart or takeover the holder map is rebuilt at once (agents re-send `have` on reconnect: verify; persist the map if it is not). (4) Mixed-version heads: a head that sees a peer without signed-beacon support logs a clear warning and the ha status shows it; OPERATIONS.md-ready text in your report for rolling upgrades (standby first, switchover, then the old head, agents in waves). | cargo test green; live: 200 MB to 10 agents with 1 seed shows partial-piece serving (time below whole-blob peer tree); kill primary after a blob job, standby serves the blob to a new agent |
| T52 | g3 | python/clusterkit/**, python/tests/**, examples/sdk/** | Warm runner: the first task of a job on a PC starts a per-user warm Python worker (unix socket in a 0700 dir under the PC's temp/CK_DATA, keyed by code hash + Python path, idle exit after `warm_idle_s` 120 s); later tasks and later train rounds with the same code reuse it, so imports (torch) load once. Falls back to today's path on any error. Same results bit for bit. Windows: off. | SDK tests on 3.9 and 3.12 + ruff; live on 4 virtual agents: examples/sdk/ml_torch.py round time before/after |
| T53 | g4 | console/** | Polish: /live legend never overlaps a lab (place in free corner or collapsible), lab strip text never clips (ellipsis + title), Overview legend no clip; event rail human labels for every kind (one map in lib, tested); head id/since in topology from `ha.head`; PC power since; lab watts in power panel and wall; logs/reserves/candidates/timetable/bookings use SSE `changed`/`log` (keep a slow 60 s poll as backup); Simulation Lab page /sim (SIM-05/UI-09): scenario form + presets from examples/sim, run via POST /api/v1/sim, charts of the result; mock-api.ts serves all new contract fields. Then UI-14: check viewer and researcher roles, Users and Nodes pages, dark mode at 1440 and 390, fix what is wrong. | `bun run check && bun run test && bun run build` + noscroll script + JPEG screenshots (q70) at 1440/390, light+dark, of /live, /, /sim, /settings |
| T54 | g5 | rust/src/{api,head,sched,jobs,research,sim}.rs, docs/API.md | Implement the contracts above (ha.head, power since_ms, power labs watts, SSE changed + log, POST /sim). SCH-04 fair share: when jobs of different users share a priority, PCs split by user share (equal by default) instead of oldest-first; SCH-09 preemption: a higher-priority job arriving takes PCs at their next chunk boundary (no kill), and an optional `preempt: true` cancels lower jobs' outstanding chunks at once (requeued, credited exactly once). Pure logic in sched.rs with tests. API.md wave 14 section. | cargo test green; live: two users' jobs on 8 virtual agents split ~4/4; a priority job takes all PCs within one chunk time, totals exact |
| T55 | g6 | scripts/stress/**, docs/audit/ROUND2.md | Audit round 2 on a `git archive HEAD` copy built in your scratchpad (never edit the tree): 20-min soak with 25 SSE viewers (RSS, fd, thread counts, tasks/s drift); secured fleet (CK_FLEET=secured) with kill -9 and kill -STOP/-CONT of agents, primary and standby; clock jump (faketime not available: simulate with a big beacon/heartbeat skew if possible, else note); standby replay of 100k changes (time + memory); 1,000 alerts open/close; 10k-item list job; huge stdout. Each finding: repro, cause (file:line), proposed patch tested on your copy. | ROUND2.md with numbers and patches; report the top 5 |

Ports: g1 9610, g2 9620, g3 9630, g4 dev 5204 (mock on its usual port + 4), g5 9650, g6 9660-9699.
Cargo targets: target-g1 .. target-g6.

## Integration (lead)
1. read reports 2. wide gate: cargo test, chaos.sh, console check/test/build + noscroll, SDK 3.9/3.12, ruff 3. apply g6 patches that hold 4. tick CONSOLE_REQUIREMENTS boxes that are really done 5. STATE tick 22 6. commit.

# Swarm plan: wave 10b (job fields max_pcs, placement, mem_mb) follows wave 9 below

## Wave 10b
Contracts (lead, done): `JobSpec.max_pcs`, `placement` (`jobs::PLACEMENTS`), `mem_mb` (rust/src/jobs.rs); API parsing in api.rs `client_job_json`; proto CTRL `{"job": {"id", "mem_mb"}}` (comment below Inventory); docs/SDK.md section 4 (semantics).
| id | agent | owns | deliverable | status |
|----|-------|------|-------------|--------|
| T40 | f1 | `rust/src/head.rs`, `rust/src/sched.rs` | max_pcs: a job never holds chunks on more than max_pcs PCs at once; placement: spread (today), pack (prefer PCs already running this job, then most threads), fastest (highest calib_mhs first), efficient (smallest set of fastest PCs whose summed rate meets the job in about the time `spread` would take x 1.2, others left idle so eco can sleep them); eco wake-on-demand wakes at most max(0, max_pcs - awake matching PCs) for a job with max_pcs (else as today); send CTRL job {id, mem_mb} to an agent before its first TASK of a job with mem_mb > 0; pure placement chooser in sched.rs with tests | todo |
| T41 | f2 | `rust/src/agent.rs`, `rust/src/kernels.rs` | on CTRL job remember mem_mb per job; when running a chunk of that job use copies = min(threads, max(1, floor(free RAM MB x 0.8 / mem_mb))) (free RAM from sysinfo, refreshed per chunk); test the copy count function | done |
| T42 | f3 | `rust/src/store.rs`, `rust/src/api.rs` (job JSON only) | migration 6: jobs columns max_pcs INTEGER, placement TEXT, mem_mb INTEGER (default 0/''), saved by create_job and insert_job_with_id, read back into JobSpec on resume; JobRow JSON in the API gains the three fields; a resume test | done |
| T43 | f4 | `console/**` except schema.ts, app.css | New job form: "Use at most [ ] PCs", placement Segmented (Spread / Pack / Fastest / Efficient, with one-line help each), "Memory per copy [ ] MB"; job detail shows them; mock accepts them; check/test/build + screenshots 1440/390 | done |

# Swarm plan: wave 9, every value adjustable + head choice by calibration + one shared settings page
status: done        updated: 2026-11-05
Waves 1-7 are done (records: docs/loop/STATE.md). Wave 8 (console design, docs/DESIGN.md) is running (agent c1, console/**).

User asks (2026-11-05): "every thing and value should be adjustable"; "dynamic head and calibration can help to find
best suitable head or heads"; "one shared page" (one console page for all of it).

## Contracts (frozen; lead-owned; need a change? exact code in your report)
- `rust/src/tune.rs`: the registry `KNOBS` (keys, groups, ranges, scope) and `get` / `secs` / `n` / `validate` / `load` /
  `set` / `agent_values` / `apply_remote` / `listing`. Already wired by the lead: heartbeat, lost_after, pipeline,
  target_chunk_s, ring_max, member_window_s, standby_step_s, switchover_wait_s, max_items, max_item_bytes
  (`proto::heartbeat_every()`, `proto::lost_after()`, `proto::pipeline()`, `sched::target_secs()`).
- `rust/src/sched.rs`: `HeadInputs`, `HeadWeights`, `head_score(x, best, w) -> (score, [speed, memory, stability, free])`.
- `rust/src/repl.rs`: `SETTINGS_META` (power, ha_settings, tune, asleep) replicate inside "config" entries.
- `docs/API.md` section "Every value adjustable, and head choice (wave 9)"; `console/src/lib/api/schema.ts` Tunable, Candidate.
- CTRL head -> agent `{"tune": {key: value}}` (agent-scope values that differ from default; send on connect and on change).
- No new crates or npm packages.

## Rules for every agent
- Touch ONLY the files you own. Read anything. No git commits, pushes, stashes or `git add` (the lead commits). No secrets.
- Rust: `cd rust && CARGO_TARGET_DIR=target-<dir> cargo test --offline --release --no-default-features` (d1 target-b2, d2 target-a1, d3 target-b3).
  Keep the crate compiling after each edit; a break in a file you do not own: wait a minute and retry.
- Every `ck` process: `CK_PORT=<your port>`, `CK_DATA` / `CK_KEY_DIR` in your scratchpad, `CK_FLEET=open`, `CK_PRESENCE=off`,
  `CK_POWER_DRY=1`. Kill ONLY the processes you started (by PID; never `pkill -f "release/ck"`, other agents run heads too).
  Scratchpad: `/private/tmp/claude-501/-Users-divyansh-Desktop-code-divy-cluster-kit/3503507e-9975-404d-a2ea-555606dce1c0/scratchpad/w9<you>/`.
- A literal you replace must keep today's default (the registry default equals the old constant). Remove the old const.
- Style: short, comments only where non-obvious, `ponytail:` on deliberate shortcuts, a test per non-trivial piece.
  Report <= 12 lines: built / files / check output / open problems / integration notes.

## Tasks
| id | agent | owns (exclusive) | deliverable | status |
|----|-------|------------------|-------------|--------|
| T36 | d1 | `rust/src/head.rs` | (1) at head start `tune::load(ctx.store.meta("tune").as_deref())`; pub `tune_changed(ctx)` (b/c API PUT) that pushes CTRL tune to every connected agent; CTRL tune also right after an agent's admission. (2) Replace head literals with knobs: max_chunk, max_attempts, max_events, noq_grace_s, recruit_tick_s, recruit_every_s, recruit_retry_s, eco_tick_s, waking_for_s, wake_retry_s, wake_max, push_max_kb if the head checks it, target_standbys (HaSettings.target_standbys stays the stored source; keep both in step: the knob is a fallback when meta "ha_settings" is absent). (3) Head choice: pub `candidates(ctx) -> Vec<serde_json::Value>` per docs/API.md Candidate (inputs from node calib_mhs, inventory cores/ram, `node.lost` events in the last 24 h from the store, presence; weights from head_w_*), and the HA-10 recruiter picks the best eligible candidate (lab without a standby first). Unit test the candidate ranking with fake nodes. | todo |
| T37 | d2 | `rust/src/agent.rs`, `rust/src/client.rs`, `rust/src/presence.rs`, `rust/src/kernels.rs`, `rust/src/identity.rs` | CTRL `tune` -> `tune::apply_remote`; replace agent literals with knobs: unacked_max, unacked_max_mb, presence_idle_s (CK_PRESENCE_IDLE_SECS keeps working as a fallback), presence_probe_s, calib_s, energy_every_s, out_max_mb, client_failover_s (CK_FAILOVER_SECS stays as a fallback); values must be read where used (not cached at start) so a change applies without restart where that is cheap; unit test apply_remote ignoring head-scope keys | done |
| T38 | d3 | `rust/src/api.rs`, `rust/src/users.rs`, `rust/src/jobcli.rs`, `rust/src/dash.rs` | routes GET/PUT `/api/v1/tunables` and GET `/api/v1/ha/candidates` (docs/API.md wave 9), roles + ROUTES + CLI_MAP; PUT stores `tune::set` output in meta "tune", writes event `tune.changed` (old -> new), calls `crate::head::tune_changed(ctx)` (d1 adds it; wait if missing); `max_body_mb`, `max_list_body_mb` replace MAX_BODY / MAX_LIST_BODY; users.rs: session_idle_h, session_max_d, login_fail_max, login_fail_window_min replace the consts (ITERS stays: changing it breaks no hash but is not a runtime knob); CLI `ck admin tune` (list grouped), `ck admin tune KEY VALUE|default`, `ck admin heads` (candidates table) | done |
| T39 | (wave 8 c1, after its design task) | `console/**` | the one shared page, `/settings` renamed "System" in the nav: sections Head and standbys (HeadTopology + a ranked candidates table with score bars and "why not"), Power, and All settings (every tunable grouped, search, inline number field with unit and range, Changed badge, reset to default, env values read-only, a sticky Save bar with the change count and a confirm listing risky groups: Liveness, Head failover, Security) | todo |

Shared acceptance (lead): change lost_after_s to 8 and calib_s to 2 from the console: `/api/v1/tunables` shows source db, an agent logs the new values, a standby after failover still has them; `ck admin heads` ranks a faster/stabler PC first and recruiting picks it; 133+ Rust tests, console check/test/build, chaos 5/5.

## File ownership map
- lead: `proto.rs`, `jobs.rs`, `main.rs`, `repl.rs`, `sched.rs`, `select.rs`, `store.rs`, `sim.rs`, `tune.rs`, docs, `scripts/`, `schema.ts`, `app.css`
- d1 `head.rs` · d2 `agent.rs`, `client.rs`, `presence.rs`, `kernels.rs`, `identity.rs` · d3 `api.rs`, `users.rs`, `jobcli.rs`, `dash.rs` · c1 `console/**`
