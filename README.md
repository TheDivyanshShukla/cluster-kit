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

## Layout

```
kit, kit.bat, kit.ps1   one launcher for everything (Linux/macOS, Windows)
python/                 Dask engine: cluster.py (head/worker/bench/firewall)
rust/                   Rust engine: src/main.rs, src/gpu.rs + sha.wgsl (GPU), src/dash.html,
                        bench/mpi_pingpong.c + bench/mpi_sha.c (MPI baselines)
examples/               example jobs: dask_sha256.py (Dask), primes.c / primes.js / primes.py (any-language --cmd)
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

## Run

| | Python engine | Rust engine |
|---|---|---|
| Head PC | `kit py head` | `kit rs head` (add `--gpu` to hash on the GPU) |
| Other PCs | `kit py worker` | `kit rs worker` (add `--gpu` to hash on the GPU) |
| Check | `kit py status` | dashboard (has a Details toggle) |
| Work | `kit py run examples/dask_sha256.py` | `kit rs run --n 1e9 --sha 00000` |
| Benchmark | `kit py bench` | `kit rs ping --head HEAD_IP` |

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
rs run     --n 1e9  --from 1  --chunk 2e7  (--sha PREFIX | --cmd "prog")
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

**Security:** neither engine has authentication. Anyone on the LAN can run code on every PC: through Dask, or through `ck run --cmd`. Use them on trusted networks only.

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
