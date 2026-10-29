# Query latency, 1 computer vs 3 nodes (single machine)

All processes run on one Mac (the 3 "nodes" are 3 processes sharing the same CPU and disk), so this measures the overhead of the fan-out/merge path, not the speedup of three real computers. Same rows in both setups (3 clips, 3 recordings). Median and worst of 10 runs, ms, via `POST /api/v1/queries`.

| setup | person | frames | lua_count | lua_frames |
|---|---|---|---|---|
| single store (1 computer) | 2 (max 2) | 2 (max 2) | 2 (max 2) | 2 (max 2) |
| 3 nodes | 3 (max 4) | 3 (max 3) | 4 (max 4) | 3 (max 4) |

Rounded to whole milliseconds. The data set is tiny (3 clips, 475 detections), so both columns are HTTP overhead: the federated path (3 parallel node calls plus a 3-way merge) added about 1 to 2 ms here. This says nothing about large indexes or about real network latency between computers; a speed-up from three computers cannot be measured on one machine.
