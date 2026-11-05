# Simulator validation (SIM-04)

The simulator (`ck sim`, rust/src/sim.rs, using the real `sched::chunk_len`) is compared with the measured points before any study number is used.
Rule: throughput and efficiency of the 4-PC paper benchmark and the local head points within 10 %. CV rows are informational.

| Point | Measured | Simulated | Unit | Error % | Result |
| --- | ---: | ---: | --- | ---: | --- |
| 2 PCs throughput (mean of runs) | 44.69 | 43.26 | MH/s | -3.2 | pass |
| 2 PCs efficiency vs sum of single PCs | 101 | 97.76 | % | -3.2 | pass |
| 2 PCs CV of the repeats | 2.798 | 2.718 | % | -2.9 | info |
| 3 PCs throughput (mean of runs) | 67.2 | 65.78 | MH/s | -2.1 | pass |
| 3 PCs efficiency vs sum of single PCs | 99.41 | 97.32 | % | -2.1 | pass |
| 3 PCs CV of the repeats | 2.403 | 1.55 | % | -35.5 | info |
| 4 PCs throughput (mean of runs) | 87.85 | 87.98 | MH/s | +0.1 | pass |
| 4 PCs efficiency vs sum of single PCs | 96.61 | 96.76 | % | +0.1 | pass |
| 4 PCs CV of the repeats | 1.418 | 1.205 | % | -15.0 | info |
| 1 PC throughput | 23.32 | 23.31 | MH/s | -0.1 | pass |
| head ceiling, 200 agents (measured range 2100-2670) | 2385 | 2274 | tasks/s | -4.7 | pass |
| head ceiling, 1000 agents (measured range 1560-1900) | 1730 | 1648 | tasks/s | -4.8 | pass |
| adaptive 10 s-ideal job, 200 agents | 11 | 10.88 | s | -1.1 | pass |
| adaptive 10 s-ideal job, number of parts | 4919 | 4760 | parts | -3.2 | pass |

**Overall: PASS - the study below may be used**

How the model is calibrated (the only fitted numbers):

- Head PC rate 20.92 MH/s and worker rate 23.34 MH/s are the means of the paper's seven single-PC runs; run-to-run CV is 4.78 % for the head PC and 0.55 % for workers (raw.csv). The 2-4 PC results are predictions: nothing is fitted to them.
- Head service time per task = 379 us + 0.199 us x connected agents. Two measured saturated points (200 agents: 2,100-2,670 tasks/s; 1,000 agents after the fix: 1,560-1,900 tasks/s) fix the two numbers (mid-points 2,385 and 1,730 tasks/s give 419 us and 578 us).
- Not validated here (inputs, taken from measurements): standby promotion 5.7 s, planned switchover 3.7 s. The measured 'job done 11.5 s after the kill' (10 virtual agents) is 5.7 s promotion plus the work that remained, so it is consistent with, not independent of, the input.
- Not validated (needs the lab): anything above 200 agents with real PCs, per-PC speed spread of a mixed lab, power and wake latency, failure rates.
- Extrapolation: the per-agent head cost is a straight line through two points; beyond 1,000 agents on one head (the 10,000 case) it is an extrapolation.
