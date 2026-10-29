# Watch release checklist

Gates come from `docs/VISION_IMPLEMENTATION_PLAN.md` section 13. Status values: **pass**, **fail**, **not run**.
A gate is "pass" only with a linked artefact. Nothing here is a production-readiness claim.
Last updated by agent H on 2026-10-29, before coordinator integration of Wave 1.

Evidence index: `docs/evidence/h/acceptance_run1.txt` (integration harness, legacy head as built at run time),
`rust/src/watch/auth/mod.rs` tests (security unit tests, run with the command in the agent H report).

## Status update after integration (2026-10-29)

This section supersedes the per-gate statuses further down where they differ. Evidence lives under `docs/evidence/`.

| Item | Status | Evidence |
| --- | --- | --- |
| Auth wired into the head | pass: enforced once an admin exists; signed-out requests to the API get 401, `/login` signs in and returns to the page, a session cannot post worker batches (403), password change works | `auth::tests::*` (incl. `change_password_needs_current_and_takes_effect`), `coordinator/signed-in-sources.jpg` |
| v1 API in the head | pass | `h/acceptance_run4.txt` (17 pass, 0 fail, 1 skip = auth in open mode) |
| Two workers, worker loss | pass: tasks move at generation 2, 0 duplicate observations | `coordinator/failover.md` |
| Late-joining worker | pass: one archive moves to the idle worker, resumes from checkpoint, 0 duplicates | `scheduler::tests::late_worker_takes_movable_work`, real run in this session (500 obs / 500 distinct) |
| Export cancel against the real API | pass: status `cancelled`, partial file removed, file route 404 | real run in this session |
| Webcam devices | discovered automatically (avfoundation / /dev/video* / dshow); `CW_DEVICES` overrides | `decode::device_tests` |
| Accuracy | first measurement only: 30 frames, single non-human annotator; person P 1.00 / R 0.77 | `coordinator/accuracy_sample30.md` |

Still open: a human-labelled representative evaluation set, a holding-capable relation model, TLS (use the reverse proxy in WATCH_OPERATIONS.md), user delete/role change, and release-build throughput numbers.

## Pre-release blockers (must be closed before any release)

1. **Authentication is not wired.** `auth` is built and tested but `head.rs` calls none of it. Today every
   endpoint, including source/rule/module/settings mutation, `/api/media`, `/api/live`, `/api/hit`, `/api/logs`
   (audit) and the worker routes, is open to anyone who can reach port 7710, and the head binds `0.0.0.0`.
   (Harness check `lan_exposure` = FAIL.)
2. **No login UI.** A real login/bootstrap page does not exist; do not add a fake one over unprotected
   endpoints.
3. **JSONL idempotency is in memory only.** The duplicate-batch key lives in `hub.index_batches`; after a head
   restart the local worker re-processes an archive and appends duplicate rows (harness `reindex_no_duplicates`).
4. **JSONL crash safety is untested.** Truncated last line, disk-full and concurrent-writer behaviour have no
   tests; `seen.jsonl` is rescanned in full for every query.
5. **Arbitrary media paths.** `media_path` serves any file a source URL points at; `allowed_media` exists but is
   not applied. Source URLs and credentials are stored and echoed (`GET /api/sources`) unredacted.
6. **Missing model is silent.** With the object model file absent, run 1 saw no error, no completion and zero rows after 90 s (harness `missing_model` = FAIL); the panel gives the operator nothing to act on.
7. **Model licence.** YOLOv5 is AGPL-3.0; no legal review recorded. `cw fetch` has no checksum pinning.
8. **No retention policy**: files grow without bound; no immutable audit store.
9. **Accuracy is unmeasured.** No labeled evaluation; weapon/fight/holding claims unsupported.
10. **No recorded performance budget** (hardware, p50/p95, FPS, coverage).

## Gates (plan section 13)

### Current integration gate

| Gate | Status | Evidence |
| --- | --- | --- |
| Isolated `CW_ROOT`, no writes to real data | pass | `acceptance_run1.txt` `repo_data_untouched` |
| Add archive source through API; reject malformed config | pass | `add_source`, `reject_bad_source` |
| Real FFmpeg + ONNX indexing of sample media; inspect records and thumbnails | pass for records (58 frames, 108 detections); thumbnails not inspected | `indexing_completes`, `legacy_query` |
| Query durable index, restart, query again | pass (legacy path); v1 path not run (not wired) | `durable_restart` |
| Replay original media at saved offsets | partial: Range 206 pass; offset replay in a browser not run | `media_range` |
| Identical batch retry does not raise counts; malformed payload | pass within one process; **fail** across restart (blocker 3) | `idempotent_batch`, `malformed_batch` |
| Concurrent batches | not run | no test |
| Missing model / invalid video shows failure, not empty success | **fail** (model); invalid video not run | `missing_model` |
| `/sources`, `/logs`, SQL search, rules, media verified in a real browser | not run (agent F/G own UI evidence) | none |

Statuses above are from `acceptance_run1.txt` (12 pass, 3 fail, 3 skip). Re-run the harness before changing any status.

### Product acceptance scenarios

| # | Scenario | Status | Evidence / note |
| --- | --- | --- | --- |
| 1 | One PC: finite job, coverage, usable query strips | not run | needs v1 + review UI integration |
| 2 | Cluster: two workers, affinity, worker loss, no duplicates, no fake track continuity | not run | scheduler (agent B) not integrated; harness covers one worker only |
| 3 | Classroom fixture: 60 people / 12 phone holders semantics | not run | stub-relation tests are agent C/D; real relation model absent (no holding model) |
| 4 | Distinctness: observations vs tracks | not run | agent C/D tests |
| 5 | Unknown coverage stays unknown | not run | partial: `FrameSummary::class` unit test in `domain.rs` |
| 6 | Alerts: time-based duration/cooldown, sandbox failure reporting | not run | agent E |
| 7 | Timeline: three recordings, seek accuracy, boundaries, thousands of matches | not run | agent F |
| 8 | Review/export: save, reload, manifest, correct clip cut | not run | agent F + exports not built |
| 9 | Persistence: crash mid-index and mid-export, migration, disk-full, corrupt legacy line, missing media | not run | storage agent tests do not cover crash of the live head; blocker 4 |
| 10 | Access: unauthenticated cannot see video/audit/credentials/mutate; roles server-side; worker token is not an admin session | **fail** end to end (not wired); unit level pass | unit: `auth::tests::role_matrix`, `worker_and_session_are_disjoint`, `unknown_mutations_denied_*`; end to end: harness `auth_protected_routes` (SKIP until wired) and `lan_exposure` |
| 11 | UI: screenshots, keyboard, contrast, error/empty states | not run | agents F/G evidence under `docs/evidence/f`, `g` |
| 12 | Performance: p50/p95, FPS, coverage on named hardware | not run | no measurement exists |

### Security unit coverage (agent H, passes at module level)

| Item | Status | Test |
| --- | --- | --- |
| HMAC-SHA256 RFC 4231 cases 1, 2, 6 | pass | `hmac_rfc4231_vectors` |
| PBKDF2-HMAC-SHA256 published vectors (c=1, 2, 4096, dkLen 40) | pass | `pbkdf2_published_vectors` |
| 210,000+ iterations, salt, hash stored, 0600 file, worker tokens hashed only | pass | `production_cost_and_hash_verify`, `auth_file_is_0600_and_tokens_hashed` |
| Bootstrap once, token required | pass | `bootstrap_only_once_and_needs_token` |
| Session cookie flags, sliding idle expiry, absolute lifetime, logout | pass | `session_cookie_flags_expiry_and_logout`, `absolute_session_lifetime` |
| Role matrix incl. reviewer cannot mutate sources, read-only cannot see media or audit | pass | `role_matrix` |
| Worker token vs session disjoint; unknown mutation denied | pass | `worker_and_session_are_disjoint`, `unknown_mutations_denied_*` |
| Login rate limit; CSRF origin check | pass | `login_rate_limit`, `cross_origin_mutation_refused` |
| URL redaction; media roots reject `..` and symlink escape; audit redaction | pass | `redact_cases`, `allowed_media_rejects_traversal_and_symlinks`, `audit_record_is_redacted` |

Known security gaps even after wiring: no TLS in the process (proxy required), sessions are lost on restart,
no password-change or user-delete endpoint, no account lockout beyond the 5-per-5-min window, hand-rolled PBKDF2
has no independent audit beyond the published vectors, and `rate_limited` is not a status in
`ApiError::status()` (handlers return 429 themselves).

## Definition of done (plan section 14)

Not met. Open: wired auth, v1 API in the head, one-PC and cluster demos on shared contracts, strips opened from
history, alerts, stored evidence surviving restart, measured model accuracy and limits.
