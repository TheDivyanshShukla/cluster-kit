# Cluster Kit 2: requirements and work plan

Draft v0.1, 2026-11-04, branch `feature/general-purpose`.

How to read this: Part A says what we build and why. Part B lists every requirement with an ID (`FLT-03`), a priority and a "done when" check. Part C is the to-do list in build order, plus the open questions. To change a requirement, edit its row. Never reuse an ID.

| Priority | Meaning |
| --- | --- |
| **P0** | Needed for the 200-PC demo and the paper. Must ship. |
| **P1** | Makes it a real lab data centre. Should ship within the 6 months. |
| **P2** | Later. Kept here so the idea is not lost. |

---

# Part A: what and why

## A1. Goal

Turn the college's 200+ lab PCs into one general-purpose compute facility that an administrator runs like a small data centre:

- Every PC is discovered and inventoried automatically.
- PCs are organised into labs and pools, with limits and reserved capacity.
- A scheduler feeds them work, and a student's use of their own PC always comes first.
- Any program in any language can be a job.
- A job survives the head failing.
- A web console shows and controls all of it, including a floor map of the labs.

Success criteria (all measured, none estimated):

1. 200 physical PCs in one cluster run a 24-hour full-capacity job with no manual intervention.
2. Killing the head mid-job: the job finishes with exactly the result of an uninterrupted run, and dispatch resumes within 15 s.
3. Pulling the network cables of 20 working PCs: their work is redone elsewhere, the result is exact, and each loss is detected within 10 s.
4. SHA-256 benchmark at 200 PCs reaches at least 90% of the PCs' summed stand-alone throughput. The paper measures 96.4% at 4 PCs.
5. A lab administrator who is not on the team can add a new lab of PCs and run a job from the console in under 30 minutes, using only the docs.
6. The experiments in A8 produce the paper's data.

## A2. Principles (what keeps it flexible and sustainable)

1. **General core, workloads at the edge.** The head and the agent know nothing about any particular workload. A workload is a program plus a contract (JOB-04). Built-in kernels exist only for benchmarks and simulation.
2. **One binary, one protocol, one database file.** `ck` runs every role (head, agent, relay, client, simulator). Head state lives in one SQLite file.
3. **API first.** The console, the CLI and scripts all use the same HTTP API. No action is console-only.
4. **Pure scheduler core.** Scheduling decisions are a pure function of state, events and a clock passed in, so the real head and the simulator run the same code.
5. **Students first.** During class a lab PC belongs to the student sitting at it. The cluster takes only what policy allows, at the lowest OS priority, and backs off when someone uses the PC.
6. **Secure by default.** There is no open mode on the campus LAN. Every PC, user and command is authenticated and audited.
7. **Few dependencies.** Every new crate or vendored JS file needs a written reason (what it saves, its size, its licence). Builds work offline.
8. **Versioned everything.** The wire protocol, the database schema and the config export carry versions. An old agent gets a clear "upgrade" message, never undefined behaviour.
9. **Measured, labelled numbers.** Every number in the console, the docs or the paper says where it was measured: real PCs, virtual nodes on one machine, or simulation.
10. **Small modules with owners.** Each module starts with a comment saying what it does, has at least two people who know it, and has tests.

## A3. Where we start

`ck` ([rust/src/main.rs](../rust/src/main.rs), 926 lines) works, and it is what the paper measured: 4 PCs, 87.38 MH/s, 96.4% efficiency. It has UDP discovery, persistent TCP, pull scheduling with two chunks in flight, requeue on disconnect, any-language `--cmd`, `--push` scripts, GPU SHA-256 and a dashboard on port 7702. The Dask engine ([python/cluster.py](../python/cluster.py)) also works and stays for now (D14).

Each limit below is removed by the requirement named in the last column.

| Limit today | Where | Fixed by |
| --- | --- | --- |
| No timeouts or heartbeat. A worker whose cable is pulled is never detected, so its chunks stay in flight and the job never finishes. A worker whose head dies silently waits forever. | main.rs:358-397, 621-662 | HA-01 |
| One job at a time | main.rs:467 | SCH-01 |
| The job dies when the client disconnects; results exist only in the client's stream | main.rs:485-500 | JOB-02 |
| A node is a connection number plus an IP address; a DHCP change or a reconnect makes it a new node | main.rs:360, 519-522 | FLT-01 |
| Node selection is `--only IP,IP`, capped at 255 characters (about 17 PCs) | main.rs:708 | ORG-04 |
| The discovery beacon assumes a /24 subnet | main.rs:550 | FLT-05 |
| `--push` resends the whole script with every chunk | main.rs:217-221, 678 | JOB-05 |
| All state is in memory; a head restart loses everything | main.rs:289-296 | HA-02 |
| One chunk size per job (total / (16 x PCs), clamped) | main.rs:476-477 | SCH-06 |
| No authentication: anyone on the LAN can run commands on every PC | README | SEC-01..04 |

The video system (`cw`: rust/src/watch, vision/, its docs and scripts) was removed on 2026-11-04. Some of it is general and already tested, so port those parts instead of rewriting them. Everything is in git history at commit `6eca566`; read a file with `git show 6eca566:<path>`.

| Port from 6eca566 | Gives us |
| --- | --- |
| `rust/src/watch/auth/mod.rs` | users, PBKDF2 passwords, sessions, roles, worker tokens, login throttling, audit records, redaction |
| `rust/src/watch/scheduler/mod.rs` | the pure lease-scheduler pattern: time passed in, lease generations, retries, worker capabilities |
| `rust/src/watch/fleet.rs` | node registry with exponential averages and idle/busy/slow/lost states |
| `rust/src/watch/join.rs` | joining with a pairing code; worker bundles served by the head |
| `rust/src/watch/ui/app.css`, `docs/swarm/UI_SHELL.md` | console design tokens (dense dark console) |

## A4. Vocabulary

| Term | Meaning |
| --- | --- |
| Node | One PC running the `ck` agent. Identified by a stable node ID, not by its IP. |
| Lab | A physical room. Has a floor plan, seats and optionally a timetable. |
| Seat | A position on a lab's floor plan. A node sits in at most one seat. |
| Pool | A named, logical group of nodes from any labs, for example `night-batch` or `gpu`. |
| Label | A key=value tag on a node (`lab=A`, `row=3`, `gpu=yes`). Selectors match labels. |
| Hardware class | A label set automatically from the inventory (CPU model, cores, RAM). Identical PCs share a class. |
| Cap | An upper limit on what the cluster may use on a node, pool or lab: threads, CPU %, RAM, temperature, hours. |
| Reserve | Capacity the scheduler must leave free, for example "keep 10 PCs free in Lab A" or "keep 20% of the fleet idle". |
| Window | A time rule, for example "Lab B, Mon-Fri 09:00-17:00: idle PCs only, cap 25%". |
| Reservation | A dated booking, for example "Lab C, 12 Dec 10:00-13:00, exam: no cluster work". |
| Job | One submitted piece of work: a program, an input space (a range or a list), a target selector and a policy. |
| Task | The piece of a job sent to one node (today's chunk). Tasks must be idempotent. |
| Lease | The right to run a task, with an expiry and a generation number. |
| Epoch | The head's term number. A head elected after a failover has a higher epoch, and messages from a lower epoch are refused. |
| Head roles | Primary (dispatches work), standby (keeps a replica and can take over), relay (fans out to one lab or subnet). |
| Node states | pending (asked to join), active, cordoned (takes no new tasks), draining (finishes its tasks, then cordons), maintenance, offline (shut down on purpose), lost (stopped answering), retired. |

## A5. Users and roles

| Role | Can |
| --- | --- |
| Viewer | See everything except secrets; use wall mode. |
| Researcher | Submit, pause and cancel their own jobs within quota; run experiments; download results. |
| Operator | Everything a researcher can do, for all jobs. Node actions (approve, cordon, drain, maintenance, restart, identify, upgrade). Edit labs, pools, windows and reservations. |
| Administrator | Users and roles, tokens, security settings, policies, fleet-key rotation, deletes. |

Power users also get:

- an API token per user, and a CLI command for every action;
- a command palette and keyboard shortcuts;
- saved views and the node selector language;
- config export and import;
- scripting through the API.

## A6. Architecture decisions

| # | Decision | Why | Rejected |
| --- | --- | --- | --- |
| D1 | `ck` stays one binary with roles: `head`, `agent` (today's `worker`), `relay`, `run`/`job` (client), `admin` (CLI for the API), `sim`. | One thing to build, install and upgrade on 200 PCs. | A separate binary per role. |
| D2 | Keep the binary TCP protocol for tasks and results (the hot path). Send control messages as JSON frames. HELLO carries a protocol version. | The paper measured this path. JSON for rare messages keeps the protocol easy to evolve. | HTTP polling for tasks (slower per task); gRPC (heavy dependencies). |
| D3 | Control plane: HTTP plus Server-Sent Events on the head, port 7702 (replacing dash.html), built on `tiny_http` and `serde_json`. | Proven in the removed video system; synchronous and small. | Async web frameworks (large dependency trees). |
| D4 | Head state in one SQLite file (WAL mode), `rusqlite` bundled, numbered migrations, no database server. | 200-2,000 nodes write at most a few hundred rows a second. Nothing to administer; one file to back up. | PostgreSQL (a server to run and secure). |
| D5 | The scheduler core is a pure library: state, events and a clock go in, decisions come out. Policies are plain functions chosen by name. | The head and the simulator run the same code, and tests are deterministic. | Scheduling logic spread over network handlers, as it is today. |
| D6 | High availability: one primary and ordered standbys holding a replica fed by an ordered change log. Election needs a majority of the known nodes. Epoch fencing. Agents keep results until the head confirms they are durable. | Survives a head crash without a consensus library. The majority rule stops two heads appearing after a network split. | A Raft library (new heavy dependency, 3+ heads needed); the database on a network share (a new single point of failure). |
| D7 | Hierarchy is optional: one relay per lab or subnet forwards discovery, tasks and files. | Crosses subnets, cuts the head's connection count, and sends each file over a lab's uplink once. | Always-hierarchical (more moving parts on small clusters). |
| D8 | Console UI (changed 2026-11-04): SvelteKit + Svelte 5 runes, TypeScript strict, Tailwind v4 (tokens in one `app.css`), Zod for every API response, `@lucide/svelte` icons, built with Bun as a static SPA (`adapter-static`, `ssr = false`) in `console/`. The build output is embedded in the `ck` binary (build.rs) and committed as `console/build/`, so lab PCs build and run with no Bun, no Node and no internet. No Node server, no Postgres, no Redis: the Rust head (D3, D4) is the only backend. SVG floor map, Server-Sent Events. | Components and types keep 17 pages maintainable; static output keeps one offline binary. | A Node server (adapter-node) with its own database (a second backend to run on the head); no-build vanilla JS (hard to keep 17 pages consistent); CDN scripts (labs have no internet). |
| D9 | Security: a per-node credential issued at approval, an HMAC-SHA256 challenge on connect, a MAC on every frame, user auth ported from 6eca566, and an audit log for every change. No TLS in v1; run on an isolated VLAN. | `sha2` is already a dependency. On a LAN, integrity and authentication matter most. | A TLS crate in v1 (bigger dependency, certificates to manage on 200 PCs). |
| D10 | Limits: the agent enforces caps itself (thread count, pausing), and the OS enforces ceilings through a systemd unit (`Nice=19`, `CPUSchedulingPolicy=idle`, `MemoryMax`, `CPUQuota`). | The kernel already knows how to give students priority. Less code. | A custom resource manager. |
| D11 | Workloads are processes that follow a contract (JOB-04). The only built-in kernels are SHA-256, synthetic tasks and Monte Carlo. | Keeps the core general; works with any language. | Per-workload code in the head (what the video system did). |
| D12 | Metrics in SQLite, downsampled: 1 s in memory for 10 minutes, 10 s for 7 days, 1 min for 90 days. | No Prometheus or Grafana to run. An optional `/metrics` endpoint can come later. | An external monitoring stack. |
| D13 | The "full capacity for a day" demo uses the SHA-256 proof-of-work benchmark, not real Bitcoin mining (A9). | Same hash, same load, measurable, no policy problem. | Pool mining. |
| D14 | The Dask engine stays as it is until Q8 is answered. | It is not in the way, and the decision needs the team. | Deleting it now. |

## A7. Prior art we borrow from (and cite)

- **Kubernetes:** labels and selectors, cordon and drain.
- **Slurm:** partitions, priorities, fair share with decaying usage, reservations, backfill.
- **HTCondor:** idle-desktop policies (start when the keyboard is idle, suspend when it is touched). The closest existing system to ours.
- **BOINC:** redundant computation to catch wrong results.
- **Self-scheduling (guided self-scheduling, factoring):** chunk sizing.

## A8. Experiments this enables (paper)

| # | Question | Needs |
| --- | --- | --- |
| E1 | How does throughput scale from 1 to 200 PCs, using the paper's protocol (interleaved, 7 repeats)? | ORG-04, EXP-01..03 |
| E2 | Where does a single head saturate, and how far do adaptive chunks, batching and relays move that point? | SCH-06, NET-04, HA-08, SIM-01 |
| E3 | What does losing k of N workers cost, in completion time and recomputed work? | HA-01, EXP-04 |
| E4 | After a head crash: recovery time, lost work and duplicates. Targets: under 15 s, 0 and 0. | HA-02..06 |
| E5 | Single head vs relays vs standbys: measured on 200 PCs, emulated at 1,000 nodes, simulated at 10,000. | SIM-01..04 |
| E6 | How much do idle-priority agents slow a student's foreground work? | SCH-10, SEC-04 |
| E7 | 24-hour soak: failures per hour, energy per unit of work. | OBS-03, JOB-09 |

## A9. The Bitcoin question

Use the built-in SHA-256 proof-of-work job (JOB-09) for the full-capacity day. It is the hash Bitcoin uses, it loads every core, and it gives clean numbers: hashes per second, utilisation, energy and recovered failures.

Real pool mining is not built unless the college approves it in writing:

- **Ownership:** the PCs and the electricity belong to the college. Mining on them without approval is misuse and usually against IT policy.
- **It earns essentially nothing:** Bitcoin is mined on dedicated chips.
  - 200 lab CPUs make roughly 2 GH/s of double SHA-256. The network makes around 10^21 H/s.
  - That is about two trillionths of the rewards: well under one rupee a day.
  - Mining alone, the fleet would find a block roughly once every ten million years.
- **It costs money:** 200 PCs x about 100 W x 24 h is about 480 kWh, roughly Rs 3,000-6,000 of electricity a day.

---

# Part B: requirements

## FLT: fleet discovery and inventory

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| FLT-01 | Stable node ID from the OS machine ID (`/etc/machine-id`, Windows `MachineGuid`, macOS `IOPlatformUUID`), stored with the node credential. The IP is only an attribute. | P0 | A PC that changes IP or reconnects keeps its ID, history and seat. |
| FLT-02 | Inventory sent at join and whenever it changes: CPU model, physical and logical cores, SHA extensions, RAM, GPU adapter, free disk, NIC speed, MAC, OS, kernel, ck version. | P0 | The node page shows it for every PC, and a RAM upgrade appears without manual edits. |
| FLT-03 | Hardware class label derived automatically from CPU model, cores and RAM. | P0 | 200 identical PCs show as one class, and an odd PC stands out. |
| FLT-04 | Join flow: new PCs appear as pending. An operator approves one PC or a selection, or sets an auto-approve rule (subnet plus join code). | P0 | An unapproved PC never receives a task. |
| FLT-05 | Discovery across subnets: the beacon carries the head's address; relays re-broadcast it in their subnet; `--head` and a DNS name also work; the last known head is remembered. | P0 | PCs on three subnets join one head without anyone typing an IP. |
| FLT-06 | Calibration at join and nightly: a 3-second SHA-256 run and a memory-bandwidth probe give a capacity score. | P1 | Scores of identical PCs agree within 3%. |
| FLT-07 | Identify: from the console, make a PC show its name full-screen for 10 s and beep. | P1 | A lab of 40 PCs can be mapped by walking the room once. |
| FLT-08 | Retire and forget a node, keeping its history. | P2 | |

## ORG: organising the fleet like a data centre

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| ORG-01 | Labs with floor plans and seats; nodes are placed in seats. | P0 | The map matches the room. |
| ORG-02 | Pools: named sets of nodes. A node can be in several pools. | P0 | |
| ORG-03 | Free-form labels on nodes, set on one node or in bulk. | P0 | |
| ORG-04 | One selector language used everywhere (jobs, bulk actions, views, policies), for example `lab=A state=idle cores>=12 !broken pool=night`. | P0 | It replaces `--only`, and one selector targets 150 PCs. |
| ORG-05 | Caps per node, pool or lab: maximum threads, CPU %, RAM, temperature (throttle above it) and allowed hours. | P0 | A capped PC never exceeds its cap, sampled every second over a 1-hour run. |
| ORG-06 | Reserve rules: keep N PCs or X% free, per lab or fleet-wide. The scheduler never breaks a reserve. | P0 | With "keep 10 free in Lab A", a full-fleet job leaves exactly 10 Lab A PCs idle. |
| ORG-07 | Windows (timetables) per lab saying what the cluster may use at which hours; importable from CSV. | P1 | At 09:00 on Monday, Lab B drops to idle-only by itself. |
| ORG-08 | Reservations on a calendar (exam, class event, maintenance). | P1 | |
| ORG-09 | Node states: cordon, drain, maintenance and offline, each recording the reason and who set it. | P0 | A drained PC finishes its tasks and then takes no more. |
| ORG-10 | Capacity planner: the cores available in a time range after windows, reserves and reservations. | P1 | It answers questions like "Friday 18:00-06:00: 2,160 cores". |
| ORG-11 | Config as code: export the whole fleet setup (labs, pools, labels, caps, windows, policies) as JSON; import it with a diff preview. | P1 | One college's setup can be applied at another college. |

## SCH: scheduler

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| SCH-01 | A job queue with several jobs running at once; whole-node allocation by default. | P0 | Two jobs run side by side on disjoint PCs. |
| SCH-02 | Priority classes: urgent, high, normal, low, background. | P0 | |
| SCH-03 | Admission and estimates: before a job starts, show when it can start and how long it should take, from measured rates. | P1 | Estimates are within 15% for SHA jobs. |
| SCH-04 | Fair share between users and groups, with usage decaying over a 7-day half-life; quotas in core-hours. | P1 | |
| SCH-05 | Placement modes: spread (across labs and switches), pack (fill lab by lab so whole labs stay free), and labels/affinity. | P1 | |
| SCH-06 | Chunk policies, chosen per job: `fixed` (today's behaviour, the paper's baseline); `guided` (shrinks toward the end); `adaptive` (chunk = node rate x target time, guided at the tail). | P0 | All three are tested on the pure core, and E2 compares them. |
| SCH-07 | Rate estimates per node and per workload (exponential average), used by placement, adaptive chunks and estimates. | P0 | |
| SCH-08 | Speculative tail: when the queue is empty and a task has run 2x longer than expected, run a copy on an idle PC. The first result wins and the other copy is cancelled. | P1 | The slow-PC tail shrinks in E1 at 200 PCs. |
| SCH-09 | Preemption: a higher-priority job takes PCs from preemptible jobs after a grace time, and their tasks go back to the queue. | P1 | |
| SCH-10 | Students first: when someone uses a PC (its logind session is not idle, or non-cluster CPU load passes a threshold), the agent drops to the window's cap or pauses within 2 s. It resumes after 5 idle minutes. | P1 | E6 |
| SCH-11 | Backfill: small jobs run in the gaps while a large job waits for its minimum number of PCs. | P2 | |
| SCH-12 | Job dependencies (run B after A). | P2 | |
| SCH-13 | Sharing one PC between jobs by threads. | P2 | |

## JOB: general-purpose job model

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| JOB-01 | Input spaces: an integer range (as today) or a list of items as JSON lines (parameter sets, file names, frame numbers). | P0 | A 10,000-item parameter sweep runs as one job. |
| JOB-02 | Jobs live on the head, not in the client. Submit returns a job ID; `ck job ls`, `watch`, `pause`, `resume`, `cancel` and `result` work from any PC; results are stored on the head. | P0 | Closing the laptop that submitted a job does not stop the job. |
| JOB-03 | Programs: a command present on every PC (as today), a pushed script (as today), a bundle (JOB-05), or a built-in kernel. | P0 | |
| JOB-04 | Program contract. Input: `START END` (ranges) or the item on stdin. Environment: `CK_JOB`, `CK_TASK`, `CK_ATTEMPT`, `CK_THREADS`, `CK_SCRATCH`. Output: stdout is the result. Exit codes: 0 = done, 75 = retry, anything else = failed attempt. Programs must be idempotent. | P0 | Documented in docs/JOBS.md with examples in C, Python and Node. |
| JOB-05 | Bundles: a folder (program plus data) packed, addressed by its SHA-256, cached on each PC and fetched once. Replaces sending the `--push` script with every chunk. | P0 | A 50 MB bundle reaches each of 200 PCs once, not once per task. |
| JOB-06 | Limits per job: per-task timeout, maximum attempts, maximum output size, and fail-fast or best-effort. | P0 | |
| JOB-07 | Reducers on the head: concat (as today), sum, count, sort, top-k, or keep the files. | P1 | The `primes` example needs no `awk`. |
| JOB-08 | Output files uploaded to the head's blob store, listed and downloadable per job. | P1 | |
| JOB-09 | Built-in kernels: SHA-256 proof of work (as today); synthetic (sleep or spin for a set time, for tests and simulation); Monte Carlo pi. | P0 | |
| JOB-10 | Python jobs with declared packages: the agent builds the environment with uv from a wheel cache on the head, with no internet. | P2 | |
| JOB-11 | Result validation: re-run a sample of tasks on another PC and compare, to catch faulty hardware. | P2 | |
| JOB-12 | Job templates in the console: benchmark, parameter sweep, render frames, soak test. | P1 | |

## HA: failures and the head

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| HA-01 | Liveness both ways: a heartbeat every second and read timeouts. A silent worker is lost after 5 s and its tasks are requeued; when the head goes silent, agents look for the next head. | P0 | Pulling a cable requeues that PC's tasks within 10 s, and the job result is exact. |
| HA-02 | Durable head state: jobs, progress (completed ranges as interval sets), results, nodes and policies are in SQLite, and a restarted head resumes its jobs. | P0 | After `kill -9` of a head with no standbys and a restart, the job finishes with the exact result. |
| HA-03 | Standby heads (chosen by an operator; default 2, in different labs) receive the ordered change log. The console shows their lag. | P0 | Lag stays under 1 s at 200 PCs. |
| HA-04 | Agents keep each result until the head confirms it is durable (written locally and on one standby, if any). After a failover they resend; duplicates are dropped by (job, range, generation). | P0 | E4: 0 lost, 0 duplicates. |
| HA-05 | Election: when the primary has been silent for 5 s, standbys try in priority order. A standby promotes itself only if it reaches a majority of the nodes in the last replicated membership list. Each promotion increases the epoch by 1. | P0 | A network-split test never produces two primaries. |
| HA-06 | Epoch fencing: every message carries the epoch and messages from lower epochs are refused. An old primary that comes back becomes a standby. | P0 | |
| HA-07 | Planned switchover from the console ("make PC A-07 the head") with no lost work. | P1 | Dispatch pauses for under 3 s. |
| HA-08 | Relays (D7), one per lab or subnet: forward discovery, batch task requests, cache bundles, and keep results until they are acknowledged. Agents fall back to the head when a relay dies. | P1 | The head sees about 6 relay connections instead of 200 agents. |
| HA-09 | Chaos script: kill agents, kill the head, partition a lab, slow a PC. It runs on one machine with virtual nodes and on the real fleet, and prints pass/fail. | P0 | It runs in CI. |

## NET: network

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| NET-01 | RTT to the head, packet loss and NIC speed per node; traffic per lab. | P1 | |
| NET-02 | Bandwidth probes from the console (head to node, node to node). | P1 | |
| NET-03 | Bundle distribution through relays and peers: a PC that has a bundle serves it to its lab, so each file crosses a lab's uplink once. | P1 | |
| NET-04 | Batching: several tasks per message when tasks are short; the measured dispatch rate is shown. | P1 | |
| NET-05 | Traffic caps per job (MB/s) for data-heavy jobs. | P2 | |
| NET-06 | Wake-on-LAN and power windows, for example "wake Lab B at 18:00, shut idle PCs down at 06:00". | P1 | Depends on WoL being enabled in the BIOS (Q4). |
| NET-07 | Topology view: subnets, relays, head and standbys, with links coloured by RTT and loss. | P1 | |

## OBS: monitoring and the smart parts

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| OBS-01 | Metrics per node every second: CPU (total and ours), RAM, temperature, network, task rate. | P0 | |
| OBS-02 | Fleet numbers: PCs online and total; cores used, free and reserved; utilisation; throughput; jobs running and queued. | P0 | |
| OBS-03 | Energy: CPU package energy from RAPL where it is readable, otherwise a per-class power model calibrated with a wall meter. Reports energy per job. | P1 | |
| OBS-04 | Events and audit log: joins, losses, state changes, failovers, and every admin action with who did it and when. Searchable. | P0 | |
| OBS-05 | Alerts in the console: lab offline, PC lost, overheating, job stalled, reserve broken, head failover. | P1 | |
| OBS-06 | Advisor: explainable suggestions with one-click apply. Examples: a PC slower than its identical peers (below the peer median minus 3 MAD; the paper's slow head node is exactly this case); a flaky PC to quarantine; chunks too small (head too busy); a reserve about to break; a hot lab; a bad network port. | P1 | Each suggestion links to the data that triggered it. |
| OBS-07 | Reports: core-hours delivered per lab, user and month; utilisation; energy. | P2 | |
| OBS-08 | A `/metrics` endpoint in Prometheus format. | P2 | |

## EXP: experiments

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| EXP-01 | Experiment runner: a matrix (node counts, policies, chunk sizes, workloads) times repeats, interleaved, with warm-up, run unattended. | P0 | E1 runs overnight with nobody present. |
| EXP-02 | Provenance for each run: git commit, binary hash, node list and classes, temperatures, policy, start and end times. | P0 | |
| EXP-03 | CSV export in the format of docs/paper/data/raw.csv, so docs/paper/figs.py draws the figures. | P0 | |
| EXP-04 | Fault scripts inside experiments: at time t, kill k agents, the head or a whole lab. | P1 | Used by E3 and E4. |
| EXP-05 | Runs on virtual nodes or shared machines are labelled as such and never mixed with real-PC results. | P0 | |

## SIM: simulating other architectures

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| SIM-01 | Virtual nodes: `ck agent --virtual 200 --synthetic` runs 200 logical agents (each with its own ID and connection) in one process, with set speeds, jitter and failure rates. | P0 | One laptop tests the head against 1,000 agents. |
| SIM-02 | Discrete-event simulator on the same scheduler core: a JSON scenario (nodes, speeds, network delays, failures, architecture) goes in, CSV comes out. | P1 | 10,000 nodes x 1 hour simulates in under a minute. |
| SIM-03 | Architectures to compare: single head; primary plus standbys; relays per lab; relays plus standbys. | P1 | |
| SIM-04 | Validation: the simulator must reproduce the measured E1 points within 10% before its numbers are used anywhere. | P1 | |
| SIM-05 | Simulation Lab page: pick a scenario, draw the topology, run it, compare results side by side. | P2 | |

## SEC: security and privacy

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| SEC-01 | Users, roles, sessions, login throttling and API tokens, ported from 6eca566. | P0 | |
| SEC-02 | A per-node credential issued at approval; an HMAC challenge on connect; a MAC with a sequence number on every frame, so frames cannot be replayed. | P0 | A PC without a credential, or a modified frame, is refused. |
| SEC-03 | Commands and scripts are accepted only from authenticated users with the right role. Every job records its submitter and the program's hash. | P0 | |
| SEC-04 | The agent runs as an unprivileged `ck` user in a systemd unit with `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `Nice=19`, `CPUSchedulingPolicy=idle`, `MemoryMax` and `CPUQuota`. | P0 | A job cannot read students' home folders. |
| SEC-05 | Fleet-key rotation; revoking a node. | P1 | |
| SEC-06 | An isolated VLAN, or firewall rules that limit ports 7700-7702 to the cluster subnets (documented, and applied by `kit`). | P0 | |
| SEC-07 | Privacy: idle detection uses only idle time and session state, never keystrokes, screens or files. This is stated in the docs. | P0 | |
| SEC-08 | TLS for the console, behind a documented reverse proxy. | P2 | |

## OPS: running the fleet

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| OPS-01 | Install: `ck agent install --head HOST --code CODE` installs to `/opt/ck` and writes the systemd service. | P0 | 40 PCs are installed from one USB drive in under 30 minutes. |
| OPS-02 | Rolling upgrade from the console: a canary (5%) first, then the rest, with automatic rollback on failure; version skew is shown. | P1 | |
| OPS-03 | Remote actions: restart an agent, collect logs, run diagnostics (calibration, ping, bandwidth). | P1 | |
| OPS-04 | Backups: a nightly copy of the head database, with a tested restore. | P1 | |
| OPS-05 | Offline build and install: vendored crates, console assets inside the binary. | P0 | Builds on a PC with no internet. |
| OPS-06 | Windows agent, running as a service at Idle priority class. | P1 | |

## UI: console pages and design

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| UI-01 | Overview: fleet numbers (OBS-02), a mini map, a 24-hour utilisation chart, active alerts, recent events. | P0 | |
| UI-02 | Floor map (SVG), described below the table. | P0 | 1,000 tiles pan and zoom at 30+ fps on a lab PC. |
| UI-03 | Nodes table and node detail, described below the table. | P0 | |
| UI-04 | Labs and pools: floor plans, seats, pools, labels and caps (P0); windows as a timetable grid, reservations on a calendar, the capacity planner (P1). | P0 / P1 | |
| UI-05 | Jobs: the queue (drag to reorder within a priority), a submit wizard, and a job detail page, described below the table. | P0 | |
| UI-06 | Policies: chunk policy, fair share, quotas, preemption, reserves, failover (standbys, timeouts). | P1 | |
| UI-07 | Network: topology, RTT and loss heat map, probes. | P1 | |
| UI-08 | Experiments: matrix builder, runs, charts (speed-up, efficiency, Karp-Flatt), CSV export. | P1 | |
| UI-09 | Simulation Lab (SIM-05). | P2 | |
| UI-10 | Events and audit, alerts, users and tokens, settings. | P0 | |
| UI-11 | Wall mode for the press event: full screen, big numbers, the live map, and a recovery timer when a PC or the head fails. No controls. | P0 | |
| UI-12 | Power-user features: a command palette (Ctrl/Cmd+K) for navigation and actions, keyboard shortcuts, URLs that hold filters (shareable), an undo window on bulk actions, a dry-run preview of policy changes. | P1 | |
| UI-13 | Live updates over SSE within 2 s, with no polling storms. | P0 | |
| UI-14 | Quality bar, listed below the table. | P0 | Checked on every page before a milestone closes. |

UI-02 floor map:

- Labs are drawn as rooms and PCs as tiles in seats.
- Edit mode: drag, grid snap, auto-arrange rows (by IP or name), copy a layout.
- Overlays: state, CPU, temperature, throughput, job, RTT, hardware class.
- Box-select with bulk actions; zoom, pan and a mini-map.
- A tile flashes when it finishes a task.

UI-03 nodes table: sortable, filtered with the selector language, configurable columns, saved views, and bulk actions with a preview. The node detail drawer shows inventory, live charts, tasks, events and actions.

UI-05 jobs: the submit wizard steps through template, program, input, selector, policy and limits, then a review page with an estimate. The job detail page shows progress, each PC's share, the task timeline, failures, logs and results.

UI-14 quality bar:

- design tokens ported from 6eca566;
- dark and light themes;
- colour-blind-safe states (shape plus colour);
- WCAG 2.1 AA, and everything reachable by keyboard;
- a table alternative to the map;
- works at 390 px with no sideways scroll;
- no console errors.

## API: interfaces

| ID | Requirement | P | Done when |
| --- | --- | --- | --- |
| API-01 | REST under `/api/v1` with one error shape `{"error": {"code", "message"}}`, cursor pagination, and SSE at `/api/v1/events`. | P0 | |
| API-02 | A `ck admin ...` CLI covering every API action, authenticated with an API token. | P0 | A test lists the routes and fails if any route has no CLI command. |
| API-03 | docs/API.md, generated from or checked against the route table. | P1 | |
| API-04 | A Python client (`submit`, `watch`, `results`). | P2 | |

## NFR: non-functional targets

| ID | Target | P |
| --- | --- | --- |
| NFR-01 | 500 agents on one head without relays (P0); 2,000 with relays, emulated (P1). | P0 / P1 |
| NFR-02 | The head dispatches at least 5,000 task round trips a second on a lab PC, measured with virtual nodes. | P0 |
| NFR-03 | SHA-256 at 200 PCs reaches at least 90% of the summed stand-alone rates. | P0 |
| NFR-04 | An idle agent uses at most 0.5% CPU and 30 MB of memory. | P0 |
| NFR-05 | A lost worker is detected within 10 s (P0); a head failover resumes dispatch within 15 s (P0). | P0 |
| NFR-06 | Console API p95 latency at most 200 ms with 200 PCs. | P0 |
| NFR-07 | With the whole fleet loaded, a foreground benchmark on a lab PC slows by at most 10% (E6). | P1 |
| NFR-08 | Ubuntu 22.04 to 26.04 for the head and agents (P0); Windows 10/11 agents (P1); macOS for development. | P0 / P1 |
| NFR-09 | Maintainability: modules of about 800 lines at most, each with a header comment; `cargo fmt`, `clippy -D warnings` and tests run in CI on Linux, Windows and macOS; every dependency is listed with its reason in docs/ARCHITECTURE.md. | P0 |

---

# Part C: work plan

Week 1 starts on Monday 2026-11-09. Each item names its requirement IDs, and it closes only when its "done when" check passes and the evidence is recorded.

Suggested split for five people (put names in):

1. Scheduler core and the head.
2. Agent, protocol and security.
3. Console UI and UX.
4. High availability, network, relays and the simulator.
5. Experiments, monitoring and the paper.

## M0 Foundations (weeks 1-2, until 2026-11-22)

- [x] Remove the video system (2026-11-04).
- [x] Split main.rs into modules (proto, head, agent, client, kernels) with no change in behaviour: same 24 hits, 31.1 M/s before and after (2026-11-04).
- [x] HA-01 liveness and timeouts (1 s heartbeat, 5 s loss), with the cable-pull test (`kill -STOP`) in scripts/chaos.sh. `--spin` has no timeout yet.
- [x] Protocol v2: a version in HELLO, JSON control frames reserved, FLT-01 node ID, FLT-02 inventory.
- [x] SIM-01 virtual nodes (`ck agent --virtual N`) and the synthetic kernel (`ck run --synth NS`, JOB-09).
- [ ] Security and install basics:
  - [x] SEC-02: library (auth.rs) in M0, wired in M1 wave 3;
  - [ ] SEC-03 command authorisation (M1, needs users);
  - [x] SEC-04 the systemd unit;
  - [x] OPS-01 the install command (`ck install`; dry-run checked, real install not yet run on Ubuntu).
- [~] CI (NFR-09): .github/workflows/ci.yml written; it has never run, because nothing has been pushed.
- [x] Console scaffold (D8): SvelteKit SPA in console/, embedded in ck by build.rs, Overview and Nodes pages on live /stats data.

## M1 Durable jobs and console v1 (weeks 3-6, until 2026-12-20)

- [x] HA-02 SQLite on the head (2026-11-04). Measured: `kill -9` of the head mid-job, restart, job finished with 22 hits, the same as an uninterrupted run.
- [x] JOB-02 jobs independent of the client: queue, `ck run --detach`, `ck run attach N`, `ck job ls/show/cancel/result`.
- [x] JOB-04 the contract and docs/JOBS.md (env vars, exit 0 / 75 retry / other failed); JOB-06 limits: 3 attempts per range, per-task timeout (the timeout path is unit-tested only).
- [x] API-01 HTTP and SSE on port 7702 (dash.html kept at /legacy); SEC-01 auth (bootstrap, login, sessions, tokens, roles, throttle); OBS-04 audit log.
- [x] Console pages: UI-01 Overview, UI-03 Nodes, UI-05 Jobs (list, detail, new), UI-10 Events and Users, with UI-13 live updates over SSE. Checked in a real browser against the real head.
- [ ] UI-14 quality pass: not yet checked at the viewer and researcher roles, on Users and Nodes, or in dark mode at desktop width.
- [x] UI-02 floor map v1 (place, auto-arrange, state/CPU/working overlays, box-select and act, table alternative); FLT-04 approvals and join codes; ORG-09 node states (drain then cordon checked live).
- [x] SEC-02 wired: join code -> per-node key; CHALLENGE/PROOF on reconnect; every frame sealed after admission; `CK_FLEET=open` for development and chaos.sh.
- [x] API-02 CLI parity: `ck admin ...`, `ck job ...`, `ck token`; a test fails when a route has no CLI command.
- [x] UI-11 wall mode v1 (checked at 1920x1080 against the real head). The recovery banner and tile flashes are not yet seen live.

## M2 Data-centre organisation (weeks 5-8, until 2027-01-03)

- [x] ORG-01..06: labs, pools, labels, selector language (`--selector`, console bar with live match count; `--only` kept for old clients), thread caps (agents run chunks on N threads), reserves (stable choice of held PCs) (2026-11-04).
- [x] SCH-01, SCH-02: several jobs at once through the pure scheduler core (sched.rs), five priorities, even split at equal priority. Measured: switching job waits for the PC's 2 in-flight chunks (adaptive chunk sizes in M3 shorten this).
- [x] FLT-03 hardware classes (`class` attribute); FLT-05 beacon carries the head address, `ck agent --relay`, remembered last head (relay not yet run across two real subnets).
- [x] ORG-11 config as code: export, import with a dry-run list of changes.
- [x] ORG-07 timetable windows (block or cap; CK_TZ_OFFSET_MIN, IST default) and ORG-08 bookings; a block window stopped its PC live.

## M3 Smart scheduling (weeks 7-10, until 2027-01-17)

- [x] Pure scheduler core (sched.rs); SCH-06 fixed/guided/adaptive with table tests (`--policy`); SCH-07 per-PC per-kind rates; FLT-06 3 s calibration at join (2026-11-05). Measured on one 10-core Mac: fixed and adaptive gave the same time (37.8 s vs 37.5 s) because the slowed virtual PCs were not slow enough; the real comparison needs E2 on lab PCs.
- [x] SCH-03 estimates (`eta_ms`, `/estimate`), SCH-08 speculative tail copies (first result wins; the losing copy is not cancelled yet).
- [ ] SCH-05 placement modes, SCH-09 preemption, SCH-04 fair share and quotas.
- [x] SCH-10 students first: presence probe (Linux logind, macOS HID idle, CPU fallback), busy at once / idle after 5 min, busy PCs get no new chunks; checked with CK_PRESENCE_FAKE. [ ] E6 pilot on 4 real PCs.
- [x] JOB-01 list jobs (`--list FILE`, console List kind), JOB-07 reducers (`--reducer`, result?reducer=). Measured: sum over 1..2000 = 2001000.
- [ ] JOB-05 bundles, JOB-08 output files, JOB-12 templates.

## M4 Resilience and architectures (weeks 8-13, until 2027-02-07)

- [x] HA-03 standbys and the change log; HA-04 durable results; HA-05 election; HA-06 fencing. (Tick 5: kill -9 of the primary at 51 % -> standby promoted in 5.7 s, exact result; network-split test with real PCs still to run.)
- [x] HA-07 planned switchover. (Settings: Make head, `ck admin switchover NODE`, POST /api/v1/ha/switchover. Mid-job on 10 virtual agents: exact 6138 hits; work resumed on the new head 3.7 s after the request, measured as first progress, so just above the 3 s target.)
- [ ] HA-08 relays: discovery first, then tasks and bundles. (Discovery relays exist. Tick 7, one Mac: one head serves 200 agents at 2,100 tasks/s, 10x what 200 PCs need, so task relays are deferred until a fleet above ~500 PCs.)
- [ ] HA-09 chaos script running in CI.

## M5 Simulation (weeks 9-14, until 2027-02-14)

- [ ] SIM-02 simulator, SIM-03 architectures, SIM-04 validation against real runs. (Tick 13: SIM-02/03 done as `ck sim`, 10,000 nodes x 1 h in 1.4 s, matches the local 200-agent point; SIM-04 needs the lab E1 data.)
- [ ] SIM-05 / UI-09 Simulation Lab page (P2).

## M6 Network and fleet operations (weeks 10-15, until 2027-02-21)

- [ ] NET-01, NET-02, NET-07 topology and probes; NET-03 bundle distribution; NET-04 batching.
- [ ] OPS-02 rolling upgrades; OPS-03 remote actions; OPS-04 backups.
- [ ] NET-06 Wake-on-LAN and power windows; FLT-07 identify.
- [ ] OPS-06 Windows agent.

## M7 Monitoring, advisor, experiments (weeks 9-16, until 2027-02-28)

- [ ] OBS-01, OBS-02 metrics and fleet numbers; OBS-03 energy; OBS-05 alerts; OBS-06 advisor.
- [ ] EXP-01..05: experiment runner, provenance, CSV for figs.py, fault scripts, labels. (Tick 9: EXP-01/02/03/05 in `scripts/experiment.py`; EXP-04 still open.)
- [ ] UI-06 policies, UI-07 network, UI-08 experiments, UI-12 power-user features.

## M8 200 PCs and the paper (weeks 14-20, until 2027-03-28)

- [ ] Inventory and map every PC.
- [ ] Install lab by lab, in steps of 16, 32, 64, 128 and 200 PCs.
- [ ] Run E1-E7 and write the results into the paper.

## M9 Demo and handover (weeks 19-22, until 2027-04-11)

- [ ] 24-hour proof-of-work soak, with a report (E7). (Tool ready: `python3 scripts/soak.py --head URL[,URL] --hours 24`, tick 8; the lab run is still to do.)
- [ ] Rehearse the press demo three times: pull cables, kill the head, reserve a lab live.
- [ ] Accessibility and performance pass on every page.
- [ ] Docs: README quick start, docs/ARCHITECTURE.md, docs/OPERATIONS.md (runbook for lab staff; first version tick 11), docs/JOBS.md, docs/API.md.

## Open questions (the answers change the plan)

| # | Question | Affects |
| --- | --- | --- |
| Q1 | Are all labs on one VLAN or subnet, or several? Which switches and uplink speeds? Can IT give us an isolated VLAN? | FLT-05, HA-08, SEC-06, NET-03 |
| Q2 | Which OS do the 200 PCs run (Ubuntu, Windows, dual boot)? Do we get admin rights to install a service? | OPS-01, OPS-06, SEC-04 |
| Q3 | When may the cluster run: only nights and weekends, or also during classes on idle PCs? Where is the lab timetable kept? | ORG-07, SCH-10 |
| Q4 | May PCs stay on overnight? Is Wake-on-LAN enabled in the BIOS? Are there wall power meters? | NET-06, OBS-03 |
| Q5 | Who submits jobs: only the team, or other departments too? | SCH-04, SEC-01, support load |
| Q6 | Which machines can be the head and the standbys: lab PCs, or a separate machine? | HA-03, HA-05 |
| Q7 | Real Bitcoin mining: is there written approval? The default is no (A9). | JOB-09 |
| Q8 | Dask engine: keep it, retire it, or replace it with a Python client for ck? | D14, API-04 |
| Q9 | Which paper venue, and what is the deadline? | Order of M3-M8 |

## Not in scope for this version

- Containers and virtual machines.
- Tightly coupled MPI-style jobs (gang scheduling).
- Internet or volunteer computing.
- A federation joining several colleges.
- Scheduling GPUs beyond labels.
- A Windows head.
