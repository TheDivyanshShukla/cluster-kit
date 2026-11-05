# Swarm plan: wave 7, power modes (sleep / wake) and dynamic heads (auto-recruited standbys), plus the 1,000-PC ceiling
status: running        updated: 2026-11-05
Waves 1-6 are done (records: docs/loop/STATE.md, docs/CONSOLE_REQUIREMENTS.md Part C).

User ask (2026-11-05): "different modes and methods and full control on systems: make them sleep so they are power
efficient and turn them on on command; dynamic heads, multiple heads as per need, recovery at every failure".
Requirement IDs: NET-06 (Wake-on-LAN, power), HA-10 (new: keep N standbys, recruit a replacement after every failover).

## Contracts (frozen; lead-owned; need a change? exact code in your report)
- `rust/src/proto.rs`: `Inventory.macs: Vec<String>`; the CTRL JSON keys comment block right below Inventory
  (`power`, `wol`, `standby_of` head -> agent; `standby_started` / `standby_failed` agent -> head).
- `rust/src/jobs.rs`: `PowerSettings` (store meta "power", JSON) and `HaSettings` (store meta "ha_settings", JSON), with defaults.
- `docs/API.md` section "Power and dynamic heads (wave 7)": routes, bodies, replies, events, and the eco / recruit rules.
- `console/src/lib/api/schema.ts`: `NodeRow.power`, `PowerSettings`, `PowerReply`, `HaSettings`, `HaSettingsReply`.
- No store.rs change: settings and the asleep list live in store meta (`ctx.store.meta(k)` / `set_meta(k, v)`); meta "asleep" = JSON list of node ids the head put to sleep.
- No new crates or npm packages.

## Rules for every agent
- Touch ONLY the files you own. Read anything. No git commits, pushes, stashes or `git add`. No secrets.
- Rust: `cd rust && CARGO_TARGET_DIR=target-<dir> cargo test --offline --release --no-default-features` (b1 target-a1, b2 target-b2, b3 target-b3, b5 target-a3).
  Keep the crate compiling after each edit; a break in a file you do not own: wait a minute and retry.
- Every `ck` process: `CK_PORT=<your port>`, `CK_DATA=<scratchpad>/data...`, `CK_KEY_DIR=<scratchpad>/keys`, `CK_FLEET=open`,
  `CK_PRESENCE=off`, `CK_POWER_DRY=1` (NEVER let a test suspend this Mac), kill everything you start.
  Scratchpad: `/private/tmp/claude-501/-Users-divyansh-Desktop-code-divy-cluster-kit/3503507e-9975-404d-a2ea-555606dce1c0/scratchpad/w7<you>/`.
- Style: short, comments only where non-obvious, `ponytail:` on deliberate shortcuts, a test per non-trivial piece.
  Report <= 12 lines: built / files / check output / open problems / integration notes.

## Tasks
| id | agent | owns (exclusive) | acceptance check | status |
|----|-------|------------------|------------------|--------|
| T31 | b1 | `rust/src/identity.rs`, `rust/src/agent.rs`, `rust/src/presence.rs` | MACs in inventory (Linux /sys/class/net/*/address, macOS `ifconfig`/`networksetup`, Windows `getmac /fo csv /nh`; skip lo and all-zero); pure `magic_packet(mac) -> [u8; 102]` + test; CTRL `power: sleep` runs the OS suspend (Linux `systemctl suspend`, macOS `pmset sleepnow`, Windows `rundll32.exe powrprof.dll,SetSuspendState 0,1,0`), CK_POWER_DRY=1 only logs `power: would sleep now`; CTRL `wol` sends magic packets to 255.255.255.255:9 and the /24 broadcast:9; CTRL `standby_of` spawns once `ck head --standby --head ADDR --no-local-worker` with CK_DATA=<agent CK_DATA or ck-data>/standby, reports `standby_started`/`standby_failed` | todo |
| T32 | b2 | `rust/src/head.rs` | pub fns for the API: `power(ctx, ids, "sleep"/"wake", actor) -> Result<Vec<String>, String>`, `power_settings_changed(ctx)`, `ha_settings_changed(ctx)`, `power_state(ctx, node_id) -> &'static str` ("awake"/"asleep"/"waking"); WoL from the head (pure helper may live in agent.rs: call `crate::agent::magic_packet`) plus CTRL wol to one connected agent on the same /24; eco tick every 30 s per docs/API.md rules (never the head PC, a standby, a PC with work in flight; keep keep_awake awake; wake_on_demand wakes asleep PCs matching a queued job's selector); asleep list in meta "asleep", a PC that reconnects leaves it; HA-10 recruit tick every 10 s (repl::standby_count vs HaSettings.target_standbys, candidate = connected, real, matches selector, not already standby, prefer a lab with no standby; one CTRL standby_of per 60 s); events power.sleep / power.wake / ha.recruit; unit tests for the pure choosers (who to sleep, who to wake, whom to recruit) | done (live run pending) |
| T33 | b3 | `rust/src/api.rs`, `rust/src/users.rs`, `rust/src/jobcli.rs`, `rust/src/dash.rs` | the 5 routes in docs/API.md wave 7 with validation, roles in users::min_role and ROUTES, NodeRow JSON gains `power` (head::power_state), CLI `ck admin sleep ID..`, `ck admin wake ID..`, `ck admin power [off|eco] [IDLE_MIN] [KEEP_AWAKE]`, `ck admin standbys [N] [SELECTOR]` (CLI_MAP parity test passes); until b2's pub fns land, stub calls behind the exact signatures in T32 and tell the lead | todo |
| T34 | b4 | `console/src/**` except `schema.ts`, `app.css`; `console/scripts/mock-api.ts` | Nodes: Sleep / Wake actions (operator) with confirm, an Asleep / Waking badge on rows and map tiles (dimmed tile, moon icon); Settings: Power card (mode off/eco, idle minutes, keep awake, wake on demand) and a "Standby heads to keep" control (target 0-5, selector with preview via GET /api/v1/select) in the HA panel; mock routes for all of it; `bun run check`, `bun test`, `bun run build` green; screenshots 1440 and 390 (JPEG q70, look once, delete .playwright-mcp after) | done |
| T35 | b5 | read-only investigation (edits nothing in src; may write files only in its scratchpad) | find why 1,000 agents cap at ~440 tasks/s with the head at ~29 % CPU and agents near 0 % (one process or 4 processes of 250 make no difference; 200 agents reach ~2,100 tasks/s). Reproduce, profile (`sample <pid> 5` on macOS, per-thread states), read head.rs dispatch/feeder/heartbeat paths; report the root cause with evidence and an exact patch (diff text) for head.rs that b2 or the lead can apply | todo |

Shared acceptance (lead, at integration): with CK_POWER_DRY=1, 20 virtual agents + 2 real-looking agents: `ck admin power eco 1 2` sleeps idle PCs after a minute (events power.sleep), a submitted job wakes them (power.wake, magic packet logged); kill -9 the primary with a standby: after the failover the new primary recruits a new standby within ~20 s (event ha.recruit, /api/v1/ha shows 1 standby again); the 1,000-agent ceiling explained and, if the patch is small, fixed and re-measured.

## File ownership map
- lead: `proto.rs`, `jobs.rs`, `main.rs`, `repl.rs`, `sched.rs`, `select.rs`, `store.rs`, `sim.rs`, docs, `scripts/`, `schema.ts`, `app.css`
- b1 `identity.rs`, `agent.rs`, `presence.rs` · b2 `head.rs` · b3 `api.rs`, `users.rs`, `jobcli.rs`, `dash.rs` · b4 rest of `console/` · b5 none

## Integration steps (lead)
1. Reports. 2. cargo test (release, GPU), console check/test/build, chaos.sh 5/5. 3. Shared acceptance above. 4. Docs (OPERATIONS.md power + standbys), STATE.md, requirements NET-06 / HA-10.
