# Cluster Kit

Turns ordinary Windows and Ubuntu PCs on one LAN into a compute cluster. It has two engines that share the same setup and launcher:

| Engine | Folder | What it is | Dashboard |
|---|---|---|---|
| `py` | [python/](python/) | Dask (Python). Run any Python function across the cluster, with a 1 → N PC scaling benchmark. | `http://HEAD_IP:8787` |
| `rs` | [rust/](rust/) | `ck`, a Rust binary that uses raw TCP and a binary protocol. Built-in SHA-256 search, or runs a program in any language per chunk. | `http://HEAD_IP:7702` |

Both engines have:
- **Auto-discovery:** workers find the head by UDP broadcast, so you never type an IP.
- **Auto-rejoin:** workers reconnect by themselves if the head restarts.
- **Auto firewall:** ports open only while running, close on exit, and stale rules are cleared on the next start.

## Quick start (Rust engine, one PC)

Try the whole thing on one machine first. Use the `ck` binary directly; `kit rs ...` runs the same commands with the firewall handled.

Fastest look at the console: `scripts/demo.sh` (after the build below). It starts a root head, a standby, two labs of 8 simulated PCs with a lab head each, and a few jobs, then opens `http://127.0.0.1:15002`. Power actions are dry runs and the long job only sleeps; Ctrl-C stops it all.

```
# 1. Build (installs nothing else; add --no-default-features to skip GPU support)
cd rust && cargo build --release && cd ..
export PATH="$PWD/rust/target/release:$PATH"

# 2. Start the head (console at http://127.0.0.1:7702)
ck head --no-local-worker

# 3. In a second terminal: join an agent. Here 4 pretend PCs on this machine;
#    on a real PC use:  ck agent --head HEAD_IP   (no --head: it finds the head by broadcast)
ck agent --head 127.0.0.1 --virtual 4

# 4. Create the first admin: open http://127.0.0.1:7702 and fill the form, then get an API token
CK_USER=admin ck token --head 127.0.0.1        # asks for the password, prints a token
export CK_TOKEN=<the token>

# 5. First job: SHA-256 search, then a program of your own
ck admin nodes --head 127.0.0.1                # the agents should be online
ck run --head 127.0.0.1 --n 1e8 --sha 00000
cc -O2 examples/primes.c -o examples/primes    # run from the kit folder; agents must start there too
ck run --head 127.0.0.1 --n 1e7 --from 0 --chunk 1e6 --cmd examples/primes | awk '{s+=$1} END {print s}'   # 664579
ck job ls --head 127.0.0.1                     # past and running jobs
ck logs --since 10m --kind node. --limit 5     # structured log (run on the head PC)
```

Open `http://127.0.0.1:7702` for the console: Overview, Nodes, Jobs (New job, results, breakdown), Live map, System settings. A new head asks for the first admin there. For a real lab (join codes, standby heads, running agents as a service) follow [docs/OPERATIONS.md](docs/OPERATIONS.md). How the pieces fit: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### Python SDK

Run ordinary Python functions on the cluster. The SDK needs only the standard library (Python 3.9 or newer).

```
pip install ./python                           # or: export PYTHONPATH=$PWD/python
export CK_HEAD=http://127.0.0.1:7702 CK_TOKEN=<the token>
```

```python
import clusterkit as ck

def square(x):
    return x * x

if __name__ == "__main__":
    print(ck.map(square, range(1000))[:5])     # [0, 1, 4, 9, 16], same order as a list comprehension
```

It runs locally when the cluster would not be faster. More: `examples/sdk/` (`sweep.py`, `model_avg.py`, `ml_torch.py`) and [docs/SDK.md](docs/SDK.md).

## Beyond one head (Rust engine)

- **Lab heads** ([docs/HIERARCHY.md](docs/HIERARCHY.md)): for hundreds of PCs, one head per lab serves its PCs and talks to the root in big leases, so the root's CPU stops being the limit. Off by default; `ck admin hier settings mode=auto` turns it on in a running fleet, and `mode=off` turns it off again in the middle of a job. Every job kind runs through lab heads except `--only`.
- **Advisor** ([docs/ADVISOR.md](docs/ADVISOR.md)): the console's Advisor page and `ck admin advice` list measured suggestions with options, cost and risk; you apply, undo or dismiss them, and the head checks each applied one against its prediction after 10 minutes.
- **Limits per program**: a command program is stopped when it uses more than `job_mem_pct` (80 %) of the PC's RAM, prints more than `out_max_mb` (64 MB) to stdout, or leaves more than `out_files_mb` (256 MB) in `$CK_OUT`. A chunk waiting for its files is re-staged after `stage_timeout_s` (120 s). All tunables: [docs/OPERATIONS.md](docs/OPERATIONS.md) section 8.
- **Real workloads** (`examples/real/`): CNN training, an SVM sweep, a 5-million-row ETL, an image pipeline and a 1 GB text job, each `python3 examples/real/NAME.py local|cluster`, with the same exact digest either way. `scripts/stress/workloads.py` runs them against a fleet, with faults. Results: [docs/audit/ROUND4.md](docs/audit/ROUND4.md).

## Layout

```
kit, kit.bat, kit.ps1   one launcher for everything (Linux/macOS, Windows)
python/                 Dask engine: cluster.py (head/worker/bench/firewall)
rust/                   Rust engine: src/main.rs, src/gpu.rs + sha.wgsl (GPU), src/dash.html,
                        bench/mpi_pingpong.c + bench/mpi_sha.c (MPI baselines)
examples/               example jobs: simple_sha256.py (start here, see docs/SIMPLE.md), dask_sha256.py (Dask), primes.c / primes.js / primes.py (any-language --cmd)
docs/                   ARCHITECTURE.md (start here), OPERATIONS.md, API.md, SDK.md, JOBS.md, BULK.md, HIERARCHY.md (lab heads), ADVISOR.md,
                        SIMPLE.md, BENCHMARK.md, UBUNTU.md, LOGGING.md, audit/ (bad-case and real-workload rounds), roadmap: CONSOLE_REQUIREMENTS.md
examples/real/          five real Python workloads (train, sweep, etl, cv, text)
```

To add an engine later, create a folder and add one `case` branch to `kit` (and `kit.ps1`). To get firewall handling for free, start it through `python/cluster.py fwrun`.

## Setup (once per PC)

Copy or clone this folder to every PC, then:

| Windows | Linux / macOS |
|---|---|
| `kit setup` | `./kit setup` |

- **Python engine:** installs uv, then the pinned Python 3.12.13 and packages from `uv.lock`.
- **Rust engine:** installs Rust and builds `ck`. Use `kit setup py` to skip Rust.
- **Sleep:** setup also offers to disable sleep on that PC.
- **Windows and Rust:** building needs the MSVC build tools. If the build fails, setup prints the `winget` command that installs them.
- **Ubuntu PCs:** what to copy, packages, offline builds and ports are in [docs/UBUNTU.md](docs/UBUNTU.md).
- **Many Ubuntu PCs, fast:** `sudo bash scripts/provision.sh` does everything on one PC and repairs what it finds wrong: no sleep or screen lock, packages, ssh, the folder in `~/Desktop/cluster-kit`, the build, and a `ck-worker` service that starts at boot and joins the head. Run it again any time. An ESP32-S3/S2 stick can type it for you: flash it with `hid/flash.sh` (see the header of [hid/ck_provision/ck_provision.ino](hid/ck_provision/ck_provision.ino)), plug it into a logged-in PC, unplug after typing finishes (USB-paced; wait for the done light).

### Visible Windows 10 / 11 worker

Use the saved repository files: right-click `scripts\provision-windows.cmd` and choose **Run as administrator**, then approve UAC. It uses built-in Windows PowerShell 5.1, installs Git, Rust/MSVC and OpenSSH as needed, builds the worker, configures the cluster firewall rules, and starts the scheduled worker at boot. The final console shows hostname, IPv4 addresses, SSH commands and live worker logs. Set `CK_FOLLOW=0` to finish without following logs.

The launcher respects the existing script execution policy and makes no Defender exclusions. Managed PCs may require an administrator-approved signed script. Antivirus acceptance and live Windows behaviour have to be verified on the target PC. Logs are in `%ProgramData%\cluster-kit`.

### Visible Ubuntu worker

Run `bash ~/Desktop/cluster-kit/scripts/worker-ubuntu.sh` in an Ubuntu terminal. It repairs missing packages (including interrupted dpkg and broken apt dependencies), enables SSH, updates the current Git branch without overwriting tracked edits, rebuilds if necessary, and replaces old local `ck` processes, including an old local head. The replacement worker starts at boot in open mode with no join code. Sleep, display blanking and automatic locking are disabled.

The terminal shows the hostname, large IP addresses, SSH connection commands and live worker logs. `connected to head` confirms that PC joined; an IP banner alone does not. Ctrl+C closes the log view while the service keeps running. SSH still uses the PC's normal account authentication or your configured public key.

For an Ubuntu-only ESP32 stick: `CK_PROVISION_BRANCH=main bash hid/flash.sh --os ubuntu SERIAL_PORT`. The first typed command authenticates sudo and installs curl/certificates before downloading the setup script. Leave it plugged into the target until typing finishes (USB-paced; wait for the done light). Flashing leaves the stick in download mode: unplug it from the flashing computer and plug it into a logged-in Ubuntu PC to run. The script and firmware must use a branch already pushed to GitHub.

## Run

| | Python engine | Rust engine |
|---|---|---|
| Head PC | `kit py head` | `kit rs head` (add `--gpu` to hash on the GPU) |
| Other PCs | `kit py worker` | `kit rs worker` (add `--gpu` to hash on the GPU) |
| Check | `kit py status` | dashboard (has a Details toggle) |
| Work | `kit py run examples/dask_sha256.py` | `kit rs run --n 1e9 --sha 00000` |
| Benchmark | `kit py bench` | `kit rs ping --head HEAD_IP`, `kit py run examples/sha_bench.py` (see [docs/BENCHMARK.md](docs/BENCHMARK.md)) |

On Linux and macOS, write `./kit` instead of `kit`. Stop anything with Ctrl+C or by closing the window, and the ports close automatically.

On Windows, `head` and `worker` ask for Administrator rights, which firewall rules need. On Ubuntu with ufw active, they ask for your sudo password once.

### Options (put them after the command)

```
py head    --procs N  --no-local-worker  --name lab  --no-firewall
py worker  --head 192.168.1.10  --procs N  --name lab
py bench   --total 2e7 1e8  --repeats 3  --max-nodes 3
py run     <script.py> [its args]     runs a script in the cluster's Python env

rs head    --threads N  --no-local-worker  --name lab  --gpu  --spin
rs worker  --head 192.168.1.10  --threads N  --name lab  --gpu  --spin
rs run     --n 1e9  --from 1  --chunk 2e7  --only IP,IP  (--sha PREFIX | --cmd "prog")
rs ping    --head IP  --count 10000  --size 8  --spin

tune       on | off       low-latency network settings (Linux, Windows); reverts on "off" or reboot
```

## Your own jobs

- **Python (Dask engine):** copy [examples/dask_sha256.py](examples/dask_sha256.py). Write a plain function, then `client.map(fn, inputs)`. Run it on the head PC only with `kit py run your_job.py`; Dask ships the function to the workers. Any package it imports must be in `python/pyproject.toml` on every PC.
- **Any language (Rust engine):** write a program that takes `START END`, does the work and prints its result.
  - Each worker splits its chunk across all cores and runs one copy of your program per core. For a program that is already multi-threaded, start the worker with `--threads 1`.
  - The head prints all the output.
  - The program must exist at the same path on every PC, built for that OS. Paths are relative to the kit folder.

  These count the primes below 10 million (664579) on one 10-core PC:

  | Example | Command | Time |
  |---|---|---|
  | C | `cc -O2 examples/primes.c -o examples/primes`, then `kit rs run --n 1e7 --from 0 --chunk 1e6 --cmd examples/primes \| awk '{s+=$1} END {print s}'` | 0.25 s |
  | Node | `--cmd "node examples/primes.js"` | 0.64 s |
  | Python | `--cmd "uv run --project python examples/primes.py"` | 5.4 s |
- **Cancel:** Ctrl+C on the job cancels it. Chunks that are already running finish first.

## Ports (opened automatically, LAN subnet only)

| Engine | Head | Worker |
|---|---|---|
| py | TCP 8786–8787, TCP 9000–9200 | TCP 9000–9200, UDP 8785 |
| rs | TCP 7700 (workers, clients), TCP 7702 (dashboard) | UDP 7701 (discovery) |

Rules are tagged `daskcluster-head`, `daskcluster-worker`, `daskcluster-ck-head` and `daskcluster-ck-worker`. Nothing else in your firewall is touched.

**Security:** the Dask programs (py) have no authentication: anyone on the LAN can run code on every PC through them, so use them on trusted networks only. The Rust kit (rs) does: PCs join with a join code and prove a node key, links are sealed, and `ck run` needs an API token in `CK_TOKEN` (`cmd` and list jobs need the operator role). Only a test fleet started with `CK_FLEET=open` skips these checks.

## Benchmarks

**`kit py bench`**
- Runs the same Monte Carlo π job on the first 1, 2, 3, then 4 PCs. The head is always first, then the others in IP order.
- Writes `results_summary.csv`, `results_raw.csv` and `scaling.png`.
- Speedup = T₁ / Tₙ, and efficiency = speedup / n.
- Expect less than a perfect 4×, because of network and scheduling overhead, the serial part of the program (Amdahl's law), and hardware differences between the PCs.

**Rust engine vs MPI**

How `ck` saves time compared with a typical MPI program:
- **Pipelining:** each worker holds 2 chunks, one computing and one queued, so it never waits on the network between chunks. On one PC this was 7–38% faster than without pipelining.
- **One message per chunk each way:** a result doubles as the request for the next chunk.
- **No handshake:** large messages are sent straight away. MPI first waits for a "ready" reply above about 64 KB.
- **Buffered reads:** one system call usually reads a whole message.
- **Cancel stops work mid-chunk:** the next job starts in about 40 ms.
- **`--spin`:** waits by polling the socket instead of sleeping. It lowers latency on an idle PC, but each open connection uses a full core while waiting.
- **`--gpu`:** SHA-256 runs on the GPU through wgpu (Metal, Vulkan or DX12), with an automatic CPU fallback. It is tested to give exactly the CPU's results.

Measured on one 10-core Mac (busy with other work, so treat the numbers as rough):

| SHA-256 of 1..3×10⁸, prefix `0000000` | Speed |
|---|---|
| MPI master/worker in C, 10 ranks (`bench/mpi_sha.c`) | 26.7 M/s |
| `ck`, CPU | 23.0 M/s |
| **`ck --gpu`** (built-in Apple GPU) | **312 M/s** |

| Round trip on one machine, TCP | MPI | `ck` |
|---|---|---|
| 8 B | 14.0 µs | 14.1 µs |
| 64 KB | 53 µs | 31 µs |
| 1 MB | 190 µs | 169 µs |

On one PC the CPU paths tie, because there is no network to win on. Once messages carry the load, `ck` wins. This test hashed 10⁷ numbers with MPI forced onto TCP (`--mca btl self,tcp`), on loopback:

| Work per message (per core) | MPI over TCP | `ck` |
|---|---|---|
| 1,000 numbers | 0.337 s | 0.344 s |
| 10,000 numbers | 0.304 s | **0.202 s** |
| 100,000 numbers | 0.273 s | **0.194 s** |

Why `ck` wins:
- **Fewer messages:** one connection per PC instead of one per core.
- **Pipelining:** the next chunk is already queued, so there's no round trip between chunks.
- **No polling master:** MPI's master process takes a core just to watch for messages.

On a real LAN, round trips are longer, which should widen the gap. To measure your LAN:
```
mpicc -O2 rust/bench/mpi_sha.c -o mpi_sha            # Linux: add -lcrypto
mpirun -np 41 --host A:11,B:10,C:10,D:10 ./mpi_sha 1e9 1e6 0000000
kit rs run --n 1e9 --sha 0000000                     # with kit rs head/worker running on A..D

mpicc -O2 rust/bench/mpi_pingpong.c -o mpi_pingpong
mpirun -np 2 --host A,B ./mpi_pingpong 8 10000       # and from B: kit rs ping --head A --size 8
```
Run `kit tune` on each PC first for the lowest latency.

`kit tune`:
- **Linux:** turns off NIC interrupt coalescing, enables socket busy-polling and sets the CPU to full clock. The original values are saved and restored by `kit tune off`.
- **Windows:** turns off interrupt moderation and switches to the High performance power plan. The network adapter restarts briefly while this applies.

**GPU notes:**
- Linux needs the Vulkan driver (`mesa-vulkan-drivers`, or the NVIDIA driver).
- Windows uses DX12 or Vulkan from the normal graphics driver.
- To build without GPU support: `cargo build --release --no-default-features` in `rust/`.

## Troubleshooting

| Problem | Fix |
|---|---|
| Worker stuck on "still waiting" or "looking for head" | Make sure the head is running on the same subnet. Some routers and Wi-Fi block broadcast, so use `--head HEAD_IP`. |
| Node joins, then work hangs | A firewall is still blocking. Run as Admin (Windows), or check `sudo ufw status` (Ubuntu). Third-party antivirus firewalls need the ports above. |
| Firewall rules left behind | They're removed on the next start, or run `kit fw-clean`. |
| Wrong network adapter (VPN/VirtualBox) | Disable the extra adapter, or use `--head` with the LAN IP. |
| `ck not built` | Run `kit setup rs` (or `kit rs build` after changing `rust/`). |
