# Lab heads: the two-level hierarchy

One root head serves up to a few hundred PCs directly. A **lab head** is a second head, running in a lab, that serves that lab's PCs and talks to the root as if it were one big PC. The root then sees tens of connections instead of hundreds or thousands, and its CPU stops being the limit. Design and simulated numbers: [sim/HIERARCHY.md](sim/HIERARCHY.md) (assumptions A1-A12). Measured results on one machine: [HIERARCHY.md section 9](#9-measured-results).

It is off by default. A root with no lab heads behaves exactly as before, and old agents and old heads keep working.

## 1. How it works

```
        root head  (jobs, database, API, console, standbys)
          |  leases (big ranges)         ^  finished pieces, merged
          v                              |
     lab head A      lab head B      lab head C        <- `ck head --parent ROOT`
      |  |  |          |  |            |  |
     PCs of lab A    PCs of lab B     PCs of lab C     <- ordinary agents
```

1. **Lease.** The lab head connects up like a worker (`HELLO_W`, with `lab_head` set) and asks for work. The root gives it a **lease**: a range of `lease_s` seconds of the lab's measured speed (default 30 s), at most half of what is left per lab, never a sliver. Only jobs a lab head can serve are leased: no selector, no `--only`, no blobs, no `max_pcs`, placement `spread`, built-in kernels (`--sha`, `--synth`). Command, list and SDK jobs, and jobs with a selector or blobs, are not leased (see section 7).
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

All are tunables ([API.md](API.md): `PUT /api/v1/tunables`, `ck admin tune`) and also in one place: `PUT /api/v1/hierarchy/settings`, `ck admin hier settings KEY=VALUE ...`.

| Setting | Default | Meaning |
|---|---|---|
| `hier_mode` (`mode`) | `off` | `off`: no leases, every PC works for the root. `manual`: only lab heads you pin or start by hand get leases. `auto`: the root also picks the best PC of every lab with at least `lab_head_min_pcs` PCs (the head-suitability ranking of `ck admin heads`) and starts a lab head there. |
| `lease_s` | 30 | Seconds of a lab's work per lease. Shorter: less recomputed when a lab head dies (about 0.75 s of job time lost per second of lease, simulated), but a shorter outage or partition is hidden. Longer: the reverse. |
| `lab_head_min_pcs` | 8 | Auto mode: a lab gets a lab head when at least this many of its PCs are connected. |
| `lab_silence_s` | 90 | A lab head that goes silent (timeout, not a closed connection) keeps its leases this long, so it can finish and upload after a partition. After that the root takes them back. Too short: false takeovers in a long partition (work computed twice). Too long: a job's last lease waits this long if the lab head is truly stuck. |
| `lab_head_standby` | off | Reserved. A lost lab head's PCs fall back to the root instead. |
| `lab_head_port` | 7710 | First port for lab heads the root starts. |
| `lab_part_s` | 0.5 | How often a lab head sends finished pieces up. |

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

## 7. Limits (v1)

- Leased kinds: `--sha` and `--synth`. Command, list and SDK jobs, jobs with `--selector`, `--only`, blobs, `max_pcs` or a placement other than spread are served by PCs connected to the root; while one is running the hierarchy pauses. Reason: their programs read blobs from, and write output files to, the head that owns the job; carrying that through a lab head (the lab head as first blob source) is not built.
- No standby for a lab head (`lab_head_standby` is reserved). Losing one costs about 2 s plus the in-flight chunks.
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
| `kill -9` a lab head mid-job (3 labs x 40 PCs) | +0.6 s (a first version without PC-count-aware chunks: +47 s, see FINDINGS); exact. Simulated: +2.3 s. |
| Root frozen (SIGSTOP) 30 s, 90 s job | off: +31.0 s. on: -0.01 s (labs hold one to two leases = 30-60 s of work). |
| `kill -9` the root, standby takes over (labs list both) | exact, 11.9 s against about 10 s ideal. |
| `hier_mode` off, manual, off, manual, off during one job | exact; PCs change owner within about 3 s each time. |
| Auto mode (lab of 18 PCs, min 4), drain, undrain, pin to another PC, unpin | lab head started by the root, PCs moved; every job exact. |
| E2: 600 virtual PCs, 0.1 s chunks (6,000 tasks/s asked) | root CPU 9.9 % and 24.5 % (two runs) off, 3.1-3.4 % on. Wall 20-24 s off, 26-34 s on. One Mac cannot saturate a root at 600 PCs, so no speed gain shows; the root CPU drop is the result. |

Not measured: more than 600 PCs, real network latency between lab and root, command/list jobs (not leased).
