# Cluster demo: a head and three more PCs

What this shows, in the order of the user's three asks:

1. A PC joins the cluster with **one command** (or a pairing code), and the work it needs (models, settings) comes from the head. Recordings are read from the head as the worker goes; nothing else is installed.
2. A **benchmark** of indexing time with one PC, then with two, three, four, with speed-up and efficiency read off the chart.
3. **Realtime**: phone cameras as live cameras, the head shows which PC analyses which camera, how much headroom there is, and cameras move when a PC is overloaded and another one is free.

Everything is in the **Cluster** page (`/analytics`) and the **Join a PC** page (`/join`). Evidence of what was actually run is in `docs/evidence/k/`.

## 0. Rules of the room

- A PC joins only when someone runs a command on it, or approves a pairing code. Nothing on the head pushes software to another machine. `cw deploy user@host` is the one exception that reaches out, and it only runs ssh to a host you name, with the ssh access you already have.
- Numbers measured with several workers on **one** machine are not cluster numbers: they share one CPU. The pages say so, and so should you.

## 1. Before the day

On **every** PC (head included):

| Need | Why | Check |
| --- | --- | --- |
| Same network (Wi-Fi or Ethernet), no client isolation | PCs must reach the head | `ping <head-ip>` |
| `ffmpeg` and `ffprobe` on `PATH` | decoding | `ffmpeg -version` (macOS `brew install ffmpeg`, Debian/Ubuntu `sudo apt install ffmpeg`, Windows `winget install ffmpeg`) |
| `curl` and `tar` | the installer | built in on macOS, Linux and Windows 10+ |
| Rust (`https://rustup.rs`) | **only** for a PC whose OS or CPU differs from the head (it builds from the head's source) | `cargo --version` |

Firewall on the **head**:

| Port | Protocol | Used for |
| --- | --- | --- |
| 7710 | TCP | the web pages, the API, worker traffic, recordings and models for workers |
| 7711 | UDP | the head's "I am here" beacon so `cw join` can find it. Optional: give the address instead |

macOS asks once whether `cw` may accept incoming connections: allow it. Windows Defender: allow `cw.exe` on private networks.

Prebuilt vs source: the head hands out **its own program** as the worker for PCs with the same OS and CPU (for example a macOS Apple-silicon head serves Apple-silicon Macs). Any other PC gets the Rust source and builds it (first build several minutes, needs internet for crates). If the demo room has a Windows or Linux PC, build on it **before** the day, or run the head on the same OS as most PCs. The installer prints which path it takes.

## 2. Start the head

From the repo root, with the models and footage in `vision/`:

```
CW_BIND=0.0.0.0 CW_AUTH=required cw head
```

- `CW_BIND=0.0.0.0` lets other PCs reach it. Without it the head listens on this PC only (the safe default in open mode).
- Sign-in on (`CW_AUTH=required`, or any user exists): joining needs a **pairing code** and only an administrator can create one. The first start prints a one-time bootstrap token; create the administrator with the `/api/v1/auth/bootstrap` call shown in `docs/WATCH_OPERATIONS.md`, then sign in at `/login`.
- Open mode (no users, `CW_AUTH` unset) lets anyone on the network join without a code. Use it only on a closed demo network.

Open `http://<head-ip>:7710/join` **by the head's network address**, not `localhost`: the commands on that page carry the address you opened it with.

## 3. Join the other PCs

On `/join`:

1. Press **Make a pairing code** (administrator). The code is 8 characters, valid for 10 minutes, one PC per code (raise the number for a batch).
2. On each PC open a terminal and paste the line for its system:
   - macOS / Linux: `curl -fsSL "http://<head-ip>:7710/api/v1/join/install.sh?code=XXXX-XXXX" | sh`
   - Windows PowerShell: `irm "http://<head-ip>:7710/api/v1/join/install.ps1?code=XXXX-XXXX" | iex`
3. The PC prints its steps: downloads the program into `~/.watch-worker` and checks its sha256, enrolls (a worker token comes back; the head stores only its hash), downloads the detector model (checked by sha256, kept under `~/.watch-worker/root`), then runs as a worker. Ctrl-C leaves the cluster.
4. The PC appears in the **PCs** table of the Cluster page and on `/join` within a few seconds.

Already have the program on a PC: `cw join <head-ip> --code XXXX-XXXX --name lab-pc-2`. Without an address, `cw join` listens for the beacon for 30 seconds. After the first enrollment the token is cached, so restarting needs no code. If the head restarts the worker keeps retrying and reconnects by itself.

From the head, push to a PC you can ssh into (you supply the access):

```
cw deploy me@192.168.1.31 --head 192.168.1.20 --code XXXX-XXXX     # with sign-in on
cw deploy me@192.168.1.31 --dry-run                                  # prints the exact ssh command, runs nothing
```

The target needs `curl`, `tar`, `ffmpeg`, and to reach the head on TCP 7710. The worker is started detached on it, log in `~/.watch-worker/worker.log`.

A PC without the recordings is fine: the worker streams each recording from the head with HTTP range requests (`/api/v1/worker/media/{id}/stream`, worker token only). This costs network: a 720p H.264 file is a few Mbit/s. For the fairest speed-up number use a wired network.

## 4. The benchmark (indexing time, 1 PC then more)

Cluster page, **Benchmark** panel.

1. Make sure **one** PC is online (switch the others off or leave them un-joined). The title bar of the panel shows how many are online.
2. Tick one or more recordings. Use enough video that a run takes a few minutes (the four one-hour MEVA files are far too long; a 5 minute VIRAT clip repeated 4 times is a good length). **Repeat** copies the selection; **Segment** is the part length in seconds (the setting default is 120).
3. Name it "1 PC" and press **Run benchmark**. It indexes the recordings with the real pipeline, the current Rules and models settings, into a private store. Per-PC progress bars and the ETA update every second.
4. Join the second PC (section 3). Name the next run "2 PCs", **same recordings, same repeat, same segment**, run again. Then 3 and 4.
5. Read the chart: bars of time per run, and speed-up against the 1-PC run with a mark where the ideal would be. **Efficiency** is speed-up divided by PCs; 100% is perfect. Runs are only compared when recordings and settings are identical.
6. Delete test runs with **Delete** (removes the run and everything it indexed).

How the work is split: each recording longer than the segment length becomes parts of that length (2 s overlap for tracker warm-up, never indexed twice). A PC holds at most two parts at once and pulls the next when it finishes one, so a faster PC naturally does more. A part whose PC dies goes back to the queue and resumes where it stopped.

The panel shows a **warning** whenever two workers share a machine: "Workers on one machine share the CPU: no speed-up expected."

Do not run the benchmark while live cameras are analysing: they compete for the same workers. The result records how many other tasks were running at the start.

Tracker ids: a part's tracker starts fresh, so a person walking across a part boundary gets a new id there. Detections and frame times are identical to a single-PC run (see `docs/evidence/k/segments.md`).

## 5. Realtime with phone cameras

Cluster page, **Add a phone camera**.

1. On the phone start any app that publishes an RTSP or HTTP MJPEG stream and note the address it shows. Typical forms: `http://<phone-ip>:8080/video`, `rtsp://<phone-ip>:8554/<path>`. The phone and the head must be on the same Wi-Fi.
2. Paste it, press **Test**. The head runs `ffprobe` for at most 5 seconds and reports size, fps, codec and the answer time, or a plain reason (nothing listening, wrong path, no answer, wants a password). Addresses with a user name or password are refused and never stored.
3. Press **Add as live camera**. A worker picks it up; it appears under **Realtime**.
4. A Mac's own camera (or an iPhone through Continuity Camera, when macOS lists it) shows under "Cameras attached to the PCs" with a **Use this Mac's camera** button.

**Realtime** panel:

- per camera: the PC analysing it, analysed fps, needed fps, lag, and the status **keeping up** / **behind**;
- per PC: demand against capacity and the cameras it holds (the map);
- a total headroom bar. Capacity is one divided by the measured cost of a frame on that PC, so it moves with how busy the picture is;
- **Rebalancing**: a camera that stays behind for 20 seconds while another PC holds at least two fewer cameras is moved there, one camera per 10 seconds at most. The move is listed on the page. The tracker restarts on the new PC, so track ids do not continue across a move.

Demo: start with all cameras on one PC (it falls behind), join a second PC, watch the cameras spread and the status turn to "keeping up".

## 6. If something goes wrong

| Symptom | Likely cause and fix |
| --- | --- |
| `cw join` says it cannot reach the head | wrong address, head not started with `CW_BIND=0.0.0.0`, TCP 7710 blocked |
| "no head announced itself" | UDP 7711 blocked or different subnet: give the address |
| "pairing code is wrong, used up or expired" | codes last 10 minutes and one PC; make a new one. Ten wrong tries lock the door for a minute |
| installer: "this needs Rust" | the PC's OS or CPU differs from the head: install Rust, run again |
| installer: checksum mismatch | download broke; run again. Nothing is installed on a mismatch |
| a PC shows "lost" | head heard nothing for 15 s; its work went to others. Restart `cw join` there |
| a PC shows a model error | the model file did not download; look at the first lines of the join output |
| no frames, "ffmpeg did not start" | `ffmpeg` missing on that PC |
| benchmark does not start | no PC online, or another benchmark is running |
| phone test: "no answer within 5 seconds" | not the same Wi-Fi, or guest-network isolation, or the app is not serving |
| the speed-up is ~1x | workers share a machine, or the network is the limit (check the warning), or the recordings are too short to split |

## 7. What to say about the numbers

- Say what a number is. "Four workers on this laptop took 9% longer than one" is a statement about one laptop. A cluster number is one PC per worker.
- Speed-up and efficiency come only from runs you made, for the same recordings and settings. If only one run exists the page shows only that run.
- Parts are indexed twice at their edges (2 s overlap) to warm the tracker; the extra decode work is counted as "decoded", never as "inferred".
- Streaming recordings to a worker adds network time; that is part of the honest cost of a PC without a local copy.
- The capacity figure in Realtime is an estimate from measured frame cost, not a guarantee.

## 8. Decisions made without asking (defaults)

- Pairing code: 8 characters from an alphabet without look-alikes, 10 minutes, single use unless raised (max 50), at most ten wrong guesses a minute.
- Scripts and the program bundle are public in open mode; with sign-in on they need a live code. Codes are created by administrators only. Worker tokens are named and stored hashed.
- Open mode still enrolls workers with a token, so switching on sign-in later does not break them.
- Prebuilt bundle = the head's own executable (plus ONNX Runtime libraries next to it, if any), only for the head's own OS and CPU. Every other PC builds from `rust/` sources served by the head (size capped at 60 MB, no target dirs).
- Parts: default 120 s (`segment_secs` in the processing settings; 0 turns splitting off), 2 s overlap, at most two finite tasks per PC at once (`CW_TASKS` changes it).
- Parts are not moved between PCs while running (the old "move work to an idle PC" rule applies only to whole-file recordings and only when nothing is waiting in the queue).
- Live cameras: "behind" = more than 0.5 dropped frames per second or a queue/decoder lag above 2.5 s; moved after 20 s behind, never more often than every 10 s, never from a PC holding fewer than two more cameras than the target.
- Benchmark data lives in its own SQLite file under `vision/data/bench/` next to the run's JSON; footage must be under `vision/footage/` (or `CW_MEDIA_ROOTS`).

## 9. What was not verified without real extra PCs

- Real network speed-up (all measurements here are one machine; see `docs/evidence/k/bench_local.md`).
- The installer on Linux and Windows (scripts are generated and unit-tested; only the macOS path was run). The source-build path (`cargo build` on the joining PC) was not run.
- `cw deploy` over a real ssh session (the command text and dry run are tested; ssh to a second machine was not).
- UDP beacon discovery across machines (`cw join` without an address) and firewalls.
- Behaviour of phone apps: the probe and live path were exercised with an HTTP MJPEG stream served by ffmpeg, not with Android or iOS apps.
- Several PCs with different speeds sharing parts; a PC joining or dying under network latency.
