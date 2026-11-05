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
