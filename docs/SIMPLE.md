# Simple example: one computer vs the cluster

[examples/simple_sha256.py](../examples/simple_sha256.py) hashes every number 1..N with SHA-256, counts hashes starting with `0000`, and prints the time taken. Same code, three ways to run it.

## One computer

```
python examples/simple_sha256.py                 # 1 core
python examples/simple_sha256.py --cores         # all cores of this computer
python examples/simple_sha256.py --n 5e7         # bigger job
```

## Network cluster

1. Head PC:
   ```
   ./kit py head
   ```
2. Every other PC (same network):
   ```
   ./kit py worker
   ```
3. Head PC, run the job:
   ```
   ./kit py run examples/simple_sha256.py --cluster --n 5e7
   ```

The last output line `Ran on: cluster (N workers)` shows how many PCs joined. Use a big `--n`; a small job finishes before the network pays off.

The job runs from the head PC only. Workers just need the kit folder set up (see the README).

## Compare

Run the same `--n` three times and compare `Time taken`:

```
python examples/simple_sha256.py --n 5e7
python examples/simple_sha256.py --n 5e7 --cores
./kit py run examples/simple_sha256.py --n 5e7 --cluster
```

(`examples/simple_time.py` is the same idea with prime counting.)
