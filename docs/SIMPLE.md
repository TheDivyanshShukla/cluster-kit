# Simple example: one computer vs the cluster

[examples/simple_sha256.py](../examples/simple_sha256.py) hashes every number 1..N with SHA-256, counts hashes starting with `0000`, and prints the time taken. Same code, three ways to run it.

## One computer

```
python examples/simple_sha256.py                 # 1 core
python examples/simple_sha256.py --cores         # all cores of this computer
python examples/simple_sha256.py --n 5e7         # bigger job
```

## Network cluster

Works with either engine. `--cluster` uses whichever head is running on this PC.

1. Head PC:
   ```
   ./kit py head        # Dask engine
   ./kit rs head        # or: Rust engine
   ```
2. Every other PC (same network), same engine as the head:
   ```
   ./kit py worker
   ./kit rs worker
   ```
3. Head PC, run the job:
   ```
   ./kit py run examples/simple_sha256.py --cluster --n 5e7
   ```

The last output line `Ran on: cluster (N workers)` shows how many PCs joined. Use a big `--n`; a small job finishes before the network pays off.

The job runs from the head PC only. Workers just need the kit folder set up (see the README). With the Rust engine, each worker runs `uv run --project python examples/simple_sha256.py START END` from its kit folder, so the file must be there on every PC. That Python start-up per chunk makes the Rust path slower than Dask for this Python job.

## No shared files: the head sends the script

With the Rust engine, `simple_sha256.py` runs on each worker from the worker's own kit folder, because it imports the kit's `python/` code. If workers do not have the kit folder (and you have no shared folder), let the head send the program itself. `examples/sha256_chunk.py` is a stand-alone version (standard library only); `--push` sends it to every worker, which saves it once in its temp folder and runs it:

```
./kit rs head                                    # head PC
./kit rs worker                                  # other PCs (only the ck program is needed)
rust/target/release/ck run --n 5e7 --from 1 --chunk 1e6 --push examples/sha256_chunk.py | awk '/^[0-9]+$/ {s+=$1} END {print s}'
```

- `--push FILE` works for any script under 64 KB. It picks `python3` for `.py`, `node` for `.js` and `sh` for `.sh`; use `--with "interpreter"` for anything else. The worker PC must have that interpreter installed.
- The built-in search `ck run --n 1e9 --sha 00000` needs no files on workers at all.
- Checked on one Mac with a worker started from an empty folder: the 2,000,000 numbers gave 36 hits, the same as the single-computer run.

## Compare

Run the same `--n` three times and compare `Time taken`:

```
python examples/simple_sha256.py --n 5e7
python examples/simple_sha256.py --n 5e7 --cores
./kit py run examples/simple_sha256.py --n 5e7 --cluster
```

(`examples/simple_time.py` is the same idea with prime counting.)
