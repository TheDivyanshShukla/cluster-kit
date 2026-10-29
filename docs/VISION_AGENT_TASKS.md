# Parallel agent tasks for Watch

Read [VISION_IMPLEMENTATION_PLAN.md](VISION_IMPLEMENTATION_PLAN.md) first. It contains the current handoff state, the architecture, target contracts, and the required video-editor-style review workspace. Do not rebuild the current engine from scratch.

## Coordinator: run this first

Copy this into the lead agent:

> Continue the Watch video-query platform in this repository using docs/VISION_IMPLEMENTATION_PLAN.md as the product spec. First preserve and inspect all current uncommitted vision changes; do not reset or overwrite them. Exclude unrelated Python/SHA benchmark changes and result artifacts. Run the current 49-test Rust baseline and finish real-media/API integration checks. Freeze rust/src/watch/domain.rs, versioned JSON examples, timestamp/identity semantics, and an API error envelope before delegating implementation. You exclusively own head.rs, mod.rs, config.rs integration, Cargo dependencies, shared UI app.js/app.css/nav and merge decisions. Use separate file owners from this task sheet. Agents may add private modules and report required integration changes; they must not independently rewrite shared files. Keep the existing Rust backend and browser JS foundation. Integrate a complete local workflow first, then cluster failure/recovery. Deliver real evidence of tests, not only agent completion summaries. The historical review screen must be a video-editor-style player and multi-camera timeline with query-matched filmstrips, context, playhead, zoom, selection and evidence details. Distinguish observations, simultaneous counts, unique tracks, proximity and verified holding. Keep production claims conditional on real evaluation.

Coordinator outputs before Wave 1:

- A baseline containing the current vision work, with unrelated changes preserved separately.
- `domain.rs` and small JSON fixtures for every shared contract.
- Stable `FrameObservation`, `Detection`, `Relation`, `FrameSummary`, `ReviewMatch`, `TaskLease` and structured error definitions.
- A module/route integration checklist and explicit file ownership.
- Storage trait, media/thumbnail interface, rule-context interface and query-result envelope.
- A decision about which new dependencies are necessary and their compatible pinned versions.

Only the coordinator changes dependencies/shared contracts. Domain changes requested by one agent must be announced to all affected agents before implementation diverges.

## A. Storage and media catalog

> Own new rust/src/watch/storage/**, media_catalog.rs, migrations and storage tests. Read the frozen domain contracts from the coordinator. Implement a transactional embedded metadata store, initially SQLite WAL if the coordinator approves the binding. Store media as immutable references, not video blobs in the DB. Include migrations, source/media/model/job/observation/detection/relation/summary/query/review/alert/audit persistence, typed indexes and idempotent batch upsert. Archive checkpoints advance only after commit. Preserve provenance and distinguish capture UTC, media PTS and indexed time. Add a restartable legacy JSONL migration without deleting old data. Provide traits for query filtering, match windows, thumbnails/proxies and retention states. Tests must prove retry deduplication, crash/restart recovery, count correctness, legacy import, and unavailable media. Do not edit head.rs, config.rs, dependencies or UI; provide coordinator integration notes.

Deliver: migrations, repository trait implementation, compatibility import, storage contract tests, measured query/write behavior on a generated test index.

## B. Ingestion and cluster execution

> Own worker.rs, decode.rs, split.rs, net.rs and new ingest/** and scheduler/** modules. Build on the existing finite archive, folder, backpressure and /api/index changes. Use frozen contracts and the storage agent's interfaces. Implement deterministic import manifests, media probing, decode/proxy tasks, coverage tracking, cancellable bounded live queues, archive checkpoints, worker capability registration, authenticated task identity hooks, expiring leases/retries and camera affinity. Pin webcam sources to their actual device worker. Ensure one-PC mode runs the same tasks as cluster mode. Support video segment overlap without double-indexing. Avoid endless retries on permanent errors; reap FFmpeg children. Make live recording retention explicit. Tests must cover actual FFmpeg files/images, cancellation, bad media, model-unavailable states, two-worker assignments, worker loss/recovery and duplicate batch prevention. Do not edit model/query/rule/UI/shared coordinator files.

Deliver: real local import path, real two-worker run and failover evidence, coverage/throughput records, clear media accessibility behavior across workers.

## C. Inference adapters and measured accuracy

> Own yolo.rs, track.rs, fight.rs, gate.rs, new models/** and model evaluation fixtures/docs. Retain the fixes for cap40, resizing and single-class model outputs. Define modular object/tracking/relation adapters against the frozen domain types. Validate ordered labels, input/output layout, artifact/version hashes, effective execution provider, preprocessing and per-class thresholds. Prevent duplicate counts from overlapping detector modules using an explicit fusion/provenance policy. Improve scoped tracking. Add a holding-phone relation adapter only when a compatible model can be actually evaluated; keep near as an explicitly approximate relation and return unsupported for holding otherwise. A weapon model must declare its supported classes. Decouple preview and inference resolution. Build a reproducible labeled evaluation and speed/quality benchmark, recording precision/recall/count error/false alerts and p50/p95 latency on actual hardware. Do not invent accuracy or FPS guarantees. Coordinate dependency/provider additions with the coordinator; do not edit scheduling, storage, HTTP or UI.

Deliver: adapter registry, validated example module manifests, duplicate-count tests, labeled evaluation procedure/results or explicit missing evaluation inputs, actual ONNX inference tests.

## D. SQL, temporal matches and review windows

> Own query.rs, new query/** and matches.rs, plus query tests. Build on current safe SQL subset and frozen storage/domain interfaces. Distinguish detections, tracks, relations, frame summaries and temporal events. Add safe typed semantic validation, supported aggregation/distinctness, stable pagination/order, camera/media/time scopes, bounded execution/cancel and explainable errors. Do not execute arbitrary SQL against application tables. Use database pushdown through storage interfaces. Implement materialization of frame matches into ReviewMatch windows grouped by source/media/clock domain, merging configurable gaps while preserving exact matched intervals and separate pre/post context. Return coverage and unsupported-model warnings. Five returned phone observations must not imply five distinct phone holders. Tests must cover simultaneous 60/12 frame semantics, same-track repeated observations, gaps, confidence/model versions, mixed clocks, clip boundary clamping, retries, parser budgets and stable cursor pages. Coordinate SQL surface with review and product UI agents. Do not edit head.rs, rules.rs, storage migrations or shared UI.

Deliver: versioned query result/matches/timeline payloads, capability-aware queries, window coalescing and pagination tests.

## E. Lua rules, temporal alerts and backtesting

> Own rules.rs and new alerts/** with tests. Keep existing count/seen/ratio DSL and lua: rules compatible. Implement a typed rule context using FrameSummary and relation evidence, source/zone scope, confidence/coverage policy, elapsed-time hold windows, cooldown, lifecycle state and human decisions. Never count frames as seconds; unknown observations are not zero. Reuse inference outputs for multiple rules. Add bounded sandbox execution, cached compilation, errors as structured diagnostics, versioned rule state, failover/retry dedup and historical backtests. Rules for phone-holder percentage must use distinct same-observation people and validated relation capability. Persist through storage interfaces; do not own migrations. Tests must include 12/60=20%, zero denominator, unavailable model, dropped frames, varying FPS, long-running Lua, restart, cooldown and review-state separation. Do not edit query.rs, worker.rs, head.rs or UI.

Deliver: executable Lua examples, rule-state contract, backtest event windows consumable by the timeline, sandbox and temporal tests.

## F. Video-editor-style review UI

> Own new ui/review.html, review.js, review.css, timeline.js/timeline.css and the eventual clip.* replacement; coordinate route wiring with the lead. Follow docs/VISION_IMPLEMENTATION_PLAN.md section 6 exactly. Keep browser-native JS initially. Implement a player above a virtualized multi-camera timeline of query-matched thumbnail strips, ruler/playhead, original-time and matched-only views, previous/next match, playback speed, seeking/scrubbing, zoom/fit, separate context handles, detection/relation overlays synchronized to frame time, details, notes, verification and saved review selections. Show image results as a contact sheet. Do not fake simultaneous clocks across unrelated recordings or use stale overlays. Use frozen query/media/match/review APIs and clearly identified fixtures only while dependencies are pending. Handle missing video, indexing gaps, no results, errors, keyboard/touch and reduced motion. Support an evidence-manifest export first, queued rendered exports later. Match existing app identity and use scoped styles; lead owns app.css/app.js/nav. Test real playback/seeking and thousands of strips in a real browser, not only static screenshots.

Deliver: complete interaction walkthrough, desktop/mobile screenshots, browser tests, API integration notes. The visible result must look and work like a video review editor, not a dashboard or a list of thumbnail cards.

## G. Sources, search, rules/models, live and activity UI

> Own ui/sources.*, find.*, setup.*, logs.*, wall.* and a new scoped product.css if needed. Preserve the existing new pages and six-workspace navigation. Use frozen versioned contracts. Build source creation/import progress/coverage, source groups, query builder+SQL, saved searches, model capabilities/registration/disable/removal, Lua rule editing+backtest, live source health/alerts, and searchable activity with server pagination. Route query results into the review editor using query-run IDs, not just latest hits. Fix SQL time controls so scope is actually applied, saved-search error feedback and font consistency. Show unknown/unsupported states and no-result coverage clearly. Surface required auth/role state supplied by security; never ship a fake login. Lead owns shared app CSS/JS/nav. All visible actions must work and have loading/error/empty feedback, keyboard access and mobile layouts. No fabricated live feeds/counters.

Deliver: browser-tested source->query->review and rule->alert->review user journeys, component states, screenshots and no broken controls.

## H. Security, integration acceptance and packaging

> Own new auth/**, integration tests and operational/deployment docs. Define local admin bootstrap, sessions/roles and separate worker credentials using coordinator-approved dependencies. Provide Rust middleware/hooks for the lead to wire into every protected API/media/export path. Do not edit head.rs directly. Add source credential references/redaction, allowed media roots, protected mutation and audited review actions. Create an isolated reproducible integration harness using CW_ROOT so tests do not modify user footage/config. Exercise real FFmpeg+ONNX local indexing, durable restart, query->match->media playback contracts, range requests, multi-worker failure/retry, script/parser limits, access roles and export failure/cancel. Treat current lack of auth and JSONL crash/idempotency gaps as explicit pre-release issues. Document setup, model licensing/artifact acquisition, local/cluster run commands, retention, recovery and measured limits. Do not claim production readiness until these gates pass.

Deliver: automated integration command, security tests, reproducible demo instructions, release checklist with evidence links and unresolved blockers.

## Working rules for every agent

- Read relevant existing code and the current uncommitted diff before editing.
- Only modify owned files. Send the coordinator a short integration note for shared files/dependencies.
- Use actual types/fixtures from the frozen contract. Do not silently invent a different API.
- Do not revert another agent's work, migrate the UI framework, rename the product, or add infrastructure without a concrete need.
- Preserve original footage. Tests use isolated data roots and clearly marked fixtures.
- A claim needs evidence: code path, meaningful test, real-media run, browser interaction or measured evaluation as appropriate.
- Report completed files, exact tests, remaining limitations and required integration steps. No automatic production deployment.

## Recommended order

1. Coordinator establishes baseline/contracts.
2. Run A, B, C, D in parallel. E, F, G, H can begin against frozen fixtures if more slots are available.
3. If slots are limited, start F early alongside A/B/C so the main review experience does not wait until the backend is finished.
4. Coordinator integrates storage + ingestion + inference + query into one local vertical path.
5. Integrate F/G and E; run H's end-to-end and access tests.
6. Validate true two-worker execution and failure/recovery, then polish and package.

Do not assign the same shared file to two agents. For separate worktrees, commit the baseline first; otherwise new worktrees will miss the work already present in this session.
