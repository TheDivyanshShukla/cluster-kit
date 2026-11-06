# Audit round 3: bad-case matrix (2026-11-06)

Method: four test agents, one Mac, `CK_POWER_DRY=1`, secured fleets (join codes, node keys, sealed links) unless noted. Every
PASS checks an exact job result (SHA hit count, synth number count, or a byte-exact result). Scripts in `scripts/stress/`:

| Script | Covers | Run |
| --- | --- | --- |
| `netfault.py` | network faults through a userspace TCP fault proxy | `CK_BIN=... STRESS_DIR=... CK_PORT=9700 python3 -I scripts/stress/netfault.py all` |
| `diskfault.py` | storage and state faults (uses a 100 MB disk image for disk-full) | `... diskfault.py corrupt\|kill9\|diskfull\|rodata\|datadel\|blobcache\|standby\|backup\|retention` |
| `robust.py` | malformed input on every port, auth and roles | `... robust.py all` |
| `lifecycle.py` | job faults and head lifecycle | `... lifecycle.py <case>` (cases listed in the script) |

The final gate ran all four suites at once on the integrated build (commit 088cb37 plus the SDK fix in this round).

## Results

| Area | Cases | Result |
| --- | --- | --- |
| Network: slow (64 KB/s), latency, jitter, reset, flap, half-open, truncate, blackhole on agent links | 8 | PASS, exact; lost PCs rejoin 0.8-6.1 s after the link returns |
| One of six PCs blackholed | 2 | PASS; marked lost after 4.6 s (documented ~5 s), its chunks recomputed, exact |
| All PCs blackholed 30 s | 1 | PASS; job resumed, rejoin 2.8 s, nothing counted twice |
| Standby link to the primary blackholed | 1 | PASS; standby promoted, the old primary fenced itself and rejoined as standby, never two heads dispatching, exact |
| 50 MB blob over a 1 MB/s link, and with every transfer cut at 3-15 MB | 2 | PASS; resumed, sha verified on every PC |
| `ck run` client link flapping / blackholed 12 s | 2 | PASS; client re-attached, exact result printed |
| 200 virtual PCs through a link flapping for 2 min | 1 | LIMIT: head memory, fds and threads flat, 190 PCs back 3.3 s after restore; the job ran out of the test's time at 96 % because 200 virtual PCs oversubscribe this Mac (the same job without faults needs 487 s) |
| Database corruption: header, truncated file, zeroed and schema pages, WAL deleted/truncated/flipped, `-shm` garbage | 9 | PASS: corrupt files refused at start, WAL damage recovers and the job re-runs exact |
| Head `kill -9` 20 times during heavy writes (~570 results/s) | 1 | PASS; 0 lost, 0 duplicate parts |
| Head disk full mid-job, read-only data dir, standby disk full, standby db corrupt | 5 | PASS after fixes 4-7 below |
| Backup during load + restore; retention on 500k parts / 500k events | 3 | PASS; dispatch p50 559 ms before, 512 ms while pruning |
| Malformed frames, wrong keys, 2,000 idle connections, slow senders, forged beacons, path traversal, every route by role, login throttle, CSRF, out-of-range numbers, 500 SSE viewers | 120 | PASS; a background exact SHA job ran through all of it |
| Programs that segfault, exit 1, exit 75, hang, leave orphans, eat 1.8 GB, write 10 GB, print binary, are non-deterministic, poison items | 10 | PASS |
| SDK: exceptions, crashes, a hung function with `timeout_s` | 1 | PASS after fix 11 |
| Double failover, switchover under load, standby killed mid-snapshot, head restart without standby, cancel during failover, client killed mid-job, two heads on one data dir, job with zero PCs, 200 jobs from 5 users, 200 virtual PCs flapping every 3 s for 3 min | 10 | PASS; flap: head fds/threads flat, RSS 20 -> 19 MiB |

## Bugs found and fixed

| # | Bug | Fix |
| --- | --- | --- |
| 1 | **Security:** the TCP client path ran any SUBMIT, `cmd` jobs included, with no credentials on a secured fleet | `ck run` sends an API token (`CK_TOKEN`); `cmd`/list need operator, the rest researcher. `CK_FLEET=open` test fleets unchanged |
| 2 | A sender dripping one byte per second held a connection thread 55 s | one overall 10 s deadline for unauthenticated first frames (`proto::recv_by`) |
| 3 | Blob port and key proof accepted a 256 MiB unauthenticated first frame | capped at 4 KiB / 1 KiB |
| 4 | A result whose database write failed (disk full) counted as stored: job "done" with nothing stored | not ACKed, requeued; then (h1) dispatch backs off 2 s doubling to 60 s after 3 failed writes, resumes on the first good write |
| 5 | Corrupt `head.db` started anyway with a warning, then served 404s | refuses to start; `CK_IGNORE_CORRUPT=1` to rescue |
| 6 | Read-only data dir started, every write failed | refuses at start |
| 7 | Standby with an unusable copy died or ran on bad data | moves it aside and takes a fresh snapshot |
| 8 | Agent blob cache trusted by name and size: flipped bytes gave wrong results (4 of 4) | hashed once per process before connecting; bad files deleted and fetched again |
| 9 | Cancel and the chunk timeout never killed a running program or its children; hung programs held PCs forever | programs run in their own process group, killed on cancel, timeout, over-cap and exit |
| 10 | A stale range-cancel flag killed a re-sent chunk at once | cleared when a chunk starts |
| 11 | Two heads on one data dir both wrote the same SQLite file | `head.lock` file lock; the second exits |
| 12 | An idle standby was dropped every 5.2 s (it sent nothing back) | the standby answers the primary's heartbeat |
| 13 | SDK: a hung function hung the caller during local profiling | every local sample run is time-limited (30 s or `timeout_s` for item 0, ~3 s for the rest), then the work is planned as slow and goes to the cluster |

Follow-up in the same round (h1, k1): fencing without UDP beacons (agents carry a signed proof of the newest epoch they have seen;
an older head that sees it fences within ~2 s), join-code guessing throttled per (IP, node id), per IP and fleet-wide, standbys
run retention too, `CK_HEADS` implemented, a per-program memory cap (`job_mem_pct`), `CK_OUT` size enforced while running and
nested `CK_OUT` files.

## Known limits (not fixed)

- A 0-byte `head.db` with no WAL looks like a fresh install.
- `CK_DATA` deleted under a running head: results since then are lost at restart; the head warns every minute (`store.missing`).
- A blob cache file corrupted while the agent runs is only caught at the agent's next start.
- Closed: every program carries env `CK_RUN_ID=<unique>`; processes that left the group (`setsid`, double fork) are found by that tag (Linux /proc/*/environ, macOS `ps -E`, same user, scanned at most every 1 s while a program runs) and killed on cancel, timeout, over-cap and exit, and counted in the memory cap. Remaining limit: a program that clears its own environment (`env -i`) escapes.
- One thread per connection: with 2,000 idle connections open, the background job's worst time was 10.8 s (baseline 4.0 s).
- A standby-only partition (agents still reach the primary) causes a full failover (~5 s pause, no data loss).
- On a full disk the `store_write` alert itself cannot be stored; only the `store.backoff` / `store.recovered` log lines appear.
- Not testable on one Mac: real network splits between two PCs, clock skew, Windows agents, the Linux `/proc` memory path.
