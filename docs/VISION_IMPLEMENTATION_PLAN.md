# Watch: Rust video query and review platform

Status: implementation blueprint and agent handoff, 29 October 2026.

This is the agreed product direction. The latest requirement is a video-editor-style review workspace: a query on old footage produces matching clips as strips on a timeline, grouped into camera tracks, with a player, playhead, filmstrip thumbnails, and evidence details. It is a core workflow, not a later dashboard enhancement.

## 1. Product outcome

Run the same application on one PC or a cluster. Connect live cameras, import recordings, or index a folder of images. Extract structured detections once, search that metadata, open the original evidence, and evaluate persistent alert rules as new frames arrive. Rust owns ingestion, inference, indexing, scheduling, query execution, alert evaluation, media delivery, and audit logs. The UI may use any language; keep the existing browser-native JavaScript for the first implementation unless a specific interaction requires a justified change.

Deliver six connected workspaces:

1. **Live:** camera wall, camera detail, source health, active alerts, pause/review actions.
2. **Search & review:** source/time selection, visual query builder and SQL, matched clip strips, synchronized playback and evidence review.
3. **Sources:** cameras, files, image collections, import/index progress, recording availability and retention.
4. **Rules & models:** model registry, capabilities, thresholds, Lua rules, alert review and feedback.
5. **Cluster:** local/cluster switch, worker capabilities, assignments, throughput, queue age and bottlenecks.
6. **Activity:** searchable operational/audit logs, query runs, index jobs, failures, model changes and alert decisions.

The first release is a functional demonstration and engineering foundation. Production deployment is a separate acceptance gate with authentication, authorization, operational recovery, retention controls, deployment hardening, and measured model performance.

## 2. Requirements and exact meanings

### Supported inputs

- Live RTSP and supported HTTP streams; USB/device cameras on a specifically eligible worker.
- Existing video files and finite archive scans.
- Single images and image-folder manifests, with stable item IDs.
- Future adapters for NVR/VMS APIs behind the same source interface. “Any camera system” means an adapter exists for its actual protocol; it is not a universal compatibility guarantee.
- A source group selects multiple cameras, recordings or image collections for one query.

### Classroom and safety examples

- “Show five people holding phones” must return five distinct people with evidence from a validated relation model, or clearly report that holding is unsupported by installed modules. Do not substitute five phone detections for five people.
- “20% of a classroom of 60” means 12 distinct people satisfying the relation in the same observed frame/window, divided by the 60 visible eligible people in that observation. Occupancy and relation coverage must be shown. It does not mean phone detections summed over time divided by person detections summed over time.
- “Four phones visible” is an object count. “Four people holding phones” is a relation count. Keep separate fields, query options, and labels.
- Crowd rules count simultaneous people within a camera/zone and a duration; a density rule additionally requires a calibrated area or must say it is an image-space approximation.
- A weapon alert requires an installed model that declares the relevant labels. Default COCO object detection does not supply a generic gun/weapon capability. A visible knife alone does not establish threat or intent.
- Multiple queries should reuse one inference pass and its structured output. Do not run a detector again per active rule or search.
- Confidence is the detector's score, not a measured probability of correctness. “Super strong accuracy and speed” must become measured precision/recall and latency targets on representative labeled footage, with hardware and settings recorded.

## 3. Repository handoff: what exists now

Start from this workspace, including its uncommitted changes. Do not reset it.

### Existing foundation

- `rust/src/cw.rs` and `rust/src/watch/mod.rs`: separate `cw` binary, head/worker/fetch commands, local worker by default.
- `head.rs`: HTTP service, camera work assignments, state, preview/media delivery, source configuration, query integration and log/index integration.
- `worker.rs`, `decode.rs`, `gate.rs`: FFmpeg decoding, frame gates, inference loop and telemetry.
- `yolo.rs`: ONNX Runtime object detector adapter; YOLOv5/v8 output handling.
- `split.rs`: deterministic camera assignment weighted by CPU core count, with local-only mode.
- `track.rs`: camera-local IoU tracking and spatial phone/person proximity heuristic.
- `query.rs`: words, template, sandboxed Lua search and a restricted SQL parser.
- `rules.rs`: `seen`, `count`, ratio conditions and sandboxed `lua:` boolean expressions.
- `ui/`: live, search, clip review, cluster, setup, sources and activity pages.
- `vision/modules.json`, `rules.json`, `watch.example.json`: configuration examples.

### Work already added in this session

- Removed the silent 40-detections-per-class cap and repaired holes in image upscaling.
- Added single-class custom YOLOv8 output support.
- Added explicit `archive`, `image` and `folder` source modes; finite scans, archive backpressure, deterministic image IDs and detector/decode error handling.
- Added per-frame metadata with confidence, model path, frame sequence, media offset, track ID and explicitly approximate proximity.
- Added acknowledged `/api/index` batches and thumbnails from the exact inference frame, rather than indexing once-per-second display statistics.
- Added durable JSONL metadata, basic persisted alert/audit history, source and capability endpoints and SQL endpoint integration.
- Added Sources and Activity pages, SQL search mode, module registration and clearer feedback/connection states.
- Added `CW_ROOT` override for isolated runtime/config data roots.
- Added route/embedded-asset mappings for Sources and Activity.

### Verification actually completed

`cargo test --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw --offline` passed **49 tests** after the latest handoff edits. UI JavaScript syntax checks and the Oxfords mechanical checker passed. This is not evidence that real-media indexing, persistence recovery, playback, the browser UI or multi-worker execution has passed acceptance testing.

### Important current limitations

- Storage is append-only JSONL with journal scans; it is not a database-backed scalable event index yet. In-memory recent metadata is capped at 100,000; durable query code reads the journal.
- `COUNT(*)` counts indexed observations. It does not count unique people or simultaneous occupancy.
- SQL currently supports `SELECT *`, `id` or `COUNT(*) FROM sightings`, typed filters, `IN`, boolean conditions, camera grouping, and `LIMIT`. No `HAVING`, `DISTINCT`, ordering, temporal joins or holding predicate yet.
- Phone association means spatial proximity, not holding. Tracks are simple per-worker-session IoU associations, not stable cross-camera identities.
- Folders currently enumerate immediate children only. Remote workers need the same accessible media/model paths; there is no automatic binary/media transfer.
- Scheduling is camera-level core-weighted assignment. It is not adaptive GPU-aware scheduling or a transactional lease system.
- Multiple detector modules can produce duplicate boxes and therefore inflate counts until fusion is implemented.
- Archive sampling/gates can omit brief events. “Completed” does not mean every frame was inferred; record coverage explicitly.
- Lua runtime alert errors currently fail closed but lack a separate diagnostic event.
- There is no login/RBAC. Keep the current service on a trusted local network until the security agent's gate passes.
- Query audit coverage, event deduplication across restart, corrupt/torn journal recovery, durable archive checkpoints and ingestion transactional behavior still need integration work.
- UI browser testing is pending. SQL mode date controls need to apply to SQL or be hidden; saved-search failures need feedback; Sources font loading needs consistency; module deletion is not implemented.
- Existing clip review has a basic ruler and tracks. It is not yet the multi-camera query-result strip editor described below.

Unrelated local changes predate this feature: `examples/dask_sha256.py`, `examples/simple_sha256.py`, `python/cluster.py`, and generated CSV/PNG benchmark results. Preserve them and exclude them from video-feature commits.

## 4. Architecture

```text
 Camera adapters / file manifests / image collections
                 |
          Source registry + media catalog
                 |
   Local scheduler OR cluster scheduler (same task contract)
                 |
     Worker: decode -> sample -> detect -> track -> relate
                 |
      FrameObservation batches + media references
                 |
    Transactional event index + thumbnail/proxy storage
            /                   \
     query planner          persistent rule evaluator
            |                   |
   match windows + clips     alerts + review decisions
            \                   /
       Rust HTTP API + live event stream + audit
                 |
     multi-page web app + filmstrip review workspace
```

Use one executable/configuration model in local and cluster modes. A local executor invokes the same task runner and batch contract as a remote worker. The head may also be a worker. Avoid maintaining a separate “demo pipeline” whose semantics differ from the real pipeline.

For the first release, use an embedded transactional database on the head (proposed SQLite WAL; choose/pin a supported Rust binding after a dependency check). Store original video, proxies and thumbnails as files addressed by media IDs. Do not put full video blobs in the event database. Hide storage behind a trait so a later PostgreSQL backend does not change the query/review contracts. Do not introduce Kafka, a vector database or a new database engine for the first working release.

## 5. Shared domain contracts (freeze before parallel implementation)

Create `rust/src/watch/domain.rs` as the sole source of shared serializable types. Rust internal structs and HTTP JSON use the same versioned semantics. UUID/ULID/string identifiers are opaque; timestamps are explicit, not inferred from filenames.

| Entity | Required fields and invariants |
| --- | --- |
| Source | `id`, `name`, `kind`, location/credential reference, worker eligibility, enabled, optional capture timezone; kind is live/webcam/archive/image/collection |
| MediaAsset | `id`, `source_id`, URI, content fingerprint, duration, dimensions, codec, timebase, optional real capture-start UTC, proxy status, availability/retention |
| ModelVersion | `id`, version, artifact hash, adapter kind, output schema, ordered class mapping, supported relations, threshold policy, provider/device, validation profile |
| IndexJob | `id`, manifest/version, media IDs, model versions, sampling settings, state, checkpoint, coverage summary, error, created/finished times |
| TaskLease | `task_id`, job/media/segment IDs, worker, generation, expiry, attempt, segment overlap, execution settings |
| FrameObservation | stable `observation_id`, source/media/job IDs, frame index or PTS/timebase, `media_time_us`, optional `capture_time_utc_us`, `indexed_at_utc_us`, model versions, coverage/quality flags |
| Detection | stable `detection_id`, observation ID, class, confidence, normalized box, model version, track ID scoped to source/media/run, provenance |
| Relation | observation ID, subject track/detection, predicate, object track/detection, confidence, module version, evidence method; `near` and `holding` are distinct |
| FrameSummary | observation ID, distinct per-class/per-relation counts, confidence filters, zone, coverage state; never confuse unknown with zero |
| Alert | rule/version, source/media/time interval, matched predicate values, severity, evidence, machine state, human decision, actor, timestamp |
| QueryRun | id, normalized query/scope, snapshot/version, coverage, warnings, result count, elapsed time, actor |
| ReviewMatch | stable match ID, query run, source/media IDs, start/end, representative thumbnail, detection/relation evidence, scores, coverage, verification state |
| ReviewSelection | id, ordered references to matches, selected in/out ranges, notes, actor, export status; does not modify original media |
| AuditRecord | id, UTC time, actor/worker, action, target IDs, result, correlation ID, sanitized detail |

**Time:** use integer microseconds in new canonical contracts. UTC capture time is optional for imported recordings; unknown must remain unknown. Media-relative time is always available for videos. Do not label indexing wall-clock time as original recording time. Live segments map media time to known capture time when available. Keep the existing seconds API through an explicit compatibility adapter during migration.

**IDs and retries:** derive observation keys from media fingerprint + frame/PTS + analysis version, not wall-clock processing time. Use unique database constraints and transactional batch upserts. A retry must not increase counts. A new model version is a new analysis layer, not an overwrite of prior evidence.

## 6. Historical review: video editing-style strips

### Workspace layout

```text
 Search | selected sources | capture/media time | query | Run
 -------------------------------------------------------------
 | source/query results | primary video player | match details |
 | filters and groups   | overlays + controls   | evidence      |
 -------------------------------------------------------------
 | ruler      00:00      00:30      01:00      01:30             |
 | Camera A   [thumbnail filmstrip ---- match 1]   [match 2]     |
 | Camera B        [match 3 ----------------]                  |
 | Camera C              [match 4]                [match 5]     |
 | matched-only sequence [1][3][4][2][5] + selected clips       |
 -------------------------------------------------------------
```

The strips are review clips referencing original media, not new duplicated video files. Use original-time view to see event gaps and a matched-only sequence to review only query hits. Do not visually stitch recordings from different days into a single simultaneous event.

### Required behavior

- Run a query with explicit source/media/time scope. Display result count, indexing coverage, model coverage, pending scans, and query duration.
- Convert frame detections into event windows. Merge adjacent matching observations using configurable gap tolerance; keep exact matching evidence and separate pre/post context. Multiple detections in the same frame should not produce duplicate identical clip strips.
- Group tracks by camera, then recording/day. Different clock domains cannot be synchronized unless a mapping is known. For recordings without capture timestamps, use separate media-relative groups.
- Generate thumbnail contact sheets at appropriate densities, lazy-load visible ranges, and virtualize large result sets. Show missing or unavailable media explicitly.
- Selecting a strip opens its recording and seeks to its in-point. A playhead follows playback. Previous/next match work with keyboard and buttons.
- Play/pause, seek/scrub, zoom in/out, fit selection, mute, playback rate, and frame-step while paused. Any claimed frame accuracy must respect decoded frame PTS and actual video timebase.
- Toggle detection boxes, labels, relation evidence, confidence, tracks, and zones. Overlay only detections matching the displayed frame/time tolerance; never reuse stale boxes on another frame.
- Pre/post context defaults are configurable. Trim handles adjust the review selection only; original evidence remains immutable. Keep matched interval and context interval visibly distinct.
- Save selected matches as a review collection, reorder clips for review, attach notes, mark reviewed/confirmed/rejected, and retain an audit trail.
- Export an evidence manifest first (media IDs, timestamps, query, model versions, decisions). Add rendered clip/sequence export with FFmpeg as a separately queued task and show progress/cancel/errors. Cuts requiring re-encoding must not claim stream-copy frame accuracy.
- Live cameras need retained recording segments for historical replay. If only a live preview exists, explicitly say historical footage was not recorded.
- Image matches use a contact sheet with previous/next and evidence details, not fake-duration video strips.
- Mobile uses player-first layout, horizontally scrollable timeline and collapsible details. Full desktop workflow remains keyboard accessible.

### Review API (target contract, not yet all implemented)

- `POST /api/v1/queries` -> query run with typed result kind, snapshot, coverage, warnings and cursor.
- `GET /api/v1/queries/{id}/matches?cursor=...` -> paginated ReviewMatch records.
- `GET /api/v1/queries/{id}/timeline?start_us=...&end_us=...&resolution=...` -> camera/media lanes and visible match windows.
- `GET /api/v1/media/{id}/stream` -> correct Range/Content-Range, seekable original or proxy.
- `GET /api/v1/media/{id}/thumbnails?start_us=...&end_us=...&step_us=...` -> contact-sheet URI + sprite coordinates + actual sample PTS.
- `GET /api/v1/media/{id}/observations?start_us=...&end_us=...` -> overlay evidence for the selected interval.
- `POST /api/v1/reviews`, `PATCH /api/v1/reviews/{id}` -> versioned selections/notes/decisions.
- `POST /api/v1/exports` -> queued export; status endpoint and event updates.

## 7. Ingestion and cluster execution

1. Register and validate a source without beginning an expensive re-index silently. Show discovered metadata and chosen analysis profile.
2. Build a deterministic media manifest on the head. Folder enumeration happens once in a defined order; workers must not independently enumerate different local folders under the same source ID.
3. Validate source access/model availability on eligible workers. RTSP credentials stay in secret references, not query strings, browser logs or audit text.
4. Local mode dispatches to the local executor. Cluster mode assigns leased tasks to compatible workers based on decoder/model/device capability and measured throughput/queue age, not just reported core count.
5. Keep live cameras affined for tracking. Failover resets track identity explicitly. A webcam is pinned to its attached worker. Video archives may split into segments with overlap; merge/reconcile overlaps without double counts.
6. FFmpeg handles decoding/proxies. Always reap subprocesses on EOF, cancellation and errors. Distinguish unsupported codec, missing file, no frames, intentional cancellation and ordinary EOF.
7. Live queues are bounded and may drop older frames to maintain latency; expose sampled/dropped frame counts and queue age. Archives apply backpressure and checkpoint only after durable index acknowledgment.
8. Write observations in idempotent transactions. Report decoded/inferred/skipped/failed frames and analysis coverage, not a single ambiguous percent complete.
9. Heartbeats, expiring leases, cancellation tokens and retry budgets handle failure. Avoid endless retries for permanent validation errors.
10. Preserve replayable source references, model provenance and source capture-time mappings. Live retention records segments; deleting media leaves an explicit unavailable-evidence state.

The first cluster demonstration needs at least two real worker processes and a worker-loss test. Do not report speedup from simulated worker counters.

## 8. Model and accuracy layer

Define a detector trait and output schema. Initial adapter is the current ONNX Runtime YOLO adapter. A module declares input shape, RGB/BGR, normalization, letterbox transform, output layout, ordered labels, confidence/NMS policy, supported execution providers and artifact hash. Check compatibility on registration/startup. A custom model cannot reinterpret COCO output indices by supplying an arbitrary short label list.

Add separate interfaces for object detection, tracking, relation/action detection, and optional verification. An optional vision-language model can verify candidate events asynchronously; it is not required for every decoded frame and cannot silently upgrade an unverified event to a confirmed fact.

Required next work:

- Class-aware fusion across overlapping modules; one physical object should not become two people because two models ran. Preserve both provenance records and define authoritative counting policy.
- Improve tracking with an actual association/tracker adapter, occlusion handling and scoped identity. Cross-camera re-identification is optional and separately validated; never assume the same track ID means the same person across workers.
- Holding-phone relation model/pose adapter with explicit `supported`, `unsupported`, `unknown` and score states. Proximity is a separate approximate predicate.
- GPU provider discovery/binding and CPU fallback reporting. Record effective provider, precision, batch size and input size.
- Accuracy profiles that keep necessary image detail; preview resolution and inference resolution are independent. Validate phone recall in dense scenes. An “Accuracy” preset is not proof of accuracy.
- Labeled evaluation set, frozen train/validation/test split, model hashes, per-class precision/recall, relation metrics, count error, event false alerts per camera-hour, detection-to-alert latency and missed short events.
- Benchmark repeatable hardware/settings and publish speed/quality tradeoffs. User chooses targets after representative footage and hardware are available; do not invent a promised FPS or accuracy percentage.

## 9. Query engine

Keep useful SQL syntax but build a typed semantic layer; never accept arbitrary SQL execution against application tables.

Expose distinct logical tables/views:

- `detections`: one object observation per analyzed frame.
- `tracks`: track summaries scoped to media/source/analysis run.
- `relations`: explicit subject-predicate-object observations.
- `frames`: frame summaries and coverage, suitable for simultaneous counts/ratios.
- `events`: temporal windows, persistent conditions, alerts and annotations.
- `sightings`: compatibility view for existing queries.

The planner validates referenced fields/capabilities, resolves source/time selection, chooses stored analysis vs a new scan, pushes safe filters into indexes, and returns typed rows/aggregates/match windows. Use parameterized database access. Add bounded query execution, pagination, cancellation, stable ordering, explain output and diagnostic errors.

Examples below are **target syntax**, not promises that the current subset executes them:

```sql
-- Search object observations, then materialize event windows for review.
SELECT * FROM detections
WHERE source_id IN ('classroom-a', 'classroom-b')
  AND class = 'cell phone' AND confidence >= 0.8;

-- Explicit relation evidence; requires a holding-capable module.
SELECT DISTINCT subject_track_id FROM relations
WHERE predicate = 'holding' AND object_class = 'cell phone'
LIMIT 5;

-- Simultaneous per-frame counts, not accumulated observations.
SELECT * FROM frames
WHERE people_count > 0
  AND phone_holders_count >= 4
  AND phone_holders_count * 1.0 / people_count >= 0.2;
```

Define `DURING`, `NOW`, temporal windows and `CREATE ALERT` only after semantics exist. For v1, creating a saved query or rule through an API is enough; unsupported syntax must return a precise error rather than approximating it.

A historical query that references an unindexed class must say which model and scan are required. It may offer/queue a backfill job. Existing zero results cannot be presented as evidence of absence when coverage is unknown or the installed models do not support that target.

## 10. Persistent Lua rules and alerts

Use the same FrameSummary/relation semantics for live rules and historical backtests. Keep the current DSL as a convenient builder that compiles to the typed rule model.

Proposed Lua context:

```lua
return function(ctx)
  return ctx:count_people() > 0
     and ctx:count_relation("holding", "cell phone") >= 4
     and ctx:ratio_people_with("holding", "cell phone") >= 0.20
end
```

This is a future typed API. Current executable examples are `count("cell phone") >= 4`, `ratio("cell phone", "person") >= 0.2`, and `lua: count("cell phone") >= 4 and count("person") > 0`; these count visible objects, not phone holders.

Rules specify source/zone scope, confidence/coverage policy, duration in milliseconds, cooldown, escalation and severity. Do not base duration on inference frame count: FPS, filtering and worker speed vary. States include pending, active, resolved and human review decisions. Missing/blurred/unsupported observations become unknown, not zero; record how unknown affects a duration window.

Sandbox scripts with no file/network/process access, bounded memory/instructions, immutable context, no unbounded wall-clock work and explicit errors in logs. Cache compiled scripts instead of reparsing/allocating Lua on every frame where safe. Version changes reset state or migrate it explicitly. Deduplicate alerts across retries and worker failover. Human confirm/reject remains separate from model state. Backtesting should show alert intervals, evidence and false-positive feedback in the review timeline.

## 11. Persistence, API, security and operations

- Add migrations for sources, media, model versions, jobs/leases, observations, detections, relations, summaries, events, query runs, reviews and audit.
- Index source/media/time/class/model/track/relation lookup fields. Use bounding/time-range indexes where useful. Batch writes, WAL and retention/compaction must be measured.
- Import legacy JSONL with a restartable migration and preserve IDs/unknown timestamps. Back up before migration; do not delete legacy files automatically.
- Expose versioned APIs while preserving current endpoints through adapters. Return structured errors with code, message, retryable flag and correlation ID.
- Add SSE or WebSocket updates for job progress/live results/alerts with resume cursors, plus pagination for historical reads. Do not repeatedly send the entire index through `/api/state`.
- Implement local administrator bootstrap, login/session expiration and roles: administrator, operator, reviewer, read-only. Authenticate workers separately. Protect media, exports, source credentials, mutations and audit access.
- Default to loopback or an explicit trusted bind until authentication is configured; support TLS through a documented proxy for remote access. Do not add a fake login UI over unprotected endpoints.
- Validate IDs/paths/protocols; constrain server-side access by approved media roots and source permissions. Redact credentials. Require explicit rules for retention/deletion and keep the audit immutable to ordinary users.
- Persist warnings/errors about missing models, indexing gaps, lost workers, disk pressure and alert script failures. Expose health/readiness that distinguishes serving HTTP from actually processing sources.
- Record real decoded/inferred FPS, end-to-end latency, queue age, indexed coverage and disk usage. Estimated FLOP figures remain marked estimates.

## 12. Parallel execution strategy

See `VISION_AGENT_TASKS.md` for copy-ready prompts. Use one coordinator and independent ownership. In a shared working directory, agents must not edit another owner's file. With separate worktrees, establish a baseline commit and use small commits; the coordinator integrates them. Do not create worktrees from old `main` without including this session's uncommitted vision work.

### Wave 0: baseline and contracts (coordinator)

- Capture the present changes, rerun tests, inspect the diff and preserve unrelated benchmark files.
- Finish current integration checks before treating the foundation as accepted.
- Freeze domain types, API examples, schema versions, timing/identity semantics and error envelopes.
- Own changes to `Cargo.toml`, `Cargo.lock`, `mod.rs`, `head.rs`, shared `domain.rs`, global UI shell/routes and all merges. Other agents request these changes through an integration note.

### Wave 1: parallel foundations

| Agent | Primary responsibility | Owned files / new modules |
| --- | --- | --- |
| A | Storage and media catalog | new `storage/`, `media_catalog.rs`, migrations, storage tests |
| B | Ingestion and cluster | `worker.rs`, `decode.rs`, `split.rs`, `net.rs`, new `ingest/`, `scheduler/`, ingestion tests |
| C | Inference and accuracy | `yolo.rs`, `track.rs`, `fight.rs`, `gate.rs`, new `models/`, evaluation fixtures/reports |
| D | Query and event windows | `query.rs`, new `query/`, `matches.rs`, query tests |
| E | Rules and alerts | `rules.rs`, new `alerts/`, alert tests |
| F | Review timeline UI | `ui/review.*`, `ui/timeline.*`, review browser tests; own `clip.*` replacement |
| G | Product management UI | `ui/sources.*`, `setup.*`, `logs.*`, `find.*`, `wall.*`, scoped product styles/tests |
| H | Security, acceptance and packaging | new `auth/`, integration tests, deployment docs; coordinator wires middleware |

Each agent may use contract fixtures before dependent implementation lands. Fixtures must be marked test data and must never silently appear as real detections in the product. Keep UI feature styles scoped; the coordinator owns `app.css`, `app.js`, and nav to prevent collisions.

### Wave 2: vertical integration

Integrate a local path first: import one recording -> metadata/proxy -> actual inference -> durable observations -> query -> matching strips -> playback -> rule backtest -> reviewed evidence. Then enable the same path across workers. The timeline agent can build against frozen match/media contracts while storage, models and queries run in parallel.

### Wave 3: verification and finish

Run browser acceptance, multi-worker failure/recovery, accuracy evaluation and load tests. Fix failures, document measured limits, produce reproducible setup and a demo walkthrough. Publish/merge only a tested integrated result; do not label independent agents finishing as end-to-end completion.

## 13. Acceptance tests and release gates

### Current integration gate (before new infrastructure)

- Start head using an isolated `CW_ROOT` and temporary config; no writes to the user's real data.
- Add archive/image/folder sources through API. Reject malformed config and preserve explicit source kind.
- Run actual FFmpeg + existing ONNX detector on supplied sample media; inspect records and exact-frame thumbnails.
- Query the durable index, restart head, query again, and replay original media at saved offsets.
- Retry an identical index batch and prove counts do not increase; test partial/malformed payload and concurrent batches.
- Missing model/invalid video must show failure/unsupported, not successful empty indexing.
- Verify `/sources`, `/logs`, SQL search, rules and media endpoints in the real browser.

### Product acceptance scenarios

1. **One PC:** default head + local worker imports real media, finishes a finite job, shows coverage and usable query strips.
2. **Cluster:** two real worker processes share several sources/segments; assignments respect affinity/capability; one worker stops; unfinished work resumes without duplicate observations or fake continuity of track IDs.
3. **Classroom fixture:** a labeled simultaneous frame/window has 60 people and 12 phone holders. Stubbed relation fixtures verify exact query/count/ratio semantics; real labeled footage separately evaluates the actual relation model. Do not pass the model gate from fixture-only tests.
4. **Distinctness:** one person seen in 100 frames is 100 observations but one scoped track; five matches in one frame do not become five unique people if tracks are the same.
5. **Unknown coverage:** missing model, occluded object, skipped frame, corrupt source and missing capture time remain visible as unknown/unsupported/gaps.
6. **Alerts:** duration/cooldown use time, survive retries appropriately, handle division by zero/unknown safely, and report sandbox failures.
7. **Timeline:** query across three recordings produces camera lanes; clicking/scrubbing seeks to correct PTS; unknown capture clocks are not synchronized; trim/context stays within media boundaries; overlays match the displayed frame; thousands of matches remain usable.
8. **Review/export:** save selections, reload, retain notes/decisions/provenance, export manifest and correctly cut a clip with explicit encoding/time precision.
9. **Persistence:** crash/restart mid-index and mid-export, schema migration, disk-full, corrupt legacy line and unavailable original media are recoverable and visible.
10. **Access:** unauthenticated requests cannot inspect protected video, audit, credentials or mutate config; roles are enforced server-side; worker credentials cannot act as admin sessions.
11. **UI:** desktop/tablet/mobile screenshots, real interaction tests, keyboard/contrast/reduced motion, source errors, empty/no-results/loading and retry states. No fabricated counters or footage.
12. **Performance:** record target hardware, models, providers, resolution, sampling, camera count, p50/p95 detection-to-index and query/review latency, FPS and coverage. Agree budgets from these measurements.

## 14. Definition of done

A new checkout has documented build/run commands and model/media setup; the one-PC demonstration works; the cluster uses the same contracts; historical query results open as editor-style strips; saved evidence survives restart; rules produce reviewable alerts; unsupported claims are explicit; acceptance tests pass; model performance and operational limits are documented. No claim of universal camera compatibility, verified holding detection, full-frame coverage or production security is made without its corresponding test evidence.
