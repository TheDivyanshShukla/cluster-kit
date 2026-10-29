# Query latency, 1 computer vs 3 nodes (single machine)

All processes run on one Mac (the 3 "nodes" are 3 processes sharing the same CPU and disk), so this measures the overhead of the fan-out/merge path, not the speedup of three real computers. Same rows in both setups (3 clips, 3 recordings). Median and worst of 10 runs, ms, via `POST /api/v1/queries`.

| setup | person | frames | lua_count | lua_frames |
|---|---|---|---|---|
| single store (1 computer) | 1.8 (max 2.0) | 1.7 (max 1.8) | 2.2 (max 2.2) | 2.3 (max 2.4) |
| 3 nodes | 3.4 (max 5.4) | 2.6 (max 2.9) | 3.6 (max 3.7) | 3.2 (max 3.3) |
