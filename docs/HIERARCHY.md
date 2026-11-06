# Lab heads: the two-level hierarchy

One root head serves up to a few hundred PCs directly. A **lab head** is a second head, running in a lab, that serves that lab's PCs and talks to the root as if it were one big PC. The root then sees tens of connections instead of hundreds or thousands, and its CPU stops being the limit. Design and simulated numbers: [sim/HIERARCHY.md](sim/HIERARCHY.md) (assumptions A1-A12). Measured results on one machine: [HIERARCHY.md section 9](#9-measured-results).

It is off by default. A root with no lab heads behaves exactly as before, and old agents and old heads keep working.

**Who needs this page.** Operators: sections 2 to 5 (start, settings, console and CLI controls, failures). Researchers: nothing to do, jobs of every kind run the same with lab heads on or off (section 7). Reviewers: section 9 has the measurements and says what one machine cannot show.

**Why they exist.** A single head spends CPU on every result of every PC. With a few hundred PCs that is fine (see [BENCHMARK.md](BENCHMARK.md)); at thousands, or with very short chunks, the head becomes the limit (the advisor raises `head_hot`). A lab head takes that per-chunk work for its own lab and talks to the root in big, batched messages: leases down, merged pieces up. It also fetches job files once for the whole lab. The cost is one more process to watch and a recompute of up to a lease if it dies.

**Modes at a glance.**

| Mode (`hier_mode`) | Who gets leases | Who starts lab heads |
|---|---|---|
| `off` (0) | nobody; every PC works for the root | nobody |
| `manual` (1) | lab heads you pinned or started by hand | you (`ck head --parent`, or `ck admin hier pin`) |
| `auto` (2) | every lab with at least `lab_head_min_pcs` connected PCs | the root, on the best PC of the lab |

## 1. How it works

```
        root head  (jobs, database, API, console, standbys)
          |  leases (big ranges)         ^  finished pieces, merged
          v                              |
     lab head A      lab head B      lab head C        <- `ck head --parent ROOT`
      |  |  |          |  |            |  |
     PCs of lab A    PCs of lab B     PCs of lab C     <- ordinary agents
```

1. **Lease.** The lab head connects up like a worker (`HELLO_W`, with `lab_head` set) and asks for work. The root gives it a **lease**: a range of `lease_s` seconds of the lab's measured speed (default 30 s), at most half of what is left per lab, never a sliver. Every kind is leased: built-in kernels, command, list and SDK jobs, with blobs, a selector, `max_pcs` and a placement. Only `--only` (it names PCs the root alone knows) keeps a job at the root (section 7). For list and command jobs the TASK carries the job spec the lab needs: the items of the range (the lab stores them from the range's first index, so `CK_START`/`CK_END` and result ranges are the root's), the command, env, memory, chunk length, selector, placement, its share of `max_pcs` and the blob lists.
2. **Run.** The lab head starts a local job over the leased numbers and cuts it into normal chunks for its PCs with the same scheduler, chunking and retries as the root. It holds up to `pipeline` (2) leases and runs them one after another, so it always holds between one and two leases of work.
3. **Report.** Every `lab_part_s` (0.5 s) the lab head merges the pieces its PCs finished and sends them up (`PARTIAL`: same layout as a result). The root stores each piece, shrinks the lease, and ACKs it. One message covers many chunks, so the root's work does not grow with the number of chunks. When a lease is complete the lab head says `lease_done` and asks for the next.
4. **Duplicates.** The root de-duplicates by range exactly as it does for any result, so a repeated piece is harmless.

A lab head is a full head: it has its own database (`CK_DATA`), port (`CK_PORT`), API and console. It is not a standby of the root and it does not beacon; its PCs find it because the root tells them (`move_to`) or because you point them at it.

## 2. Starting one

By hand (manual mode, or any time):

```
CK_PORT=7710 CK_DATA=/var/lib/ck-lab-a ck head --parent ROOT_IP:7700 --lab A --no-local-worker
ck agent --head LABHEAD_IP:7710        # PCs of lab A; with CK_HEADS=ROOT_IP:7700 they fall back to the root if the lab head dies
```

`--parent` takes a comma list, root first, then its standbys (the first that answers wins, so a lab head follows a takeover). A secured fleet needs a join code for the lab head's first start (`CK_JOIN_CODE`), like any PC.

From the root (`hier_mode` manual or auto): the root asks a PC's agent to start the lab head for you (the same mechanism as a standby head: `start_lab_head` in agent.rs) and tells the lab's other PCs to move under it. The lab head listens on `lab_head_port` + 10 x (hash of the lab name modulo 50).

## 3. Settings

All are tunables ([API.md](API.md): `PUT /api/v1/tunables`, `ck admin tune`, console Settings). Five of them are also set in one place: `PUT /api/v1/hierarchy/settings` and `ck admin hier settings KEY=VALUE ...` accept only `mode`, `lease_s`, `lab_head_min_pcs`, `lab_silence_s` and `lab_head_standby`. `lab_head_port`, `lab_part_s` and `stage_timeout_s` are changed with `ck admin tune` or `PUT /api/v1/tunables` only. The console Lab heads page shows `lease_s`, `lab_head_min_pcs` and `lab_silence_s`, plus the mode. Writes need the admin role. Values are checked against the ranges below; an out-of-range value is refused.

| Setting | Default | Range | Meaning |
|---|---|---|---|
| `hier_mode` (`mode`) | `off` | 0 to 2 (`off`/`manual`/`auto` in the settings call) | `off`: no leases, every PC works for the root. `manual`: only lab heads you pin or start by hand get leases. `auto`: the root also picks the best PC of every lab with at least `lab_head_min_pcs` PCs (the head-suitability ranking of `ck admin heads`) and starts a lab head there. |
| `lease_s` | 30 | 1 to 3600 s | Seconds of a lab's work per lease. Shorter: less recomputed when a lab head dies (about 0.75 s of job time lost per second of lease, simulated), but a shorter outage or partition is hidden. Longer: the reverse. |
| `lab_head_min_pcs` | 8 | 2 to 100000 | Auto mode: a lab gets a lab head when at least this many of its PCs are connected. |
| `lab_silence_s` | 90 | 5 to 3600 s | A lab head that goes silent (timeout, not a closed connection) keeps its leases this long, so it can finish and upload after a partition. After that the root takes them back. Too short: false takeovers in a long partition (work computed twice). Too long: a job's last lease waits this long if the lab head is truly stuck. |
| `lab_head_standby` | off | 0 or 1 | Reserved. A lost lab head's PCs fall back to the root instead. |
| `lab_head_port` | 7710 | 1024 to 65000 | First port for lab heads the root starts. A lab's port is this plus 10 x (hash of the lab name modulo 50), capped at 65000; its API is port + 2. |
| `stage_timeout_s` | 120 | 10 to 86400 s | Applies to leases too. A chunk or lease reserved while its blobs download is re-staged from the head after this long plus 1 s per 2 MB of missing blobs; after the same time again it goes to another PC and the slow PC is skipped for that job. If the blob is gone from the head the job fails with a message naming it. |
| `lab_part_s` | 0.5 | 0.05 to 30 s | How often a lab head sends finished pieces up. Smaller: the root sees progress sooner and a dead lab head loses less, at more messages. |

Changing `hier_mode` is live. `off` recalls every lease at once (the lab heads drop them; the pieces already stored stay), tells each lab head to send its PCs back to the root, and the job carries on with the PCs connecting directly. `manual` or `auto` again sends the PCs back to their lab heads. It is safe in the middle of a job.

## 4. Controls

`ck admin hier` shows the tree. All writes are operator (pin, unpin, drain) or admin (settings).

| Command | API | What it does |
|---|---|---|
| `ck admin hier` | `GET /api/v1/hierarchy` | The tree: root, standbys, each lab with its lab head, PCs, leases in flight, granted vs done, lag, partitioned flag. |
| `ck admin hier settings mode=auto lease_s=60` | `PUT /api/v1/hierarchy/settings` | The settings above. |
| `ck admin hier pin LAB NODE` | `POST /api/v1/hierarchy/labs/{lab}/head` `{"node"}` | Promote: this PC hosts the lab head of LAB. If another PC hosts it now, that lab head hands its PCs back and exits, and the new one starts (this is also "move"). |
| `ck admin hier unpin LAB` | `DELETE /api/v1/hierarchy/labs/{lab}/head` | Demote. In auto mode the root chooses again; in manual mode the lab has no lab head started by the root. |
| `ck admin hier drain LAB [off]` | `POST /api/v1/hierarchy/labs/{lab}/drain` `{"on"}` | No new leases; when the held ones finish, the lab head sends its PCs back to the root and idles. `off` undoes it. |

Roles: `GET` is viewer; pin, unpin and drain are operator; settings are admin. Only the primary root runs the control; a standby shows the replicated settings. The tree (`GET /api/v1/hierarchy`) has `mode`, `active`, `paused` (set while a job that cannot use lab heads runs), `settings`, `root` (address, epoch, PCs served directly), `standbys`, `labs` and `orphans` (leases of silent lab heads and when the root takes them). Each lab has a `state`: `active`, `partitioned` (silent, leases kept), `released` (sent its PCs back: drained or mode off), `absent` (wanted but not started yet) or `none`, plus `pinned`, `drain`, `pcs_at_root` and the `head` (host, address, port, PCs, threads, leases done, numbers granted and done, report lag, unacknowledged pieces, rate).

### Console: Lab heads page

`/heads` in the console (nav "Lab heads"; also reachable from the command palette). It refreshes every 3 s and has two tabs.

- **Topology.** A mode switch (Off, Manual, Auto) with a one-line description of each; admins can click, others see it disabled with a note. Switching asks for confirmation and says it is live and safe in the middle of a job. Below it, one card per lab: lab head host, PCs under it, threads, speed, a lease progress bar (numbers done of numbers granted in the leases it holds, and how many the root counts in flight), leases finished, the age of its last report and any unacknowledged pieces. Badges show the state (active, cut off, draining, absent) and "pinned to HOST". A cut-off lab head shows how long it has been silent and when the root will take its leases (`lab_silence_s`). Operators get **Pin / Move** (opens a PC picker ranked by head suitability; picking another PC while one is pinned is the move), **Unpin**, and **Drain** / **Resume**. A banner explains when lab heads are paused for a job that cannot use them. The Live canvas also draws lab heads.
- **Settings.** `lease_s`, `lab_head_min_pcs` and `lab_silence_s` with their limits; admin only. Other settings: section 3.

SSE events (the `event` stream and the events log): `labhead.join`, `labhead.leave`, `labhead.lease` (live only), `labhead.partition`, `labhead.heal`, `hier.mode`, `hier.pause`, `hier.resume`, `hier.pin`, `hier.unpin`, `hier.drain`, `hier.recruit`, `hier.release`, `hier.move`.

The advisor ([ADVISOR.md](ADVISOR.md)) suggests turning lab heads on when the head is hot or PCs wait for it and labs are big enough, moves an unhealthy lab head, and tunes `lease_s` from observed losses and partitions.

## 5. What happens when something fails

| Failure | Behaviour | Measured |
|---|---|---|
| Lab head process dies (`kill -9`) | The root sees the closed connection and puts its leases back at once. The lab's PCs reconnect to the root (agents try the next address of `CK_HEADS`, or the beacon) within about 2 s and carry on. Pieces already uploaded are kept. At most the chunks in flight on those PCs are computed again. | section 9 |
| Lab head or its link stalls (timeout) | The root keeps its leases for `lab_silence_s` (`labhead.partition`). The lab head keeps computing, never takes more, and on reconnect resends every un-ACKed piece, then lists the leases it holds (`labhead.heal`); the root re-attaches them. If the silence outlasts `lab_silence_s` the root re-queues them: some work is computed twice, but the first result wins and nothing is counted twice. | section 9 |
| Root unreachable or frozen | The labs keep computing the leases they hold (one to two leases, about `lease_s` to 2 x `lease_s` of work), then idle. They reconnect to the root or a standby listed in `--parent`, resend, and list their leases. A root that restarts or a standby that takes over adopts the listed ranges (it cuts them out of its queue), so they are not given out twice. | section 9 |
| Root dies, standby takes over | Same as above; the new root has a higher epoch. A lab head refuses a root with a lower epoch than the highest it has seen (a stale root). Labs and PCs follow the new root. | section 9 |
| Lab head restarts | It cancels any local job left from before (the root has long re-granted those ranges), then rejoins empty. Its PCs, which fell back to the root, are moved to it again by the root within a few seconds. | |
| A job a lab head cannot serve is running | The hierarchy pauses (`hier.pause`, `paused` in the tree): lab heads get no leases and send their PCs back to the root, so the job gets every PC. It resumes when no such job has work left. | |

**Epochs and fencing.** A lab head has no epoch of its own. It obeys the root's: it records the highest epoch it has seen in the root's CTRL frames and drops a connection to a root below it. It never fences itself and ignores beacons, so a root failover cannot make a lab head stop. The root fences exactly as before.

**Never starts work it was not granted.** Local jobs exist only for leases the root sent. With the root gone a lab head finishes its leases and idles.

**Quorum.** PCs under a lab head count as connected to the root for the HA quorum rule (the lab head reports their ids), so a root with standbys keeps dispatching.

## 6. Running the paper experiments

Everything below runs on one machine with virtual PCs. Scripts used for the numbers in section 9 are in `scripts/stress/hier_bench.py`.

```
# 3 labs x 40 virtual PCs under 3 lab heads, root on CK_PORT=10001, hier on
CK_FLEET=open CK_AUTH=off CK_PRESENCE=off python3 scripts/stress/hier_bench.py scale      # E2: root CPU and tasks/s, off vs on
python3 scripts/stress/hier_bench.py labkill      # kill -9 a lab head mid-job
python3 scripts/stress/hier_bench.py rootstop     # SIGSTOP the root 30 s, off vs on
python3 scripts/stress/hier_bench.py rootkill     # kill -9 the root with a standby
python3 scripts/stress/hier_bench.py switch       # off -> auto -> off during a job
```

Each prints exact-result checks (SHA hit counts, synthetic counts) and the timings. For real PCs, start lab heads with `ck head --parent` on one PC per lab, point the lab's agents at them, set `hier_mode=manual`, and submit the same jobs (`ck run --sha ... --n ...`).

## 7. Job kinds, blobs, max_pcs, and limits

**Which jobs go through lab heads.** Everything except a job submitted with `--only` (that names PCs the root alone knows). Built-in kernels, command jobs, list jobs, SDK jobs (`ck.map`, `ck.sweep`, `ck.train`, plans), jobs with blobs, a selector, `max_pcs` or a placement all run under lab heads. A job with `--only` makes the hierarchy pause (`paused` in the tree, `hier.pause`) until it has no work left; then the PCs go back under their lab heads.

**Blob flow.** The root stages a lease's blobs on the lab head exactly as it stages a chunk's blobs on a PC: the lab head reports what it holds, the root sends `fetch` with a head or peer source, and the lease is reserved until they arrive. The lab head fetches each blob once; its store is then the first source for its lab's PCs, which also fetch from each other. Items that name shards carry per-item `@refs:` (the SDK writes `@refs:SHA,...|item` for tuples, dicts, `ck.train` items and batches that hold `ck.shard` handles), so a PC fetches only the shards of its own chunk plus the job-wide blobs. Measured with 8 shard handles of a 329 MB file: agents fetched 1317 MB and the root sent 1152 MB before this, 329 MB each after ([audit/ROUND4.md](audit/ROUND4.md)). If staging stalls, `stage_timeout_s` applies (section 3).

**`max_pcs` split.** `max_pcs` is divided by PC count between the root's own PCs and the labs (each side at least one). With fewer `max_pcs` than labs only the fastest labs get leases. A job therefore holds about `max_pcs` PCs, at most one per lab more.

**Known limits.**
- No standby for a lab head (`lab_head_standby` is reserved). Losing one costs about 2 s plus the chunks in flight on its PCs.
- A PC that only ever joined through a lab head gets a minimal node row on the root (id and host from the lab head's report, state active, hardware `via lab`, no inventory, no identity of its own). PCs that also joined the root directly keep their full row. Live CPU, presence and per-PC rows are thinner than for direct PCs; the lab head's report supplies PC ids, threads and speed.
- Leases are sized by the whole lab's speed, so a selector or `max_pcs` that matches only a few PCs of a big lab gives long leases and a slow tail.
- Wall-clock gains need a real network and a real lab; see section 9 for what one machine cannot show.

More details of v1:
- **CK_OUT files and bulk results** are uploaded by the PCs straight to the root with the job's upload capability (the lab head hands it to them in `job_blobs`); the lab head forwards the file list with the finished piece, one piece per chunk that has files, so the root stores `job_files` exactly as without lab heads.
- **Errors and retries** run in the lab head: exit 75 and timeouts are retried by the shadow job like at the root. A chunk that still fails is reported to the root as that chunk (`chunk a..b failed: ...`), final, and the root fails the job with the same message. Cancel is the root's CANCEL, passed down to the shadow jobs.
- **Selector.** The lab head filters its own PCs; a lab with no matching PC declines the lease (`lease_decline`) and is not offered the job again. **max_pcs** is split by PC count between the root's own PCs and the labs (each side at least one; with fewer `max_pcs` than labs only the fastest labs get leases), so a job holds about `max_pcs` PCs, at most one per lab more. A lease is still sized by the whole lab's speed, so a selector that matches a few PCs of a big lab makes long leases.
- A lab head cuts leases with its own chunk size (the root's `chunk` and policy are not forwarded).
- The control (auto, pin, drain) runs on the primary root only. Pins, drains and settings replicate to standbys with the other settings.
- Virtual PCs are not chosen as lab heads in auto mode (set `CK_HIER_ALLOW_VIRTUAL=1` for tests).

## 8. Files

`rust/src/lab.rs` (lab head side, unit tests for merging and batching), `rust/src/hier.rs` (root side: leases, orphans, controller, API data), `rust/src/head.rs` (lease sizing `lease_len`, `PARTIAL`, `shrink_lease`, orphan handling), `rust/src/agent.rs` (`move_to`, `start_lab_head`, fallback), `rust/src/proto.rs` (`PARTIAL`, `Hello.lab_head`, CTRL keys), `rust/src/sim.rs` (`hier_model`), `rust/src/advisor.rs`.

## 9. Measured results

One Mac, virtual PCs (`--virtual`), sleep-based synthetic jobs, `scripts/stress/hier_bench.py`. Machine load varies, so read differences, not decimals.

| Experiment | Result |
|---|---|
| Exactness | SHA 1.5e8 hit count (2298) identical with hierarchy off and on; every synthetic run counted exactly N. |
| `kill -9` a lab head mid-job (3 labs x 40 PCs) | +0.6 s (a first version without PC-count-aware chunks: +47 s); exact. Simulated: +2.3 s. |
| Root frozen (SIGSTOP) 30 s, 90 s job | off: +31.0 s. on: -0.01 s (labs hold one to two leases = 30-60 s of work). |
| `kill -9` the root, standby takes over (labs list both) | exact, 11.9 s against about 10 s ideal. |
| `hier_mode` off, manual, off, manual, off during one job | exact; PCs change owner within about 3 s each time. |
| Auto mode (lab of 18 PCs, min 4), drain, undrain, pin to another PC, unpin | lab head started by the root, PCs moved; every job exact. |
| E2: 600 virtual PCs, 0.1 s chunks (6,000 tasks/s asked) | root CPU 9.9 % and 24.5 % (two runs) off, 3.1-3.4 % on. Wall 20-24 s off, 26-34 s on. One Mac cannot saturate a root at 600 PCs, so no speed gain shows; the root CPU drop is the result. |

| Real workloads under lab heads (2 lab heads, 8 virtual PCs) | etl, cv, sweep, train all exact with lab heads on, also with a lab head killed at 10 s ([audit/ROUND4.md](audit/ROUND4.md)). |

Not measured: more than 600 PCs, real network latency or bandwidth between lab and root, a real 200-PC lab. On one machine every "network" is loopback, so a lab head cannot shorten transfers here; the root CPU and the failure behaviour are what these runs show.
