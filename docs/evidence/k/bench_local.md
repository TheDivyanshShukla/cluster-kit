# Local benchmark (SINGLE MACHINE, not a cluster result)

`scripts/cluster_bench.sh -k 2 -s 10 people-detection.mp4 person-bicycle-car-detection.mp4`, run 2026-10-29T21:04:37Z on Darwin arm64. Head and every worker are processes on this one Mac, with the Mac's settings from vision/watch.json. Workers share its CPU and accelerator; a gain only means one worker was not using the whole machine. Real speed-up needs separate PCs.

```
>> 1 worker(s): running benchmark
   b1a1897ab8da: done
>> 2 worker(s): running benchmark
   b1a1897afa53: done

SINGLE-MACHINE RUN: all workers share one CPU, so little or no speed-up is expected. This is not a cluster result.
run             PCs      time    x real     inferred  speed-up efficiency  workers
1 worker(s)       1     13.2s      7.9x          400     1.00x       100%  w1 99%
2 worker(s)       2      8.1s     12.8x          400     1.62x        81%  w1 48%, w2 51%
```
