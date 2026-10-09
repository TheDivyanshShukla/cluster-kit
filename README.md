# pc-cluster: 4-PC Dask cluster (Windows + Ubuntu)

Turns ordinary PCs on one LAN into a distributed computing cluster and measures speedup from 1 → 4 PCs.

- **Engine:** Dask (`dask[distributed]`), a master/worker scheduler
- **Installer:** uv installs the exact same Python (3.12.13) and packages on every PC
- **Auto-discovery:** workers find the head by UDP broadcast, so you never type an IP
- **Auto firewall:** ports open only while running and close on Ctrl+C, window close, logoff/shutdown or kill. Stale rules are removed on the next start.
- **Auto-rejoin:** if the head restarts, workers find it again by themselves

## Architecture

```
            HEAD PC (master)
   scheduler :8786  dashboard :8787
   UDP beacon :8785  + local worker
                 │  switch / LAN
     ┌───────────┼───────────┐
   PC 2        PC 3        PC 4
  worker      worker      worker      (one process per physical core)
```

## Ports (opened automatically, LAN subnet only)

| Role   | Inbound ports                                      |
|--------|----------------------------------------------------|
| Head   | TCP 8786–8787 (scheduler, dashboard), TCP 9000–9200 |
| Worker | TCP 9000–9200 (worker/nanny), UDP 8785 (discovery)  |

Rules are tagged `daskcluster-head` / `daskcluster-worker`. Nothing else in your firewall is touched.

## Setup (once per PC)

Copy this folder to every PC, then:

| Windows | Ubuntu |
|---|---|
| double-click `windows\setup.bat` | `./linux/setup.sh` |

This installs uv, then Python 3.12.13 and the pinned packages (from `uv.lock`). It also offers to disable sleep.

## Run

1. **Head PC:** `windows\head.bat` or `./linux/head.sh`
2. **Other 3 PCs:** `windows\worker.bat` or `./linux/worker.sh`
   The head window prints `+ node joined ...` as each one connects.
3. **Check:** `status.bat` / `./linux/status.sh`, or open `http://HEAD_IP:8787`
4. **Benchmark (on the head):** `bench.bat` / `./linux/bench.sh`
   This writes `results_summary.csv`, `results_raw.csv` and `scaling.png` (speedup and efficiency charts).
5. **Stop:** Ctrl+C or close the window. Ports close automatically.

Windows launchers ask for Administrator rights (needed for firewall rules). On Ubuntu, if ufw is active, you're asked for your sudo password once.

## Options

```
head    --procs N            worker processes on the head (default: physical cores)
        --no-local-worker    head only schedules (pure 1 master + 3 compute nodes)
worker  --head 192.168.1.10  skip discovery and use this IP
        --procs N
bench   --total 2e7 1e8      workload sizes (small vs large shows communication overhead)
        --repeats 3          runs per point (median is reported)
        --max-nodes 3        limit the scaling test
global  --name lab           cluster name (two clusters can share one LAN)
        --no-firewall        don't touch firewall rules
```

Example: `windows\worker.bat --procs 4`, or `./linux/bench.sh --total 5e7 2e8 --repeats 5`

## How the benchmark works

It runs the same Monte Carlo π job (CPU-bound, pure Python) on the first 1, 2, 3, then 4 PCs. The head PC is always first, then the others in IP order. All workers stay connected, and each run is restricted to the chosen PCs.

- **Speedup** = T₁ / Tₙ
- **Efficiency** = Speedup / n

Expect less than a perfect 4× because of network/scheduling overhead, the serial part of the program (Amdahl's law) and differences between the PCs' hardware. Small workloads scale worse than large ones, which is a good point for the report.

## Troubleshooting

| Problem | Fix |
|---|---|
| Worker stuck on "still waiting" | Make sure the head is running and both PCs are on the same subnet. Some routers or Wi-Fi block broadcast, so use `worker --head HEAD_IP`. |
| Node joins, then the benchmark hangs | A firewall is still blocking. Run the launcher as Admin (Windows), or `sudo ufw status` (Ubuntu). Third-party antivirus firewalls need ports 8786–8787 and 9000–9200 allowed. |
| Firewall rules left behind | They're removed on the next start, or run `fw-clean.bat` / `./linux/fw-clean.sh` |
| Wrong network adapter (VPN/VirtualBox) | Disable the extra adapter, or use `--head` with the LAN IP |
