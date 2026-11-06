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
- **Batching:** tiny items are grouped, about `batch_s` (0.5 s) of work per remote call and at least two calls per slot; one list item then holds a list of items and its result line a list of results (`batch: true` in the payload). Per-item results and order are unchanged; a batch that would exceed `max_item_bytes` is split in half.
- **Results:** the runner prints one base64 pickle line per item (per batch). The SDK decodes, restores the order and raises
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
- Offline tests: `python3 -m unittest discover -s python/tests` (31 tests: batching order and results, code built once, re-plan falls back locally, SSE push, history fit and `learned` in the plan, one fixture per advisor rule).
- Examples in `examples/sdk/`: CSV cleaning, word count, sweep, model averaging (pure Python), each runnable as
  `python3 examples/sdk/<x>.py`.

## 6. Self-optimizing runtime

- **Auto-batching** (section 2): `ck.config(batch=False)` or `batch=False` per call turns it off; `batch_s` sets the work per call.
- **Round 4: one wait, result inline.** Against a head that holds `GET /jobs/{id}?wait=S&result=1`, the SDK sends one such request per 2 s (5 s with no replan check) instead of polling every 0.03 to 0.3 s, takes a small result from the same reply (no `/result` fetch), does not open the `/stream` connection, reads the asleep count from `/cluster` (no `/nodes`), and reuses the profile of the same function on the same first items for 10 minutes. A head that answers `?wait=` without a `long` field is remembered as old and the rest of this section applies.
- **Results pushed, not polled (older heads):** the SDK keeps one `/api/v1/stream` connection per session and reads `job` events (and the `jobs` rows in `cluster` events). It checks whether pushes beat a poll: after a poll sees a job finish first, the next job judges whether the event followed within 0.15 s. Three wins in the last five and polling drops to a 3 s safety net; otherwise it polls on a 0.03 s to 0.3 s backoff and the events still wake it early. Today's head sends job events 0.2 to 2.4 s late (measured), so today the SDK polls; see the head change in section 8. `ck.config(push=False)` disables the stream.
- **Code shipped once per session:** the pushed command (compressed sources plus runner) is built once per (file, mtime, function, mode) and reused, so `ck.train` rounds do not rebuild it (`ship_s` 0 after the first round). The head still receives the script with every job; a content-addressed push would remove that too (section 8).
- **Adaptive re-plan:** while a job runs the SDK compares progress (`done / total` from the job row) with the plan. After `replan_min_s` (2 s), if the projected time left on the cluster exceeds 1.25 x the whole local time (the finished cluster results would be discarded), it cancels the job and runs locally, logging for example `replan: the cluster is at 12% after 3.0 s and needs about 22.0 s more (plan said 4.0 s in total); finishing locally in about 8.0 s`. Never with `local='never'` or `adaptive=False`. The observed per-slot speed is written to the history, so the next plan already knows (it picks fewer PCs or local). ponytail: a running job cannot shrink to fewer PCs (no API), and partial results are not reused.
- **Learned model:** every run appends predicted vs actual to `~/.clusterkit/history.jsonl` (override the folder with `CK_SDK_HOME`; `history=False` stops it): `head, fn, n, where (cluster|local|replanned), pcs, slots, batch, item_s, pred_s, pred_local_s, actual_s` and, for cluster runs, `encode_s, ship_s, submit_s, queue_s, run_s, detect_s, fetch_s, decode_s, push, fixed_s, item_c, speed`. `fixed_s` is the overhead independent of n, `item_c` the per-item codec cost, `speed` the throughput per slot relative to this PC (runs of at least 1 s). With at least 3 runs on the same head, `plan()` uses the median of the last `learn_last` (20) runs instead of the defaults and says so: `overheads learned from 5 earlier runs (fixed 0.19 s, PC speed 1.00x)`. The cluster estimate is `fixed + n x item_c + (n x item_s + calls x start_s) / (speed x slots)`.

## 7. Advisor

`ck.advise(fn, items, sample=20)` or `python -m clusterkit advise path/script.py:func [--items 'range(1000)'] [--sample 20]` runs `fn` on a sample (so keep side effects in mind: it executes your function on up to `sample` items, twice for five of them, plus a 4-thread probe) and returns a ranked list (blockers, warnings, gains by estimated seconds saved, info). Each suggestion has `id, title, why, hint, severity, gain, gain_s, evidence`.

| id | fires when | evidence |
| --- | --- | --- |
| `unpicklable` | lambda, nested function, no source file, or an argument or result that cannot be pickled | the error |
| `raises` | `fn` raised on a sample item | the error |
| `tiny_items` | under 5 ms per item and 50+ items | per-item time, batch size, calls, start-up cost saved |
| `big_args` / `big_results` | encoded item above half of `max_item_bytes` / result above 64 KB | bytes per item, total MB, transfer time at 20 MB/s |
| `hot_loop` | cProfile: at least 50 % of the time in your own Python code and 0.2 ms or more per item | share, hottest function, builtin share; gain assumes ~10x |
| `io_bound` | CPU time under 50 % of wall time, or 4 threads give 2.5x or more | CPU ratio, thread speed-up |
| `gil_bound` | CPU-bound and 4 threads give under 1.3x | thread speed-up |
| `high_memory` | 100 MB or more per item (tracemalloc / RSS) | peak MB, suggested `mem_mb` |
| `side_effects` | AST of `fn` and same-file helpers: `global`, writes to module-level containers, file writes, `os`/`subprocess`/`requests` calls; or a module global changed during the sample | names |
| `nondeterministic` | two runs on the same input differ (definite), or unseeded `random`/`time`/`uuid` calls (maybe) | differing items, sources |
| `missing_imports` | imports outside the standard library (must exist on every PC), or your own modules beside the script that are not shipped; imports inside `try/except ImportError` are ignored | names |
| `transfer_dominates` | the plan moves blobs and the transfer estimate is at least 40 % of the cluster estimate (and 1 s or more) | bytes (shared / split), strategy, estimate per PC count; try: shard, cache with `ck.put`, fewer PCs |
| `cluster_cannot_help` | the plan is local | total work, per-item time, break-even numbers |

For `examples/sdk/tour.py:clean` (1,000 rows, 20,000 sin/cos per row): `tiny_items` (109 rows per call), `hot_loop` (70 % of the time in `clean()` itself: use numpy), `gil_bound` (threads give 1.0x), `cluster_cannot_help` (about 4 s of work locally, 0.7 s with all cores; break-even: more than about 0.2 s of total work and 0.02 ms per item, which it exceeds, so the blocker is that the awake PCs add no cores beyond this machine's own on a one-box test bed).

## 8. Overhead measurement and break-even (`python -m clusterkit bench`)

The bench submits a forced-cluster job (`local='never'`) over an empty function (`clusterkit._benchfn.noop`) and times each step from the job row timestamps and this PC's clock. Test bed: one Mac running the head and 6 virtual agents (2 threads each) on loopback, shared with other work, median of 5 runs. Real LANs add the round trip on every request and dispatch; run the bench against your own head and let the learned model take over.

| step (s) | 1 item | 100 items | 10,000 items | 10,000 batched (auto) |
| --- | ---: | ---: | ---: | ---: |
| encode (this PC) | 0.0000 | 0.0005 | 0.0510 | 0.0015 |
| build the pushed command (cached after the first) | 0.0000 | 0.0000 | 0.0000 | 0.0000 |
| submit (POST /jobs) | 0.0009 | 0.0013 | 0.0168 | 0.0011 |
| dispatch (created to first piece running) | 0.0010 | 0.0010 | 0.0140 | 0.0000 |
| remote run (python start per piece, loop) | 0.0410 | 1.5630 | 4.4920 | 0.3240 |
| detect (finished to known; polled, push not yet usable) | 0.0471 | 0.1438 | 0.2238 | 0.4258 |
| fetch result | 0.0005 | 0.0005 | 0.0013 | 0.0004 |
| decode (this PC) | 0.0000 | 0.0001 | 0.0117 | 0.0008 |
| **total wall** | **0.089** | **1.708** | **4.808** | **0.724** |
| per item | 89 ms | 17 ms | 481 us | 72 us |

Python start is 21 ms bare and 80 ms with the runner and one item on this machine, paid per piece (one piece per thread per task). Per-item encode is 5 us and decode 1 us; the rest of the unbatched per-item cost is one runner start per list item, which batching removes.

**Break-even rule** (K = 12 slots, F = 0.09 s fixed, 63 us per item batched, 313 us unbatched): the cluster is at least 1.5x faster when **total work > F x 1.5 / (1 - 1.5 / K) = 0.15 s** and **per-item work > 1.5 x c / (1 - 1.5 / K) = 0.11 ms with batching (0.54 ms without)**. These are floors for loopback; on a lab LAN use the learned `fixed_s` (typically 0.3 to 1 s with dispatch latency): `total work > 1.7 x fixed_s` and `per-item > 1.7 x c`. With few slots (K = 4) the factor `1 - 1.5 / K` grows the thresholds 1.6x. Defaults of the plan (`fixed_s=0.1, item_c=1e-5, start_s=0.03`) come from this table; `bench` prints the values for your head.

Tour before and after, forced cluster (`local='never'`, same head, interleaved runs; this machine was loaded, so read ratios, not seconds): plan 0.04 to 0.03 s, `map(clean)` 8.2 to 2.4 s, primes 4.1 to 0.3 s, sweep 0.28 to 0.11 s, train (5 rounds) 2.5 to 0.43 s. Automatic mode keeps all five steps local on this one-box bed (the plan says why).

**Head changes that would help** (not made here):
1. Flush each SSE frame. `Sse::read` returns one frame at a time but tiny_http buffers the chunked body, so `job` events arrive 0.2 to 2.4 s late and response headers only after the first buffer fills. In `api.rs` after `self.frame(...)`, pad each `job` frame to the encoder threshold, e.g. `self.buf.extend(format!(": {}\n\n", " ".repeat(8192)).bytes())`, or serve the stream from a raw TcpStream with `flush()` per frame. The SDK then trusts pushes and stops polling.
2. Treat the job's `chunk` as a cap under the adaptive policy for list jobs: in `sched::chunk_len`, `let v = if fixed_chunk > 0 && p != ChunkPolicy::Fixed { v.min(fixed_chunk) } else { v };` before the final clamp. Today the chunk size comes from the PC's items-per-second on earlier jobs, so a slow list job that follows fast ones gets huge chunks (200 x 0.5 s took 28 to 48 s against about 8 s ideal; steady state 20 s). The SDK already sends `chunk` (about 1 s of work per thread, at least 3 chunks per PC).
3. `policy: "fixed"` and `"guided"` run list jobs almost serially (96 x 0.5 s: 21 s against 8.7 s adaptive); worth a look.
4. A content-addressed push (`cmd_ref`) so the script is uploaded once per session, not with every job.


## 9. Bulk distribution (wave 13; design in docs/BULK.md, head API in docs/API.md "Bulk distribution")

Needs a head with the blob store; with an older head every call below behaves as before (big arguments hit `max_item_bytes`, code ships inline).

```python
h = ck.put(big_table)                      # object or file path -> Handle; uploaded once, same bytes = same blob
ck.map(score, [h] * 100)                   # fn gets the object (or, for a file, its local path on the PC)
ck.map(count, ck.shard("/data/500mb.txt", parts=50))   # 50 line-aligned shard blobs; each goes to the PC that runs it, once
ck.map(fn, ck.shard(rows, parts=8))        # a list: fn gets one slice
```

- **One copy per process:** a handle's object is unpickled once per process and shared by every item that process runs (and by the local profiler), so treat it as read-only; copy it before changing it.
- **Handles in items:** an item that is a handle becomes `@blob:<sha>` (the head places by it and gives the runner a path); handles inside tuples, lists and dicts (two levels, up to 64 entries) are resolved by the runner. Blobs holding objects start with a magic header, so the runner can tell an object from a plain file.
- **Automatic:** an argument whose pickle is over `min(blob_auto_kb, max_item_bytes)` becomes a blob (small containers are opened one level, so a big shared `params` is one blob for all items and `train` rounds do not re-send shards; big objects are remembered by identity, not re-pickled). Module-level data that `fn` reads and that is over `blob_auto_kb` goes as a blob and replaces the global after the module loads. The code (compressed sources) goes as a blob when over `code_blob_kb` (8): the pushed script is just the runner plus the sha, built and uploaded once per session; batching is a header line, so one blob serves both.
- **Bulk results:** a result over `blob_auto_kb` is PUT to the head by the PC (payload `rb`: URL, token, limit) and the result line holds its sha; the SDK fetches them in parallel. If the PC cannot reach the head (`ck.config(result_head=...)` names the URL PCs should use) the result goes inline. `bulk_results=False` turns this off. The runner uploads with the job-scoped capability the head puts in the PC's environment (`CK_BLOB_PUT_URL`, `CK_BLOB_PUT_TOKEN`: PUT `/api/v1/blobs` only, valid while the job runs), so this works on secured heads; only a head without it gets the plain URL in the payload, and then only when no user token is set. The user's own token is never put in a job.
- **Estimate:** `ck.plan()` accounts the blobs of the run: bytes used by several items (and code/data blobs) go to every PC that runs something, bytes used by one item go once; blobs some PC already caches (head `holders`) count as moved. Transfer seconds for k PCs come from `GET /api/v1/estimate?bytes=&work_s=&items=&max_pcs=k` (best strategy's `transfer_s`), asked at power-of-two k and memoised; against a head without the new form, the BULK.md formulas on `head_uplink_mbps` / node `net_mbps` from `/api/v1/net`, else the upload and download speed this SDK measured (history `where: "link"`), else an assumed 100 MB/s (the plan says "assumed link speed"). The cluster estimate per PC count includes it, so a data-heavy job picks fewer PCs. The plan has `bytes, shared_bytes, item_bytes, blobs, strategy, transfer_s, transfer_alt, finish_by_pcs, transfer_src` and `plan.transfer()` prints them.
- **Learning:** every upload and bulk download of 256 KB or more appends bytes and seconds to the history; the learned `fixed_s` excludes the SDK's own upload and download time, so transfer is not counted twice.
- **Typing:** the package ships `py.typed`. `ck.map(fn, items)` returns `list[R]` (or `S` with `reduce=`), `ck.put(x)` a `Handle[T]`, `@ck.remote` a `Remote[P, R]` (argument types kept on Python 3.10+), `sweep` a `list[SweepRow]`, `train` the type of `init`, `plan` a `Plan` dataclass, `advise` an `Advice` (a `list[Suggestion]`). `placement`, `priority` and `local` are `Literal` types.


## 10. Python versions and ML libraries

The SDK is standard library only and runs on Python 3.9 to 3.15 (including the free-threaded 3.13t / 3.14t). The same file also runs on the PCs (`python3` from PATH), so what the PCs have decides what your code may use. Tested with `scripts/py_matrix.sh` (uv venvs, one per version; `python -W error::DeprecationWarning -m unittest discover -s python/tests`) and in CI:

| Python | 3.9 | 3.10 | 3.11 | 3.12 | 3.13 | 3.13t | 3.14 | 3.14t | 3.15 (beta) |
|---|---|---|---|---|---|---|---|---|---|
| SDK tests | pass | pass | pass | pass | pass | pass | pass | pass | pass |
| numpy / pandas / scikit-learn / torch live (`examples/sdk/ml_*.py`) | yes | | | yes | | | yes | | |

**Rules**

- **PC minimum is 3.9.** The pushed runner has no `__future__` import and its annotations must evaluate on 3.9 (`Optional[...]`, not `X | None`). On a PC with older syntax support a job fails with `this PC runs Python 3.9.6 and cannot parse <module>.py line N` instead of a bare SyntaxError.
- **Tell the SDK the oldest PC Python:** `ck.pc_python()` runs a tiny job and returns the versions the PCs have (`['3.9.6', '3.12.4']`, `t` marks free-threaded); `ck.config(pc_python="3.9")` then (1) checks your code (and `ship=` modules) with `ast.parse(feature_version=...)` before anything is sent, and (2) picks the pickle protocol. The check covers the syntax Python's own parser versions (match, `except*`, `type X =`, positional-only, ...); it cannot know about new standard-library calls or library versions. `ck.advise` adds a `python_versions` warning when the PCs differ from this machine.
- **Pickle protocol** is the lower of both sides' `HIGHEST_PROTOCOL`, capped at 5 (protocol 5 needs 3.8+, so every supported pair uses 5; `pc_python` below 3.8 lowers it to 4). The PCs answer in the protocol named in the payload. Large numpy arrays pickle in-band with protocol 5 (one copy instead of two); arrays and models are pickled by their own libraries, so numpy / torch / pandas need compatible majors on both sides (cross-version 3.12 user and 3.9 PCs, and the reverse, was tested with numpy 2.x on 3.12 / 2.0 on 3.9).
- **Free-threading:** `ck.advise` checks `sys._is_gil_enabled()`. With the GIL it reports `gil_bound` when threads give no speed-up (use processes: the cluster already does); on a free-threaded build with the GIL off it reports `threads_ok` when threads do scale (this machine's measurement; the PCs need a free-threaded `python3` too) and never blames the GIL.
- **Local parallel runs** use fork only when this process has a single thread; otherwise forkserver / spawn (the 3.14 default), where `fn` and the items travel by pickle and your script is re-imported in each child. Guard the top level with `if __name__ == "__main__":`; without the guard the SDK runs serially (and says so at INFO level) instead of starting an endless pool.
- **Removed / deprecated stdlib:** the SDK imports only modules present on every version above (no distutils, imp, cgi, pipes, telnetlib, asyncore; no `datetime.utcnow`); the suite runs with `-W error::DeprecationWarning`. `dataclasses` `__replace__` (3.13) and `__annotate__` (3.14, PEP 649/749) are generated members and are skipped by the type-hint test; hints are strings (`from __future__ import annotations`) and resolve with `typing.get_type_hints` on 3.10+.
- **`ck.mean_params`** averages dicts (a torch `state_dict` stays an `OrderedDict`), lists, floats, numpy arrays and torch tensors without importing numpy or torch; integer arrays (BatchNorm `num_batches_tracked`) keep their dtype.

**ML examples** (each file's header lists its packages; the PCs need the same packages installed): `ml_numpy.py` (arrays as arguments, results and blobs), `ml_pandas.py` (`ck.shard` of rows, per-shard groupby, `pd.concat`), `ml_sklearn.py` (`ck.sweep` over a cross-validated random forest, `ck.map` over fits), `ml_torch.py` (CPU, `ck.train` with `state_dict` averaging). Set `CK_LOCAL=never` to force the cluster for these small demos; by default the optimizer keeps them local. CI runs the numpy / pandas / scikit-learn ones on 3.12; torch is run by hand.

## 11. Warm runner (wave 14)

The first task of a job on a PC leaves a per-user warm Python worker that has already run your code's imports (torch loads once, not on every task or train round). Later tasks with the same code and the same `python3` connect to it over a unix socket and the worker forks one child per task, so tasks stay isolated and parallel and results are the same as the plain path. The task that starts the worker runs the plain way (no wait).

- Socket and files live in `<temp>/ck-warm-<uid>/`, mode 0700 and owned by the user (anything else turns the feature off); the worker also refuses any peer whose uid differs (SO_PEERCRED on Linux, LOCAL_PEERCRED on macOS; where neither exists the 0700 directory is the guard).
- Key = hash of the shipped code + `sys.executable` + Python version. Idle exit after 120 s (`CK_WARM_IDLE_S`; the `warm_idle_s` tunable is for the lead to wire). `CK_WARM=off` disables it. Windows: off.
- Any error (no socket, refused, worker died mid-task) falls back to the plain path for the results not yet delivered.
- Measured, 4 virtual agents, `examples/sdk/ml_torch.py` (8 rounds, 6 shards, torch CPU, 3.12): 24-25 s plain, 16 s then 11 s warm; same held-out MSE.


## Blobs per chunk (lab heads and shards)

Items that hold `ck.shard` / `ck.put` handles inside a tuple, a dict or a batch are sent as `@refs:<sha>,..|<line>`: the head reads the shas to send each shard only to the PCs whose chunk uses it (before, only items that were a bare handle were placed per chunk; any other item made every PC fetch every shard). The runner drops the prefix. `ck.plan` therefore counts a blob used by one item as data for one PC (`blob_per_chunk`, now true by default; set `ck.config(blob_per_chunk=False)` against a head from before this change). Jobs through lab heads: see HIERARCHY.md section 7.

## Real-workload examples and what they found

`examples/real/` holds five jobs that people actually run, each with `local` and `cluster` modes and one JSON line `{"digest", "secs"}` that must be identical in both:

| File | Uses | What it does |
|---|---|---|
| `train.py` | `ck.train` | small CNN on 28x28 digits, models averaged each round (torch) |
| `sweep.py` | `ck.sweep` | 240 SVM configurations, 5-fold cross-validated (scikit-learn) |
| `etl.py` | `ck.shard`, `ck.map` | 5-million-row CSV cut into 24 blobs, filtered and grouped on the PCs, merged here (pandas) |
| `cv.py` | `ck.map`, `$CK_OUT` | 5,000 generated images resized, blurred, edge-mapped; PNGs saved as job files (OpenCV, Pillow) |
| `text.py` | `ck.shard`, `ck.map` | word counts and exact duplicate-line removal over 1 GB of text (numpy) |

Put the imports a function needs at the top of the file: a lazy import inside the function defeats the warm runner and inflates the profile of item 0. Numbers and faults: [audit/ROUND4.md](audit/ROUND4.md). Fixed while building them: a crash on results over 64 KB (the SDK's `sum(map(len, ...))` called its own `map`), no retry of GET calls when the head fails over mid-job (`_http` now retries while a job is in flight), blobs not uploaded again after a reconnect, and a profile window that counted import time (item 0 looked 10x too slow). With lab heads on, `/api/v1/cluster` lists each lab head's PCs as `lab_pcs` and the planner counts them; before that fix it saw a lab head as one PC and planned for one.
