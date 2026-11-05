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
| The head dies | Wall banner "Head failed — standby taking over"; the first standby takes over after about 6 s; Settings shows the new term (epoch) | Nothing for the jobs: they finish on the new head with exact results. The agents and `ck run` follow the new head on their own. Repair the old head PC later: when it comes back, it hears the newer head and rejoins as its standby. |
| The new head waits ("candidate, waiting for quorum") | It knows more PCs than are connected | It waits for more than half of the PCs it knew about. PCs that never come back are dropped 60 s after the takeover. Bring the network back if a whole lab is cut off. |
| A head stops with "FENCED" | Two heads saw each other after a network split | Expected. The one with the older term rejoins as a standby by itself. |
| A PC is refused ("wrong key", "join code invalid") | Events: `node.denied` | Make a new join code and reinstall that PC's agent with `--code`. |

Test the failover on one machine any time with `scripts/chaos.sh`. It prints PASS or FAIL for 5 scenarios, including killing the head while a standby is running.

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

## 6. Backups and moving the head

- **Everything is in `ck-data/head.db`** (SQLite). The standbys each hold a full copy that is replicated continuously, so losing the head PC loses nothing.
- **Saving the setup:** `ck admin config-export > setup.json` saves labs, the map, labels, pools, caps and reserves, but not users or jobs. `ck admin config-import setup.json` restores it, and `CK_DRY_RUN=1` shows the changes first.
- **Backup now:** `ck admin backup [PATH]` writes a consistent copy of `head.db` (SQLite `VACUUM INTO`, safe while the head runs; run it on the head PC with the same `CK_DATA`). With no PATH it writes `ck-data/backups/head-<unix-seconds>.db`.
- **Scheduled backups:** set `backup_every_h` (default 0 = off) and `backup_keep` (default 7); the head writes `ck-data/backups/head-<unix-seconds>.db` on that schedule and deletes the oldest beyond `backup_keep`. Copy that folder off the PC (another disk or share) yourself; a backup next to the database does not survive a dead disk.
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
