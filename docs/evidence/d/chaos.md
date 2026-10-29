# Sharded search chaos test (real processes, one machine)

Run: 2026-10-30 02:46:05, 94 s, `scripts/shard_chaos.sh`. Head + 3 workers as separate OS processes on one Mac, each with its own CW_ROOT and shard.db, CW_SHARDS=1, per-node deadline 2500 ms. Binary: `/Users/divyansh/Desktop/code/divy/cluster-kit/rust/target-c2/release/cw`.

| result | check | detail |
|---|---|---|
| PASS | A1 three nodes announce and answer | {"n1": "ok", "n2": "ok", "n3": "ok"} |
| PASS | A2 all jobs complete | 10s |
| PASS | A3 recordings are spread over the nodes (each row lives on its ingesting computer) | {"media_b87be4cb3a7bc4108a356b19": "n1", "media_8d566c22764ca2abb76327f2": "n2", "media_4c54894dbe6d9e129a187424": "n3"} |
| PASS | A4 head catalog holds no observations (rows live on nodes) | head node hidden from the node list while empty: n1,n2,n3 |
| PASS | B0 person: all nodes answered | count=390 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 car_sql: all nodes answered | count=54 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 tracks: all nodes answered | count=36 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 frames: all nodes answered | count=256 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 relations: all nodes answered | count=0 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 distinct: all nodes answered | count=4 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 lua_count: all nodes answered | count=390 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B0 lua_frames: all nodes answered | count=96 nodes=['n1:ok', 'n2:ok', 'n3:ok'] |
| PASS | B1 person: federated == single store | count=390 matches=13 |
| PASS | B1 car_sql: federated == single store | count=54 matches=2 |
| PASS | B1 tracks: federated == single store | count=36 matches=13 |
| PASS | B1 frames: federated == single store | count=256 matches=13 |
| PASS | B1 relations: federated == single store | count=0 matches=0 |
| PASS | B1 distinct: federated == single store | count=4 matches=0 |
| PASS | B1 lua_count: federated == single store | count=390 matches=0 |
| PASS | B1 lua_frames: federated == single store | count=96 matches=6 |
| PASS | C0 full federated traversal is complete and ordered | 475 rows |
| PASS | C1 queries ran before and after the kill | before=90 after=363 |
| PASS | C2 every answer is HTTP 200 | [200] |
| PASS | C3 every answer inside the deadline (2500 ms + 1.5 s slack) | slowest 0.02s |
| PASS | C4 answers before the kill are complete |  |
| PASS | C5 answers after the kill carry partial:true and a down node | 325 answers |
| PASS | C6 partial answers carry the warning text |  |
| PASS | C7 surviving nodes' rows are exact (equal to the full result minus the dead node's recordings) | person: 8 of 13 windows |
| PASS | C8 a cursor taken before the kill still pages while the node is down (partial flagged) | 0.01s |
| PASS | C9 n2 comes back and is ok again |  |
| PASS | C10 the pre-kill cursor completes the traversal after restart (no gaps, no duplicates, same order) | 475 rows |
| PASS | C11 results are complete again after the restart |  |
| PASS | D1 hung node: answered inside the deadline with partial:true | 1.01s, nodes=['n1:ok', 'n2:ok', 'n3:down'] |
| PASS | D2 node resumes |  |
| PASS | D3 results complete after resume |  |
| PASS | F0 three nodes up |  |
| PASS | F1 indexing is under way on a node | m1 |
| PASS | F2 query during the outage is partial and names the dead node | [["m1", "down"], ["m2", "ok"], ["m3", "ok"]] |
| PASS | F3 every job completes after failover (the lost node's cameras moved) |  |
| PASS | F4 no task is held by the dead worker | {None} |
| PASS | F5 outage persists for the lost shard: still partial while the node is down |  |
| PASS | F6 restarted node is ok |  |
| PASS | F7 complete (partial:false) once the node is back | count=440 |
| PASS | F8 traversal after failover has no duplicate keys and is ordered | 528 rows |
| PASS | F9 recordings that were mid-index on the dead node now belong to a surviving node | in flight at kill: 2; owners now {"media_fa509b97185710f0cec83d72": "m3", "media_42f198f14b50ceea7bfbf582": "m3", "media_fa6d21ad02a9d8644c88f2a3": "m2", "media_b87be4cb3a7bc4108a356b19": "m2", "media_8d566c22764ca2abb76327f2": "m3", "media_4c54894dbe6d9e129a187424": "m2"} |
| PASS | F10 nodes re-announce to the restarted head |  |
| PASS | F11 queries work right after the head restart and are complete |  |
| PASS | F12 no index event delivered twice across the head restart | 571 index events, 571 distinct (node, commit) |
| PASS | F13 alerts: unique in the store, every streamed alert is stored, no state change streamed twice | stored=15 streamed changes=24 distinct=24 |

**49/49 checks passed.**

Everything ran on a single machine: the killed/hung computers are separate processes, but they share one CPU, one disk and the loopback network. Behaviour across real LAN links (latency, partitions) is not covered here.

## Log

```
== A. index 3 clips on 3 sharded workers
PASS A1 three nodes announce and answer - {"n1": "ok", "n2": "ok", "n3": "ok"}
PASS A2 all jobs complete - 10s
nodes: {"n1": [1, 122], "n2": [1, 267], "n3": [1, 122]}
media owners: {'media_b87be4cb3a7bc4108a356b19': 'n1', 'media_8d566c22764ca2abb76327f2': 'n2', 'media_4c54894dbe6d9e129a187424': 'n3'}
PASS A3 recordings are spread over the nodes (each row lives on its ingesting computer) - {"media_b87be4cb3a7bc4108a356b19": "n1", "media_8d566c22764ca2abb76327f2": "n2", "media_4c54894dbe6d9e129a187424": "n3"}
PASS A4 head catalog holds no observations (rows live on nodes) - head node hidden from the node list while empty: n1,n2,n3
== B. federated == single store (same rows)
PASS B0 person: all nodes answered - count=390 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 car_sql: all nodes answered - count=54 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 tracks: all nodes answered - count=36 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 frames: all nodes answered - count=256 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 relations: all nodes answered - count=0 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 distinct: all nodes answered - count=4 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 lua_count: all nodes answered - count=390 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B0 lua_frames: all nodes answered - count=96 nodes=['n1:ok', 'n2:ok', 'n3:ok']
PASS B1 person: federated == single store - count=390 matches=13
PASS B1 car_sql: federated == single store - count=54 matches=2
PASS B1 tracks: federated == single store - count=36 matches=13
PASS B1 frames: federated == single store - count=256 matches=13
PASS B1 relations: federated == single store - count=0 matches=0
PASS B1 distinct: federated == single store - count=4 matches=0
PASS B1 lua_count: federated == single store - count=390 matches=0
PASS B1 lua_frames: federated == single store - count=96 matches=6
== E. latency (all processes on one machine)
== C. kill -9 n2 while queries run
PASS C0 full federated traversal is complete and ordered - 475 rows
PASS C1 queries ran before and after the kill - before=90 after=363
PASS C2 every answer is HTTP 200 - [200]
PASS C3 every answer inside the deadline (2500 ms + 1.5 s slack) - slowest 0.02s
PASS C4 answers before the kill are complete
PASS C5 answers after the kill carry partial:true and a down node - 325 answers
PASS C6 partial answers carry the warning text
PASS C7 surviving nodes' rows are exact (equal to the full result minus the dead node's recordings) - person: 8 of 13 windows
PASS C8 a cursor taken before the kill still pages while the node is down (partial flagged) - 0.01s
PASS C9 n2 comes back and is ok again
PASS C10 the pre-kill cursor completes the traversal after restart (no gaps, no duplicates, same order) - 475 rows
PASS C11 results are complete again after the restart
== D. hung node (SIGSTOP) stays inside the deadline
PASS D1 hung node: answered inside the deadline with partial:true - 1.01s, nodes=['n1:ok', 'n2:ok', 'n3:down']
PASS D2 node resumes
PASS D3 results complete after resume
== F. kill -9 a worker mid-index; kill -9 the head mid-index
PASS F0 three nodes up
PASS F1 indexing is under way on a node - m1
killed m1 at 51s into the run
PASS F2 query during the outage is partial and names the dead node - [["m1", "down"], ["m2", "ok"], ["m3", "ok"]]
PASS F3 every job completes after failover (the lost node's cameras moved)
PASS F4 no task is held by the dead worker - {None}
PASS F5 outage persists for the lost shard: still partial while the node is down
PASS F6 restarted node is ok
PASS F7 complete (partial:false) once the node is back - count=440
PASS F8 traversal after failover has no duplicate keys and is ordered - 528 rows
PASS F9 recordings that were mid-index on the dead node now belong to a surviving node - in flight at kill: 2; owners now {"media_fa509b97185710f0cec83d72": "m3", "media_42f198f14b50ceea7bfbf582": "m3", "media_fa6d21ad02a9d8644c88f2a3": "m2", "media_b87be4cb3a7bc4108a356b19": "m2", "media_8d566c22764ca2abb76327f2": "m3", "media_4c54894dbe6d9e129a187424": "m2"}
-- head restart
PASS F10 nodes re-announce to the restarted head
PASS F11 queries work right after the head restart and are complete
PASS F12 no index event delivered twice across the head restart - 571 index events, 571 distinct (node, commit)
PASS F13 alerts: unique in the store, every streamed alert is stored, no state change streamed twice - stored=15 streamed changes=24 distinct=24
```
