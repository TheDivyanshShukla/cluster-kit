# Watch swarm plan (Wave 0 frozen 2026-10-29)

Product spec: `docs/VISION_IMPLEMENTATION_PLAN.md`. Agent sheet: `docs/VISION_AGENT_TASKS.md`. Read both once; this file overrides them where it is more specific.

## Baseline

- Shared working directory, no worktrees, no commits by agents. The coordinator commits.
- `cargo test --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline` = 53 passing at Wave 0.
- Unrelated changes (do not touch, do not stage): `examples/*.py`, `python/cluster.py`, `results_*.csv`, `scaling.png`.
- Test media: `vision/footage/people-detection.mp4`, `vision/footage/person-bicycle-car-detection.mp4`. Model: `vision/models/yolov5n.onnx` (COCO 80, no weapon, no holding). FFmpeg/ffprobe at `/opt/homebrew/bin`.
- Legacy data: `vision/data/seen.jsonl` (2366 lines), `events.jsonl`, `audit.jsonl`, `thumbs/`. Never modify. Tests use `CW_ROOT=<tmpdir>`.

## Frozen contracts

- `rust/src/watch/domain.rs` — every shared type, `Store` trait, `ApiError`, `IndexBatch`, ID derivation (`observation_id`, `detection_id`, `stable_id`). Read it fully before writing code.
- `rust/src/watch/contracts/*.v1.json` — fixtures; `domain::tests::fixtures_parse` keeps them valid.
- Time = `i64` microseconds. `media_time_us` always; `capture_time_utc_us` optional and never guessed.
- Error envelope on every `/api/v1` failure: HTTP `ApiError::status()` + body `{"error": ApiError}`.
- Only the coordinator edits `domain.rs`, `Cargo.toml`/`Cargo.lock`, `mod.rs`, `head.rs`, `config.rs`, `ui/app.css`, `ui/app.js`, nav markup shared by all pages. Need a change there? Put it in your report under "Integration notes" with exact code; do not edit.
- Approved dependencies: everything already in Cargo.toml plus `rusqlite =0.32.1 (bundled)`. Nothing else. Build is `--offline`.

## Build hygiene (8 agents share one crate)

- Use your own target dir to avoid lock contention: `CARGO_TARGET_DIR=rust/target-<letter>` (pre-cloned from `rust/target`).
  Example: `CARGO_TARGET_DIR=rust/target-a cargo test --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline storage::`
- Keep the crate compiling after every edit. Your module stubs are already registered in `mod.rs` with `#[allow(dead_code)]`. Add submodules inside your own directory (`storage/sqlite.rs` declared from `storage/mod.rs`).
- If the build breaks in a file you do not own, wait ~1 min and retry; never edit it.
- Run tests filtered to your module while iterating; run the full suite before reporting.

## Ownership and exported API (coordinator wires these into head.rs `/api/v1`)

| Agent | Owns | Must export (exact names) |
| --- | --- | --- |
| A Storage | `storage/**`, `media_catalog.rs` | `storage::open(path: &Path) -> Result<Arc<dyn Store>, ApiError>` (SQLite WAL, migrations); `storage::memory() -> Arc<dyn Store>` (same semantics, for other agents' tests; may be SQLite `:memory:`); `storage::legacy::import(store: &dyn Store, data_dir: &Path) -> Result<ImportReport, ApiError>` (restartable, idempotent); `media_catalog::probe(source_id: &str, path: &Path) -> Result<MediaAsset, ApiError>` (ffprobe + fingerprint, `still` for images, `Missing` when absent); `media_catalog::media_id(source_id, fingerprint) -> String` |
| B Ingest/cluster | `worker.rs`, `decode.rs`, `split.rs`, `net.rs`, `ingest/**`, `scheduler/**` | `ingest::manifest(source: &domain::Source) -> Result<Vec<PathBuf>, ApiError>` (deterministic, sorted, recursive opt-in); `ingest::batch_from_frame(...) -> IndexBatch` used by worker; `scheduler::Scheduler` (`new(lease_us)`, `register(WorkerCaps)`, `heartbeat(worker)`, `submit(IndexJob, tasks)`, `lease(worker) -> Option<TaskLease>`, `check(&LeaseRef) -> Result<(), ApiError>` stale generation = `stale_lease`, `complete(task_id, gen)`, `fail(task_id, gen, permanent: bool)`, `reap(now_us)`). Worker posts `IndexBatch` JSON to `POST /api/v1/index` and falls back to legacy `/api/index` on 404. |
| C Models | `yolo.rs`, `track.rs`, `fight.rs`, `gate.rs`, `models/**`, `docs/MODELS.md`, `vision/eval/**` | `models::validate(module: &config::Module) -> Result<ModelVersion, ApiError>` (opens ONNX, checks output layout vs label count, hashes artifact, effective provider); `models::fuse(dets: Vec<Detection>, iou: f32) -> Vec<Detection>` (class-aware cross-module dedup, provenance `fused:<ids>`); `models::relate(obs_id, dets: &[Detection], installed: &[ModelVersion]) -> Vec<Relation>` (`near` only unless a holding module exists); `models::support(installed: &[ModelVersion], predicate) -> Support`; `models::summarize(obs: &FrameObservation, dets, rels, analyzed_classes: &[String], min_conf) -> FrameSummary` (distinct counts; classes not analyzed are absent = Unknown) |
| D Query/matches | `query.rs`, `query/**` (convert to `query/mod.rs` if needed), `matches.rs` | `query::v1::run(store: &dyn Store, req: &query::v1::Request) -> Result<(QueryRun, Vec<ReviewMatch>), ApiError>`; `query::v1::timeline(store, run_id, start_us, end_us, resolution_us) -> Result<serde_json::Value, ApiError>`; `matches::windows(rows: &[(FrameObservation, Detection)], media: &HashMap<String, MediaAsset>, opts: &matches::Opts, query_run_id) -> Vec<ReviewMatch>` |
| E Rules/alerts | `rules.rs`, `alerts/**` | `alerts::Engine::new(rules: Vec<alerts::RuleSpec>)`, `engine.observe(&FrameSummary) -> Vec<Alert>` (state transitions only), `alerts::RuleSpec::from_legacy(&config::Rule)`, `alerts::backtest(store: &dyn Store, spec, scope: &QueryScope) -> Result<Vec<Alert>, ApiError>` |
| F Review UI | `ui/review.html`, `ui/review.js`, `ui/review.css`, `ui/timeline.js`, `ui/timeline.css`, `docs/REVIEW_UI.md` | Page served at `/review?run=<query_run_id>`; fixture mode `?fixture=1` reads `ui/fixtures/review.json` (owned by F, clearly labelled TEST DATA). |
| G Product UI | `ui/sources.*`, `ui/find.*`, `ui/setup.*`, `ui/logs.*`, `ui/wall.*`, `ui/product.css` | Search sends `POST /api/v1/queries` and navigates to `/review?run=<id>`. |
| H Security/acceptance | `auth/**`, `scripts/watch_acceptance.sh`, `docs/WATCH_OPERATIONS.md`, `docs/RELEASE_CHECKLIST.md` | `auth::Auth::load(root: &Path) -> Auth`; `auth.check(method: &str, path: &str, headers: &[(String, String)]) -> Result<auth::Principal, ApiError>`; `auth::Role`; login/logout/bootstrap handlers as pure fns returning `(status, headers, body)`; `auth::redact(url: &str) -> String`; `auth::allowed_media(root: &Path, path: &Path) -> bool` |

## Target `/api/v1` routes the coordinator will wire (UI agents build against these)

- `POST /api/v1/index` body `IndexBatch` -> `BatchOutcome`
- `GET /api/v1/sources`, `POST /api/v1/sources` (Source + import -> IndexJob), `GET /api/v1/jobs`, `GET /api/v1/jobs/{id}`
- `GET /api/v1/models` -> `[ModelVersion]` with `support` map; `GET /api/v1/capabilities`
- `POST /api/v1/queries` body `{"sql"?: str, "class"?: str, "min_confidence"?: f32, "scope": QueryScope, "gap_us"?: i64, "pre_us"?: i64, "post_us"?: i64}` -> `{"run": QueryRun, "matches": [ReviewMatch] (first page), "next": cursor|null}`
- `GET /api/v1/queries/{id}`, `GET /api/v1/queries/{id}/matches?cursor=&limit=`
- `GET /api/v1/queries/{id}/timeline?start_us=&end_us=&resolution_us=` -> `{"lanes":[{"source_id","media_id","clock","label","duration_us","capture_start_utc_us","availability","matches":[ReviewMatch]}],"gaps":[...]}`
- `GET /api/v1/media/{id}` -> MediaAsset; `/stream` (Range); `/frame?t_us=` (JPEG, exact seek); `/thumbnails?start_us=&end_us=&step_us=` -> `{"sprite": url, "tile_w", "tile_h", "cols", "samples":[{"t_us","x","y"}]}`; `/observations?start_us=&end_us=` -> `[{"observation", "detections", "relations"}]`
- `POST /api/v1/reviews`, `GET /api/v1/reviews`, `GET/PATCH /api/v1/reviews/{id}` (PATCH needs `version`), `POST /api/v1/reviews/{id}/manifest` -> evidence manifest JSON
- `POST /api/v1/exports`, `GET /api/v1/exports/{id}`, `POST /api/v1/exports/{id}/cancel`
- `GET /api/v1/alerts?cursor=`, `POST /api/v1/alerts/{id}/decision`, `POST /api/v1/rules/backtest`
- `GET /api/v1/audit?q=&cursor=`
- `GET /api/v1/events` SSE (`id:` cursor; `job`, `alert`, `index` events)
- `GET /api/v1/auth/me`, `POST /api/v1/auth/login`, `POST /api/v1/auth/logout`, `POST /api/v1/auth/bootstrap`

## Report format (final message, <= 60 lines)

1. Files created/changed.
2. Exact test commands + pass counts (paste the `test result:` line).
3. Real-media / real-browser evidence (paths to screenshots/logs under `docs/evidence/<letter>/`).
4. Integration notes: exact code the coordinator must put in head.rs/mod.rs/Cargo/app.js.
5. Remaining limitations, stated plainly.
