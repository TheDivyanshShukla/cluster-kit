# SHA-256 cluster benchmark

Shows, with statistics, how much each PC adds to a SHA-256 prefix search on the Rust engine (`ck`).

It measures:

- every PC alone (each PC's own compute)
- the first 2, 3, ..., N PCs together (scaling, ending with all PCs)
- every PC's share of the work and how busy it was inside the all-PC run

It proves the PCs help with:

- an exact one-sided permutation test for each step: is k PCs faster than k-1 PCs? Also all PCs vs the best single PC
- the speedup of all PCs over the head alone, with a 95% bootstrap confidence interval
- parallel efficiency: measured throughput / sum of each PC's solo throughput

It keeps results consistent with:

- each config sized to run about `--seconds`, plus one discarded warm-up run
- repeats run round-robin, so heat or background load affects every config equally
- a correctness check: every repeat of a config must find the same hits, or the bench aborts
- median, 95% CI and CV for every config, with a warning when CV is above 2%

The head PC is the one that runs `kit rs head` and the bench. Every other PC is a worker.

---

## 1. Get the code and build `ck` (every PC, once per update)

**Mac / Ubuntu**

```bash
cd ~/Desktop/code/divy/cluster-kit   # wherever the repo is on that PC
git pull
./kit rs build
```

**Windows (PowerShell or cmd, inside the repo folder)**

```bat
git pull
kit rs build
```

First time on a PC? Run `./kit setup` (Windows: `kit setup`) instead of `kit rs build`.

---

## 2. Prepare for clean numbers

- Close browsers, IDEs, and anything else heavy on every PC.
- Plug laptops in and keep the lid open.
- Use wired Ethernet if you can.
- Linux PCs: `./kit tune` for the lowest network latency (undo with `./kit tune off`).

---

## 3. CPU benchmark

**Head PC, terminal 1 (leave it running)**

```bash
./kit rs head
```

**Every other PC (leave it running)**

```bash
./kit rs worker
```

Windows: `kit rs worker`.

Wait until the head prints `+ worker <IP>` once for every PC.

**Head PC, terminal 2**

```bash
./kit py run examples/sha_bench.py --repeats 7 --seconds 3 --label cpu
```

With 4 PCs this takes about 3 minutes.

---

## 4. GPU benchmark

Stop the head and every worker with Ctrl+C, then start them with `--gpu`.

**Head PC, terminal 1**

```bash
./kit rs head --gpu
```

**Every other PC**

```bash
./kit rs worker --gpu
```

Windows: `kit rs worker --gpu`.

**Head PC, terminal 2**

```bash
./kit py run examples/sha_bench.py --repeats 7 --seconds 3 --label gpu
```

---

## 5. Open the results (head PC)

**Mac**

```bash
ls bench_out/
open bench_out/*-cpu/bench.png bench_out/*-gpu/bench.png
open bench_out/*-cpu/report.md bench_out/*-gpu/report.md
```

**Ubuntu**: `xdg-open bench_out/<folder>/bench.png`. **Windows**: `start bench_out\<folder>\bench.png`.

Each run folder contains:

| File | Content |
|---|---|
| `report.md` | verdict, significance tests, consistency, scaling, per-PC share, job-time tables |
| `bench.png` | scaling curve vs ideal, each PC alone vs inside the all-PC run, run-to-run spread, time to find k zeros |
| `summary.csv` | median, mean, stdev, CV, 95% CI, min and max per config |
| `tests.csv` | each significance test: gain, p-value, significant |
| `raw.csv` | every single run |

---

## 6. Reading the result

- **VERDICT: adding PCs helps (significant):** every added PC raised throughput, with p < 0.01. This is the proof.
- **p-value:** the chance of seeing a gain this large if the extra PCs did nothing. With 7 repeats the smallest possible value is 1/3432 ≈ 0.0003.
- **Speedup:** all PCs ÷ head alone. The 95% CI is the range the true value very likely falls in.
- **Efficiency:** close to 100% means almost no compute is lost to the network. A drop between two steps means the newly added PC is slow, or it is waiting on the network (check its **Busy** column).
- **CV:** run-to-run spread. Under 2% is consistent. Above that, the report warns you, and you should rerun after closing other programs.

---

## Options

| Option | Default | Meaning |
|---|---|---|
| `--repeats` | 7 | runs per config (minimum 5; more repeats give smaller possible p-values) |
| `--seconds` | 3 | target length of each run; raise it if CV is high |
| `--prefix` | `000000` | hex prefix searched; its hit count is the correctness check |
| `--job` | 1e11 | job size used in the "time for a real job" table |
| `--max-cv` | 2 | CV (%) above which a config is flagged as noisy |
| `--label` | none | tag in the output folder name, e.g. `cpu` or `gpu` |
| `--head` | 127.0.0.1 | head address, if you run the bench from another PC |

## Troubleshooting

| Problem | Fix |
|---|---|
| A worker never shows `+ worker` on the head | Give the worker the head's IP: `./kit rs worker --head HEAD_IP` (Mac: `ipconfig getifaddr en0` shows it) |
| `Cannot reach the ck head dashboard` | `./kit rs head` is not running on this PC |
| `1 ck worker(s) connected` | Start `./kit rs worker` on the other PCs and wait for `+ worker` |
| `no connected worker matches --only` | A PC dropped out mid-bench; restart its worker and rerun |
| `INCONSISTENT RESULT` | A PC returned wrong hashes (faulty GPU driver or hardware). Rerun without `--gpu` to find the PC |
| Noisy CV warning | Close other programs, plug in laptops, rerun with `--seconds 5 --repeats 9` |

`ck run --only IP,IP` (used by the bench) works for normal jobs too: it runs a job only on the listed PCs.
