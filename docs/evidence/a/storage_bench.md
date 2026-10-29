# Storage benchmark (Agent A)

Machine: Apple M5 (`sysctl -n machdep.cpu.brand_string`), macOS, SQLite 3 bundled via rusqlite 0.32.1.
Command: `CARGO_TARGET_DIR=rust/target-a cargo test --release --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline -- --nocapture storage::`
Generated index: 5000 observations x 3 detections + 1 relation, 5 media. Each batch = one `commit_batch` transaction (observation, detections, relation, frame summary, commit seq, job checkpoint).

| Measurement | Result |
| --- | --- |
| Write, in-memory SQLite, 5000 batches | 2996 batches/s (1.67 s) |
| Write, file-backed WAL (synchronous=NORMAL), 2000 batches | 900 batches/s (2.22 s) |
| Filtered page query (class + min_conf + media + time window, 100 rows), avg of 50 | 1.08 ms |
| `count_detections` class=person, conf>=0.6 over 15000 detections | 0.21 ms (2834 rows) |

Note: a debug build (bundled SQLite compiled unoptimized, 8 agents compiling concurrently) measured only ~51-94 batches/s; numbers above are release.
Single connection behind a mutex; correctness checked against an independent in-test count for every class.
