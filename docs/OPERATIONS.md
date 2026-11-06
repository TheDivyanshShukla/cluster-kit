# Operations runbook (lab staff)

How to run Cluster Kit on the lab PCs: set it up, keep it running, run the full-capacity day and the paper experiments, and recover when something breaks. Commands use `ck`, the binary at `rust/target/release/ck` (build it with `./kit setup rs`). On Windows, write `ck.exe`.

Ports: 7700 TCP (agents and `ck run`), 7701 UDP (discovery), 7702 HTTP (console and API). Open all three on the head PCs. The agents only need to reach the head; they open no ports. Set `CK_PORT` to move all three ports together.

## 1. First setup

1. **Choose the head and two standbys.** Use PCs that stay on and sit in different labs (Q6). The head keeps its database in `ck-data/` beside where it starts (`CK_DATA` moves it).
2. **Start the head:** `ck head --no-local-worker`. Its console is at `http://<head-ip>:7702`.
3. **Create the first admin.** Open the console. On a new head it asks for the first admin's name and password.
4. **Make a join code:** in the console (Nodes, Join codes), or on the head with `ck admin join-code 250 1440`. That gives 250 uses, valid for 24 hours. The code is shown only once. With `CK_AUTO=1` the PCs that join start active; otherwise they wait for approval.
5. **Join each lab PC once, then install the agent as a service** (low priority, restarts by itself):
   - **Join:** `ck agent --head <head-ip> --code <JOIN-CODE>`. When it prints `joined: node key saved to ...`, stop it with Ctrl+C. Run this as the same user the service will run as, so the service finds the saved key. The service itself does not carry the join code.
   - **Install:** `sudo ck install --head <head-ip>` on Linux.
   - **Check first (optional):** `ck install --head <head-ip> --dry-run` prints the service file: `Nice=19`, idle CPU scheduling, `MemoryMax=80%`.
6. **Approve the waiting PCs** in the console, or with `ck admin approve <id>...`. Then place them on the lab map (Map page, or `ck admin lab-add` and `ck admin place`).
7. **Start the standbys** on the two chosen PCs: `ck head --standby --head <head-ip> --code <JOIN-CODE> --no-local-worker`. The head's log shows `+ standby ... (sealed)`, and the console's Settings page lists each standby and its lag.

API tokens for scripts: `CK_USER=<name> ck token --head <head-ip>`. It asks for the password and prints a token; put it in `CK_TOKEN`. `soak.py` needs an operator token, `experiment.py` a researcher token.

## 2. Day to day

| Task | Console | Command line |
| --- | --- | --- |
| See every PC | Overview, Nodes, Map, Wall | `ck admin nodes` |
| Take a PC out gently (finish its work, then stop) | Nodes, Drain | `ck admin state draining <id>` |
| Stop sending work to a PC | Nodes, Cordon | `ck admin state cordoned <id>` |
| Keep PCs free for a class | Policies, Windows / Reservations | `ck admin window ...`, `ck admin reservation ...` |
| Limit threads on some PCs | Policies, Caps | `ck admin cap "lab=A" 2` |
| Submit and follow a job | Jobs, New | `ck run ...`, `ck job ls`, `ck run attach <id>` |
| Who did what | Events | `ck job show <id>` |

A student using a PC (keyboard or mouse in the last 5 minutes) makes it "in use": it gets no new chunks until it has been idle again for 5 minutes. Running chunks finish.

## 3. When something breaks

| What happened | What you see | What to do |
| --- | --- | --- |
| A lab PC dies or is unplugged | It turns "lost" after about 5 s; its chunks go to other PCs | Nothing. It rejoins by itself when it comes back. |
| The head dies | Wall banner "Head failed — standby taking over"; the first standby takes over in under 1 s when only the head program died (its PC refuses connections; `failover_refused_s`, 0.5 s), or after about 6 s of silence when the whole PC or its cable is gone; agents are back within 1-4 s after that; Settings shows the new term (epoch) | Nothing for the jobs: they finish on the new head with exact results. The agents and `ck run` follow the new head on their own. Repair the old head PC later: when it comes back, it hears the newer head and rejoins as its standby. |
| The new head waits ("candidate, waiting for quorum") | It knows more PCs than are connected | It waits for more than half of the PCs it knew about. PCs that never come back are dropped 60 s after the takeover. Bring the network back if a whole lab is cut off. |
| A head stops with "FENCED" | Two heads saw each other after a network split | Expected. The one with the older term rejoins as a standby by itself. |
| A PC is refused ("wrong key", "join code invalid") | Events: `node.denied` | Make a new join code and reinstall that PC's agent with `--code`. |

Test the failover on one machine any time with `scripts/chaos.sh`. It prints PASS or FAIL for 5 scenarios, including killing the head while a standby is running.

## 3b. Round 4 safeguards (what you will see)

- **Old head comes back without UDP beacons** (VLANs, one-PC tests): every head sends its epoch with a signed proof to agents; an agent
  repeats the newest proof in its HELLO. A head with a lower epoch that verifies it fences itself ("! FENCED: an agent proved ...") and
  restarts as a standby, within seconds of the first agent that knows the new head reconnecting. A brand-new PC that never met the new
  head cannot warn the old one. When beacons cannot be heard, give agents all heads: `CK_HEADS=ip:port,ip:port` (tried in turn).
- **Disk full**: after 3 failed result writes in a row, dispatch pauses (2 s, doubling to 60 s) so PCs do not recompute forever; log
  `store.backoff`, and `store.recovered` when a write works again. The alert row needs a writable database, so on a truly full disk
  rely on the log line (alert `store_write` is opened if any write fits).
- **Join-code guessing**: 5 failed attempts in a minute from one source address for one node id refuse that pair for 10 s, doubling up to
  15 min; 30 failed codes from one address, or 200 fleet-wide, refuse join codes for 30 s, doubling. PCs that already hold a key are only
  held back by their own failures. One `node.denied` event is logged when a block starts. Blocks are forgotten at head restart.
- **Standbys prune** events/parts with the same `events_keep_days` / `parts_keep_days` as the head (first run about 30 s after start).

## 4. The full-capacity day (proof of work)

`python3 scripts/soak.py --head http://<head>:7702,http://<standby1>:7702,http://<standby2>:7702 --hours 24 --out soak-<date>`

- It runs one SHA-256 proof-of-work job at background priority, so any class job or experiment gets the PCs first.
- It samples every minute and keeps going through a head failover.
- At the end it writes `report.md`: hash rate, PCs connected, PCs lost per hour, failovers, and energy.
- Energy comes from the CPU's own counters (RAPL) on Linux PCs. Many kernels let only root read them, and the service runs as a normal user, so expect "not readable" there and use wall meters (Q4).
- This is not Bitcoin mining (decision D13). Real mining needs written approval from the college (Q7).

## 5. Paper experiments

`python3 scripts/experiment.py --head http://<head>:7702 --pcs 1,2,4,8,16,32,64,128,200 --repeats 7 --n-per-pc 2e9 --out exp-e1`

- **What runs:** each PC count is run 7 times, interleaved, after one warm-up run. "8 PCs" is always the same 8 PCs.
- **What it writes:**
  - `raw.csv`, in the paper's format;
  - `runs.jsonl`, a record of what each run used (code version, which PCs and their CPUs, times).
- **Figures:** `python3 docs/paper/figs_scale.py exp-e1/raw.csv`.
- **Simulated PCs:** runs on simulated PCs (`ck agent --virtual N`) go to a `-virtual` folder and must not be reported as real.
- **Before you start:**
  - Put the night's PCs in a pool or label, and pass it with `--selector "pool=night"`.
  - Make sure no class reservation covers the time of the run.

## 5b. Lab heads (hundreds of PCs)

One root head saturates at about 500 PCs on 0.1 s chunks and 1,000 on 0.5 s (simulated; [sim/HIERARCHY.md](sim/HIERARCHY.md)). Past that, start one lab head per lab: `ck head --parent ROOT_IP:7700 --lab A` on a PC of the lab, `ck agent --head LABHEAD_IP:PORT` for the lab's PCs, and `ck admin hier settings mode=manual`. Or `mode=auto` and the root picks the best PC of each lab with at least `lab_head_min_pcs` PCs and starts it. `ck admin hier` shows who serves whom; `ck admin hier drain LAB` and `unpin` take a lab head out; `mode=off` returns everything to the root in the middle of a job. A dead lab head costs about 2 s and the chunks in flight; a frozen root is ridden out for one to two leases (`lease_s`, 30 s). Full guide: [HIERARCHY.md](HIERARCHY.md).

## 5c. The advisor

The console Advisor page (and `ck admin advice`) lists what the head thinks you should change, each with options, cost, risk and a check. Nothing runs unless you apply it or an admin sets a kind to `auto`. Each applied option is checked again after 10 minutes against what was predicted. Start by reading suggestions only; turn `auto` on per kind once its history looks right. Full guide: [ADVISOR.md](ADVISOR.md).

## 5d. Limits that stop a program, and the staged-chunk timeout

- **Memory.** `job_mem_pct` (80): a command program and all its processes are stopped when together they use more than 80 % of the PC's RAM. The chunk fails with a message naming `job_mem_pct` (it is a plain failure, not an exit-75 retry request). Lower it on PCs that people use, 0 turns it off. A program that leaves its process group escapes the check.
- **Output.** `out_max_mb` (64) caps stdout per chunk and `out_files_mb` (256) caps the `$CK_OUT` files per chunk; both are enforced while the program runs, which then stops. The advisor offers `out_max_mb` x2 when a job fails on it.
- **Blobs that do not arrive.** `stage_timeout_s` (120): if a chunk is reserved for a PC (or a lab head) and its files have not arrived after this long plus 1 s per 2 MB missing, the head sends them itself; after the same time again it gives the chunk to another PC and avoids the slow one for that job. If a file is gone from the head the job fails and says which.

## 5e. Checking a new cluster with real workloads

`scripts/stress/workloads.py` runs five real Python jobs (`examples/real/`: CNN training, SVM sweep, 5-million-row ETL, image pipeline, 1 GB text) locally and on a fleet and compares an exact digest; add `--fault agent9|agentstop|head9|cancel|multi` to hurt it on purpose. `scripts/stress/hier_bench.py` does the same for lab heads (`scale`, `labkill`, `rootstop`, `rootkill`, `switch`, `auto`). What was measured is in [audit/ROUND4.md](audit/ROUND4.md). On a real lab, run `workloads.py` against your head after an upgrade; a mismatch in a digest is a bug, a slower-than-local time is not (small jobs do not pay for the network).

## 6. Backups and moving the head

- **Everything is in `ck-data/head.db`** (SQLite). The standbys each hold a full copy that is replicated continuously, so losing the head PC loses nothing.
- **Saving the setup:** `ck admin config-export > setup.json` saves labs, the map, labels, pools, caps and reserves, but not users or jobs. `ck admin config-import setup.json` restores it, and `CK_DRY_RUN=1` shows the changes first.
- **Backup now:** `ck admin backup [PATH]` writes a consistent copy of `head.db` (SQLite `VACUUM INTO`, safe while the head runs; run it on the head PC with the same `CK_DATA`). With no PATH it writes `ck-data/backups/head-<unix-seconds>.db`.
- **Scheduled backups:** set `backup_every_h` (default 0 = off) and `backup_keep` (default 7); the head writes `ck-data/backups/head-<unix-seconds>.db` on that schedule and deletes the oldest beyond `backup_keep`. Copy that folder off the PC (another disk or share) yourself; a backup next to the database does not survive a dead disk.
- **A damaged database stops the head:** at start the head runs an integrity check. If it fails, the head refuses to start and says so; restore a backup (below). `CK_IGNORE_CORRUPT=1` starts it anyway on whatever is readable, for rescuing data only. A read-only `ck-data/` also stops it at start. A standby with a damaged copy moves it aside (`head.db.bad-<time>`) and takes a fresh copy from the head by itself.
- **Restore:** stop the head, replace `ck-data/head.db` with the backup (delete `head.db-wal` and `head.db-shm` next to it), start the head. Standbys with a newer copy will resync from it; if one refuses, delete its `ck-data/head.db` so it takes a full copy.
- **Retention:** a background thread on the head deletes events older than `events_keep_days` (default 30) and, if `parts_keep_days` > 0, result parts of jobs finished longer ago than that (the job row and its summary stay; default 0 = keep parts forever). Deletes run in batches of 2,000 rows every 10 minutes. Standbys are not pruned by the head; they keep their copy until pruned themselves when promoted.
- **Moving the head on purpose:** on Settings, an admin clicks Make head on a standby, or runs `ck admin switchover <node-id>`. The head waits until that standby has every change, hands over, and becomes its standby. Work pauses for about 4 s, and nothing is lost.

## 7. Rolling upgrade

1. Upgrade the standbys one at a time; wait until `ck admin ha` (or Settings, Heads) shows each one synced.
2. `ck admin switchover <upgraded-standby-node-id>`.
3. Upgrade the old head; it rejoins as a standby.
4. Upgrade the agents a lab at a time; each reconnects to whichever head answers.
5. Until every head runs the new build, the ha status lists the old ones under `mixed` and the log has `ha.mixed_versions`. That is harmless, but unsigned fence beacons are still accepted, so finish the upgrade the same day.


## Logs

Each process writes JSON lines to `ck-data/logs/<role>-YYYYMMDD.jsonl`. Read them with `ck logs --since 10m --kind node. [--follow]` or `GET /api/v1/logs` (operator). Turn chunk-level lines on with `ck admin tune log_chunks 1`, silence everything with `ck admin tune log_level 0`. Size and retention: `log_max_mb`, `log_keep`. Details: [LOGGING.md](LOGGING.md).

## 8. Tunables reference

Every adjustable value (generated from `rust/src/tune.rs`). Set one with `ck admin tune KEY VALUE` (`default` resets), `PUT /api/v1/tunables`, the console Settings page, or the environment variable `CK_TUNE_<KEY>` (which wins). Scope `agent` and `both` values are pushed to every agent; `head` values stay on the head. Out-of-range values are refused.

| Group | Key | Default | Min | Max | Unit | Scope | What it does |
|---|---|---|---|---|---|---|---|
| Liveness | `heartbeat_s` | 1 | 0.2 | 10 | s | both | How often head and agents send a heartbeat. |
| Liveness | `lost_after_s` | 5 | 1 | 120 | s | both | Silence after which a PC (or the head) counts as lost and its work moves elsewhere. |
| Dispatch | `pipeline` | 2 | 1 | 16 | chunks | head | Chunks each PC holds at once: one computing, the rest queued. |
| Dispatch | `target_chunk_s` | 1 | 0.05 | 60 | s | head | Adaptive chunks aim to take this long on the PC that gets them. |
| Dispatch | `max_chunk` | 50,000,000 | 1,000 | 1e12 | numbers | head | Largest chunk the head ever hands out. |
| Dispatch | `max_attempts` | 3 | 1 | 20 | tries | head | Tries per range before a job fails (failed program runs). |
| Dispatch | `max_events` | 2,000 | 100 | 100,000 | chunks | head | Finished chunks kept for the live task stream. |
| Head failover | `member_window_s` | 60 | 5 | 3,600 | s | head | PCs seen within this window count toward the majority a new head waits for; ones that never return drop out this long after a takeover. |
| Head failover | `noq_grace_s` | 10 | 2 | 600 | s | head | A head with standbys that has no majority for this long stops handing out work (split-brain guard). |
| Head failover | `standby_step_s` | 2 | 0.5 | 60 | s | head | Extra wait per place in the standby order, so standbys take over one at a time. |
| Head failover | `failover_refused_s` | 0.5 | 0 | 60 | s | head | Take over this long after the primary's PC refuses connections (its head process is gone, the PC is up), instead of waiting for silence. 0 = only silence. |
| Head failover | `ring_max` | 100,000 | 1,000 | 10,000,000 | changes | head | Changes kept in memory for standbys that fall behind; older gaps need a full copy. |
| Head failover | `switchover_wait_s` | 5 | 1 | 120 | s | head | Planned switchover: how long to wait for the standby to confirm. |
| Head failover | `client_failover_s` | 60 | 5 | 3,600 | s | agent | How long `ck run` keeps looking for a new head after losing one. |
| Dynamic heads | `target_standbys` | 2 | 0 | 5 | standbys | head | Standby heads to keep; a missing one is recruited automatically (also in Standby heads). |
| Dynamic heads | `recruit_tick_s` | 10 | 1 | 600 | s | head | How often the head checks whether a standby is missing. |
| Dynamic heads | `recruit_every_s` | 60 | 5 | 3,600 | s | head | At most one new standby request per this interval. |
| Dynamic heads | `recruit_retry_s` | 600 | 30 | 86,400 | s | head | A PC that was asked (or failed) is not asked again for this long. |
| Dynamic heads | `head_w_speed` | 1 | 0 | 10 | weight | head | Head choice: weight of the PC's measured speed (calibration). |
| Dynamic heads | `head_w_memory` | 1 | 0 | 10 | weight | head | Head choice: weight of RAM and cores. |
| Dynamic heads | `head_w_stability` | 2 | 0 | 10 | weight | head | Head choice: weight of staying connected (few losses in the last day). |
| Dynamic heads | `head_w_free` | 1 | 0 | 10 | weight | head | Head choice: weight of not being used by a person right now. |
| Power | `eco_tick_s` | 30 | 5 | 3,600 | s | head | How often eco mode looks for PCs to sleep or wake. |
| Power | `waking_for_s` | 120 | 10 | 3,600 | s | head | A PC that does not reconnect this long after a wake packet shows 'did not wake'. |
| Power | `wake_retry_s` | 60 | 5 | 3,600 | s | head | Wait before sending another wake packet to the same PC. |
| Power | `wake_max` | 64 | 1 | 10,000 | PCs | head | Most PCs woken at once for waiting work. |
| Power | `energy_every_s` | 10 | 1 | 600 | s | agent | How often agents report CPU energy (RAPL). |
| Presence | `presence_idle_s` | 300 | 10 | 86,400 | s | agent | A PC counts as free again after nobody touched it for this long. |
| Presence | `presence_probe_s` | 2 | 0.5 | 60 | s | agent | How often agents check keyboard and mouse activity. |
| Presence | `calib_s` | 3 | 0.5 | 60 | s | agent | Length of the speed test an agent runs when it joins. |
| Agent limits | `unacked_max` | 10,000 | 100 | 10,000,000 | results | agent | Results an agent keeps until the head confirms them. |
| Agent limits | `unacked_max_mb` | 256 | 1 | 65,536 | MB | agent | Memory for unconfirmed results; the oldest go first. |
| Agent limits | `out_max_mb` | 64 | 1 | 4,096 | MB | agent | Program output kept per chunk. |
| Agent limits | `push_max_kb` | 64 | 1 | 65,536 | KB | head | Largest script `ck run --push` may send. |
| API | `max_body_mb` | 1 | 0.01 | 1,024 | MB | head | Largest API request body. |
| API | `max_list_body_mb` | 64 | 1 | 4,096 | MB | head | Largest list-job upload (signed-in operators only). |
| API | `max_items` | 1,000,000 | 1 | 100,000,000 | items | head | Most items in one list job. |
| API | `max_item_bytes` | 4,096 | 16 | 1,048,576 | bytes | head | Longest single item in a list job. |
| Security | `session_idle_h` | 12 | 0.1 | 720 | h | head | Sign-out after this long without activity. |
| Security | `session_max_d` | 7 | 0.1 | 365 | days | head | Sign-out after this long in any case. |
| Security | `login_fail_max` | 5 | 1 | 100 | tries | head | Wrong passwords allowed per window before login is blocked. |
| Security | `login_fail_window_min` | 5 | 1 | 1,440 | min | head | Window for counting wrong passwords. |
| Alerts | `alert_slow_mad` | 3 | 1 | 20 | MAD | head | slow_pc: a PC is slow when its rate is this many MADs below the fleet median. |
| Alerts | `alert_slow_min_s` | 60 | 5 | 86,400 | s | head | slow_pc: how long a PC must stay slow before the alert opens. |
| Alerts | `alert_flaky_losses` | 3 | 1 | 1,000 | losses | head | flaky_pc: more than this many node.lost in 24 h. |
| Alerts | `alert_head_cpu_pct` | 85 | 10 | 100 | % | head | head_hot: head process CPU above this for 60 s. |
| Alerts | `alert_standby_lag` | 5,000 | 1 | 100,000,000 | changes | head | standby_behind: replication lag above this for 30 s. |
| Alerts | `alert_no_standby_s` | 60 | 5 | 86,400 | s | head | no_standby: fewer standbys than the target for this long. |
| Network | `blob_store_gb` | 50 | 0.1 | 100,000 | GB | head | Blob store on the head; above this the least recently used blobs no job names are deleted. |
| Network | `blob_cache_gb` | 20 | 0.1 | 100,000 | GB | agent | Blob cache on each PC; above this the least recently used blobs are deleted. |
| Network | `blob_peer_fanout` | 4 | 1 | 64 | PCs | both | How many PCs one PC serves a blob to at once (peer distribution). |
| Network | `blob_piece_mb` | 4 | 0.25 | 256 | MB | both | Blob transfer piece size; each piece has its own sha256 so a broken transfer resumes. |
| Network | `blob_seeds` | 2 | 1 | 64 | PCs | head | Peer distribution: PCs per lab the head sends a blob to first; the rest get it from peers. |
| Network | `blob_max_mb` | 4,096 | 1 | 1,000,000 | MB | head | Largest single blob accepted by PUT /api/v1/blobs. |
| Network | `net_probe_mb` | 16 | 0.1 | 1,024 | MB | head | Size of the probe transfer that measures a PC's link speed. |
| Network | `net_probe_every_s` | 3,600 | 10 | 604,800 | s | head | How often idle PCs get their link speed measured again. |
| Network | `blob_partial_ms` | 500 | 0 | 60,000 | ms | head | A PC downloading a blob serves its finished pieces to others only after this long. |
| Network | `blob_stall_s` | 20 | 2 | 3,600 | s | agent | A blob download with no new piece for this long is given up (the head then serves it itself). |
| Network | `blob_offer_s` | 5 | 1 | 600 | s | head | How often the primary offers new blobs to standby heads. |
| Network | `blob_auto_kb` | 64 | 1 | 1,048,576 | KB | both | SDK: arguments and results above this size travel as blobs. |
| Network | `head_uplink_mbps_cap` | 0 | 0 | 1,000,000 | Mbit/s | head | Head uplink used in estimates; 0 = learned from parallel transfers. |
| Retention | `events_keep_days` | 30 | 1 | 36,500 | days | head | Events (audit trail) older than this are deleted. |
| Retention | `parts_keep_days` | 0 | 0 | 36,500 | days | head | Result parts of jobs finished longer ago than this are deleted (the job row stays); 0 = keep forever. |
| Retention | `backup_every_h` | 0 | 0 | 8,760 | h | head | The head saves a copy of head.db to ck-data/backups this often; 0 = off. |
| Retention | `backup_keep` | 7 | 1 | 1,000 | files | head | Scheduled backups kept; the oldest are deleted. |
| Agent limits | `out_files_mb` | 256 | 1 | 65,536 | MB | agent | Output files (CK_OUT) a command chunk may leave behind; more makes the chunk fail. |
| Hierarchy | `hier_mode` | 0 | 0 | 2 | 0 off, 1 manual, 2 auto | head | Two-level heads: 0 = off (every PC talks to the root), 1 = manual (only the lab heads you pin), 2 = auto (the root picks the best PC of each big lab). |
| Hierarchy | `lease_s` | 30 | 1 | 3,600 | s | head | Length of one lease: seconds of a lab's work the root hands a lab head at once. Short leases lose less when a lab head dies; long ones ride out a root outage or partition. |
| Hierarchy | `lab_head_min_pcs` | 8 | 2 | 100,000 | PCs | head | Auto mode: a lab gets a lab head when it has at least this many PCs connected. |
| Hierarchy | `lab_silence_s` | 90 | 5 | 3,600 | s | head | A lab head that is silent (not gone) this long loses its leases to the root. Long enough to ride out a partition, short enough not to stall the end of a job. |
| Hierarchy | `lab_head_standby` | 0 | 0 | 1 | 0/1 | head | Reserved: keep a standby for each lab head (not built yet; a lost lab head's PCs fall back to the root instead). |
| Hierarchy | `lab_head_port` | 7,710 | 1,024 | 65,000 | port | head | First TCP port for lab heads the root starts; each lab takes the next 10 (the API is +2). |
| Hierarchy | `lab_part_s` | 0.5 | 0.05 | 30 | s | head | A lab head sends finished pieces of a lease upward at this interval (batched, so one message covers many chunks). |
| Network | `stage_timeout_s` | 120 | 10 | 86,400 | s | head | A chunk reserved for a PC (or lab head) while its blobs download is re-staged from the head after this long (plus 1 s per 2 MB of missing blobs), then given to another PC after the same time again. |
| Agent limits | `job_mem_pct` | 80 | 0 | 100 | % | agent | A command program (all its processes together) is stopped when it uses more than this share of the PC's RAM; 0 = no limit. |
| Logging | `log_batch` | 200 | 10 | 10,000 | lines | head | Most log lines sent to the console in one live batch. |
| Dynamic heads | `cand_recheck_s` | 5 | 1 | 600 | s | head | Live head ranking is recomputed at most this often for the console. |
| SDK | `warm_idle_s` | 120 | 0 | 3,600 | s | agent | Idle seconds before a PC's warm Python worker exits; 0 = warm runner off. |
| Logging | `log_level` | 3 | 0 | 4 | level | both | What is written to ck-data/logs: 0 off, 1 error, 2 warn, 3 info, 4 debug. |
| Logging | `log_chunks` | 0 | 0 | 1 | 0/1 | both | 1 = also log every chunk dispatch and result (debug lines; a lot of output on big jobs). |
| Logging | `log_max_mb` | 64 | 1 | 4,096 | MB | both | A log file is closed and renamed at this size. |
| Logging | `log_keep` | 14 | 1 | 1,000 | files | both | Log files kept per role; the oldest are deleted. |
| Logging | `log_flush_ms` | 200 | 10 | 60,000 | ms | both | How often buffered log lines are written to disk. |
