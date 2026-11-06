# Two-level hierarchy in the simulator (lab heads with leases)

Run: `echo '{"hier_study": 1}' > s.json && CK_SIM_OUT=docs/sim ck sim s.json` (about 25 s). It writes `hier_saturation.csv`, `hier_faults.csv`, `hier_partition.csv`, `hier_lease.csv` and `hier_ttl.csv`; seeds are fixed, so a rerun gives the same numbers. One run: `{"hier": {"pcs": 1000, "two_level": true, "lab_pcs": 50, "chunk_s": 1, "n": 1.8e6, "kill_lab": [3, 600]}}`. Code: the hierarchy section at the end of `rust/src/sim.rs` (`HScn`, `hsim`, `hier_study`).

**Status: a projection.** The relay/lab-head tier does not exist yet (HA-08 is open, D7). Only the head cost model is calibrated (VALIDATION.md); everything about leases is an assumption listed in section 2.

## 1. Model

- **Root:** one serial service thread, 379 us + 0.199 us per directly connected agent per message. It sees each lab head as one agent.
- **Lab head:** the same cost model over its own PCs, plus 20 us to merge each chunk result. It holds a pool of leases.
- **Lease:** a range of `L` seconds of the lab's work (L x the lab's summed speed). The lab asks for the next one when its pool falls under 0.5 L. The root caps a lease at half its unleased work per live lab, so the tail stays short. The lab sends one merged report per finished lease (root cost: a message + 2 us per chunk).
- **PCs:** 2 chunks in flight, speed spread CV 3 %, fixed chunks of `chunk_s`. Work is counted in PC-seconds.
- **Lab head dies:** its PCs notice after 1.5-2.25 s and connect to the root directly. Chunks in flight and the lease's finished results are lost. The root gives up on the lab after `ttl` of silence and puts its unreported leases back in the pool.
- **Root dies:** a standby takes over after 5.7 s (measured), or a person restarts it (300 s). Messages wait in the queue. Labs keep chunking what they hold.
- **Partition:** messages between one lab head and the root wait until the heal. The lab keeps computing; the report goes up at the heal. If the silence outlasts `ttl`, the root has already given the range away and the late report is thrown away.
- **1-level partition:** the lab's PCs cannot reach the root, so they finish the chunks they hold and then wait (results are sent at the heal).

## 2. Assumptions (all are fields of `HScn`)

| # | Assumption | Value |
| --- | --- | --- |
| A1 | Lab head costs the same per message as the root (379 us + 0.199 us per agent), single serial thread | calibrated for the root only |
| A2 | Merge cost at the lab head per chunk / report cost at the root per chunk | 20 us / 2 us |
| A3 | Lease length L; refill below 0.5 L; guided cap of half the unleased work per live lab | 30 s |
| A4 | Lab-to-root latency / PC-to-head latency | 0.5 ms / 0.3 ms |
| A5 | PC failover after a lab head dies: reconnect time | 1.5-2.25 s, then PCs stay on the root |
| A6 | Silence timeout `ttl`; lab heartbeat every 2 s (cost added to the root analytically) | 90 s |
| A7 | Lab-held results are lost with the lab head (no partial upload) | - |
| A8 | Standby promotion 5.7 s (measured); manual restart 300 s | - |
| A9 | Lease timers do not run while the root is down | - |
| A10 | Speed spread CV 3 %; no PC failures, no jitter in these runs | - |
| A11 | Throughput past 1,000 agents extrapolates the straight-line head cost | as in REPORT.md |
| A12 | No bandwidth model: a lease is a range, not data | - |

## 3. Results

### (1) Saturation (`hier_saturation.csv`)

Tasks per second (root CPU, busiest lab CPU); 12 s runs, rates after 3 s. The fleet asks for PCs / chunk_s tasks/s.

| chunk | arch | 200 PCs | 1,000 | 2,000 | 5,000 | 10,000 |
| --- | --- | --- | --- | --- | --- | --- |
| 0.1 s (asks 10 x PCs) | 1-level | 1,999 (84 %) | 1,731 (100 %) | 1,294 (100 %) | 728 (100 %) | 422 (100 %) |
| 0.1 s | labs of 40 | 1,999 (0.1 %, 21 %) | 9,998 (0.4 %) | 19,989 (0.8 %) | 50,019 (2.0 %) | 99,976 (4.2 %, 21 %) |
| 0.1 s | labs of 100 | 1,998 (0.0 %, 42 %) | 10,000 (0.2 %) | 19,992 (0.4 %) | 50,013 (1.0 %) | 99,970 (2.0 %, 42 %) |
| 0.5 s (asks 2 x PCs) | 1-level | 402 (17 %) | 1,734 (100 %) | 1,323 (100 %) | 728 (100 %) | 422 (100 %) |
| 0.5 s | labs of 100 | 401 | 2,011 (0.2 %, 8 %) | 4,021 | 10,060 | 20,134 (2.0 %) |
| 1 s (asks 1 x PCs) | 1-level | 205 (9 %) | 1,004 (58 %) | 1,359 (100 %) | 728 (100 %) | 422 (100 %) |
| 1 s | labs of 100 | 203 | 1,017 (0.2 %) | 2,033 | 5,087 | 10,199 (2.0 %) |

- **1-level:** the head saturates (100 %) at about 500 PCs on 0.1 s chunks, 1,000 on 0.5 s, and 2,000 on 1 s. Past that, throughput falls as the per-agent cost grows: 422 tasks/s at 10,000 PCs, which is 4 % of what 0.1 s chunks ask for.
- **2-level:** every shape is served in full (served share 100 %). The root sits at 1-4 % (lease messages and heartbeats); the lab head is the new limit: 21 % busy at 40 PCs and 42 % at 100 PCs on 0.1 s chunks, so 0.1 s chunks would saturate a lab head only above about 230 PCs per lab.

### (2) A fault in a 30-minute job, 1,000 PCs, labs of 50, 1 s chunks (`hier_faults.csv`)

| Case | Makespan | Added time | Work redone |
| --- | --- | --- | --- |
| 1-level, no fault | 1,801.4 s | - | - |
| 1-level, root killed at 10 min, standby | 1,805.9 s | +4.5 s | 0 |
| 1-level, root killed, manual restart 300 s | 2,100.2 s | +298.8 s | 0 |
| 2-level, no fault | 1,800.9 s | - | - |
| 2-level, **lab head killed at 10 min** | 1,803.2 s | **+2.3 s** | 1,575 PC-s computed twice (0.09 % of the job); 3,001 PC-s re-granted |
| 2-level, root killed at 10 min, standby | 1,800.9 s | **+0.0 s** | 0 |
| 2-level, root killed, manual restart 300 s | 2,071.0 s | +270.2 s | 0 |

- **Lab head loss** costs 2.3 s: its 50 PCs reach the root in about 2 s and keep working, so only the lease is lost.
- **Root loss with a standby** costs nothing at L = 30 s, because the labs hold more than 5.7 s of work.
- **A 300 s root outage** is hidden for only the length of the leases (about 30-45 s here): 270 s against 299 s.

### (3) 60 s partition of one 50-PC lab (`hier_partition.csv`)

Share of the lab's full speed that its PCs still finish inside the window (mean of 6 start times; 0.5 s chunks, ttl 90 s).

| | Work in the window | Wasted |
| --- | --- | --- |
| 1-level | **1.7 %** (two chunks per PC) | 0 |
| 2-level, L = 10 s | 18.4 % | 0 |
| 2-level, L = 30 s | 51.7 % | 0 |
| 2-level, L = 60 s | 84.6 % | 0 |
| 2-level, L = 120 s | 100 % | 0 |

The work is kept: the lab reports at the heal. With ttl shorter than the partition (10 s or 30 s), the root takes the leases back and 2,494 PC-s are computed twice (`hier_ttl.csv`).

### (4) Lease length (`hier_lease.csv`; 1,000 PCs, 1 s chunks, 30-min job)

| L (s) | Root CPU | Root msgs/s | Lab kill: added s (PC-s recomputed) | Root kill 5.7 s / 300 s: added s | Partition share |
| --- | --- | --- | --- | --- | --- |
| 2 | 1.35 % | 30 | +0.2 (176) | +1.6 / +296 | 5 % |
| 5 | 0.89 % | 18 | +0.5 (326) | +0.8 / +295 | 10 % |
| 10 | 0.74 % | 14 | +0.8 (578) | +0.0 / +290 | 18 % |
| 30 | 0.55 % | 11 | +2.3 (1,575) | +0.0 / +270 | 52 % |
| 60 | 0.40 % | 10 | +4.5 (3,076) | +0.0 / +240 | 85 % |
| 120 | 0.40 % | 10 | +9.0 (6,078) | +0.0 / +180 | 100 % |
| 300 | 0.38 % | 10 | +22.6 (15,080) | +0.0 / +0.6 | 100 % |
| 600 | 0.38 % | 10 | +45.1 (30,085) | +0.0 / +0.0 | 100 % |

- **Root load** is already tiny at 2 s leases (1.4 %), and the floor (0.38 %) is the lab heartbeats. Short leases buy nothing at this size.
- **Loss on a lab kill** grows linearly with L (about 0.75 s of makespan per second of L).
- **Hiding a root outage** needs L at least the outage: 5.7 s is hidden from L = 10 s on.
- **Hiding a partition** needs L at least the partition (and ttl above it).
- **Tail effect:** a lab head lost near the end of a job delays the finish until `ttl` runs out (a 120 s job with ttl 90 s waited for it). `ttl` trades this against false fences in a partition (`hier_ttl.csv`).
- **A middle value** of L = 30-60 s hides a standby takeover and loses under 5 s on a lab kill. That is a choice among assumed numbers, not a result.

## 4. What the lab must measure (before any of this is believed)

| Assumption | Measure |
| --- | --- |
| A1 lab head cost per message and per agent | run the root's head code as a lab head with 100 agents; same fit as VALIDATION.md. If a lab head is 2x slower per message, the 0.1 s / 100-PC lab (42 %) reaches 84 %. |
| A2 merge cost | time the result merge of 1,000 chunk results of a real job kind (list, synthetic, files). |
| A4 latency | ping root to lab head and PC to lab head on the real switches (labs may be in other subnets). |
| A5 fallback time | kill a lab head process; time until its PCs show as connected to the root. |
| A6 ttl and heartbeat | the heartbeat period and the missed-beat count that give no false fence on a busy lab head and detect a dead one. |
| A7 result loss | whether PCs may keep their chunk results for the lab head's successor, which would shrink the recomputed work. |
| A10 speed spread / failures | the per-PC speed CV; PC failure rate (REPORT.md section 5). |
| The whole curve | E2 with real lab heads: 200 PCs on 0.1 s chunks against 1 lab head and against 4; tasks/s and CPU of root and labs. |

Not modelled: lab head restart (the PCs stay on the root), PC failures inside the hierarchy, bandwidth and file transfer through the lab head, two simultaneous faults, unequal labs, lease stealing between labs.
