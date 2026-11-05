# Python SDK: run existing Python code on the cluster

Goal (user, 2026-11-05): run normal Python work (data processing, model training, parameter sweeps) on the lab
cluster with almost no changes to existing code. The SDK profiles the work, picks how to split it, uses only as many
PCs as help, stays within each PC's RAM, and falls back to running locally when the cluster would not be faster.
The Rust `ck` binary stays the engine (head, agents, scheduler); the SDK is a thin client over the head's HTTP API.

Package: `python/clusterkit/` (import name `clusterkit`), **standard library only** (lab PCs are offline; numpy,
pandas, scikit-learn or torch are used only if the user's own code imports them, and only need to exist on the PCs
that run that code). Python >= 3.9. Install: `pip install ./python` or copy the folder.

## 1. Surface

```python
import clusterkit as ck

ck.connect("http://10.0.0.5:7702", token=None)   # or env CK_HEAD / CK_TOKEN; default http://127.0.0.1:7702

# 1. map: the main primitive. Same results, same order as [fn(x) for x in items].
out = ck.map(clean_row, rows)                     # rows: any iterable of picklable values
total = ck.map(count_words, files, reduce=sum)    # reduce runs on your PC over the results

# 2. decorator: existing function, no other change
@ck.remote
def featurize(path): ...
feats = featurize.map(paths)                      # cluster
one = featurize(path)                             # still a normal local call

# 3. a parameter sweep (embarrassingly parallel training)
best = ck.sweep(train_and_score, grid={"lr": [1e-3, 1e-2], "depth": [4, 8, 12]}, key=lambda r: r["score"], top=1)

# 4. data-parallel training by model averaging (local SGD / federated averaging)
params = ck.train(step=train_on_shard,            # step(params, shard) -> new params (runs on a PC)
                  init=params0, shards=shards,    # one shard per task; shards stay where they are sent
                  rounds=10, average=ck.mean_params)  # default mean over dicts/lists of floats

# 5. plan without running: what the optimizer would do and why
print(ck.plan(clean_row, rows))
```

Options on every call (all optional; `ck.config(...)` sets defaults): `pcs` (max PCs; default auto), `placement`
(`auto` | `spread` | `pack` | `fastest` | `efficient`), `priority` (`urgent`..`background`), `selector` (ORG-04,
e.g. `"lab=A !virtual"`), `local` (`auto` | `always` | `never`), `mem_mb` (per item, default measured),
`timeout_s`, `retries`, `name`.

## 2. How it runs (no change to the user's code)

- **Code shipping:** the SDK sends the source of the module that defines `fn`, usually the user's script, plus the
  function's qualified name. A small pushed runner (`python3`, under the head's push limit `push_max_kb`) loads it
  as a module named `__ck_user__` (so `if __name__ == "__main__":` blocks do not run) and aliases it as `__main__`
  so pickled instances of the user's own classes load. Extra local modules can be listed with `ck.config(ship=[...])`.
  If the source does not fit the push limit, the SDK says so and names the knob (`ck admin tune push_max_kb ...`).
- **Data:** each item is pickled and base64-encoded into one list-job item (<= `max_item_bytes`). Bigger items
  are batched or, when that is not possible, the SDK says how to raise `max_item_bytes`, or to pass paths that
  every PC can read instead of the data itself. ponytail: no blob store yet; add one (content-addressed, on the head)
  when shards outgrow item limits.
- **Results:** the runner prints one base64 pickle line per item. The SDK decodes, restores the order and raises
  the first remote exception with its remote traceback (`ck.RemoteError`).
- **Job:** a list job (`kind: list`, `cmd` = the runner, `items`) with `reducer` unset, plus the hints below.

## 3. Auto-optimizer (`ck.plan`)

1. **Profile locally:**
   - run `fn` on a sample of k items (default 3 or more, up to 1 % or 2 s);
   - measure seconds per item, result bytes, peak RSS growth (`resource` / `tracemalloc`) and whether `fn` releases
     the GIL (thread vs process timing).
2. **Read the cluster:** from `GET /api/v1/cluster`, get the awake PCs, their threads, RAM and calibrated speed
   relative to this PC, and from `GET /api/v1/estimate` the queue.
3. **Decide:**
   - **Local:** run locally (serial, or multiprocessing) when the estimated cluster time plus overhead (job submit,
     code shipping, about 1 s plus 2 x RTT per task) is not at least 1.5 x faster.
   - **PC count:** otherwise `pcs = min(awake + asleep matching, ceil(total_work / (target_wall_s x per-PC
     throughput)))`. The SDK never asks for more PCs than make the job finish meaningfully sooner (each extra PC must
     cut the estimate by >= 5 %).
   - **Memory:** `mem_mb` = measured per-item RSS x 1.5, so an agent runs at most `free RAM / mem_mb` copies at once.
   - **Placement:** `fastest` for short jobs (under 60 s estimated), `efficient` when the job is `background`,
     `spread` otherwise.
4. **Explain:** the decision is returned (`ck.plan`) and logged at INFO: "1,200 items x 0.8 s: cluster, 6 PCs,
   about 2.9 min (local: 16 min); 3 PCs asleep will be woken".

## 4. New job fields the head honours (wave 10b, Rust)

`POST /api/v1/jobs` gains optional fields (the SDK sends them; older heads ignore them):

| Field | Meaning |
| --- | --- |
| `max_pcs` | use at most this many PCs (0 or absent = no limit). With power eco mode, the head wakes only enough asleep PCs to reach it. |
| `placement` | `spread` (default today: every matching PC), `pack` (fill fewer PCs fully first), `fastest` (highest calibrated speed first), `efficient` (fewest PCs that meet the estimate; keeps the rest asleep) |
| `mem_mb` | per-copy memory need; an agent runs at most floor(free RAM x 0.8 / mem_mb) copies of a chunk at once |

## 5. Acceptance

- Unit tests (stdlib `unittest`, offline): plan decisions (local vs cluster, pcs, mem), item encode/decode
  round trip, order restore, remote error propagation, the runner on a fake chunk via stdin.
- Live: against a head with 6 virtual agents, `ck.map` over 2,000 items gives the same results as the local list
  comprehension; `ck.sweep` over 12 configurations; `ck.train` 5 rounds of averaged linear-regression SGD that
  reaches the same loss as local full-batch within 5 %; `ck.plan` picks local for a 10 ms job.
- Examples in `examples/sdk/`: CSV cleaning, word count, sweep, model averaging (pure Python), each runnable as
  `python3 examples/sdk/<x>.py`.
