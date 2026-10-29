# Agent G journeys (as tested)

Setup: `cw head --port 7791` with `CW_ROOT=<tmp>` (copy of `vision/`, models and footage symlinked, UI symlinked to the repo), real local worker indexing `people-detection.mp4` and `person-bicycle-car-detection.mp4`, driven with Playwright (headless Chromium) at 1280 and 390 px. This head had **no `/api/v1` routes yet** (every v1 call returned a bare 404), so:

- **LEGACY** = ran against the real server, real indexed data.
- **MOCKED v1** = the same page with `page.route('**/api/v1/**')` returning contract-shaped JSON from the test script. This proves the page's v1 code path, not the server. Screenshots carry `-MOCKED` in the file name.

## Source -> query -> review

| Step | Page | Path used |
| --- | --- | --- |
| Add recording "Test clip" (archive, `vision/footage/people-detection.mp4`) | Sources | LEGACY: GET `/api/sources` (expanded list, which carries `kind`), POST `/api/sources`. Worker reported it within seconds on Live. |
| Tick source, save group "G1", edit then cancel | Sources | Browser only (localStorage, labelled as such). |
| Import progress / coverage | Sources | LEGACY: v1 `/api/v1/jobs` 404 -> explicit "not available on this server version" plus per-camera status from `/api/state` (frames indexed, fps). MOCKED v1: job card with decoded/inferred/skipped/failed, analyzed seconds, % of media time, polling 2 s while running then 8 s, a missing media item flagged, credential field cleared and shown only as "stored as reference". |
| Builder: source Hall, class person, confidence 0.5, From 2020-01-01 -> generates `SELECT * FROM sightings WHERE class='person' AND confidence >= 0.5 AND camera_id='hall' AND ts >= 1577817000 LIMIT 100`; SQL box editable ("edited by hand" badge, Regenerate button) | Search | Time control is applied in the generated SQL (`ts`, legacy) or `capture_time_utc_us` plus `scope.from_utc_us/to_utc_us` (v1). In legacy raw-SQL mode the time controls are hidden. |
| Run | Search | LEGACY: POST `/api/query` -> 20 moments; bad SQL shows server message ("expected SELECT"); aggregate shows count with the "not a headcount" note; words mode uses `/api/find`. MOCKED v1: POST `/api/v1/queries` body `{sql, scope}` -> run summary (2 "detection observations (not distinct people)", coverage, warnings incl. "holding unsupported", skipped/failed frames, "Open in review" -> `/review?run=qr_mock1`), "Load more" via `/queries/{id}/matches`, ApiError shown with code and correlation id. Zero results shows "not evidence of absence". |
| Open in review | Search | Link to `/review?run=<id>` (query-run id) and per match `/review?run=<id>&match=<id>`. Not followed in this run: the review page is Agent F's and the legacy server has no query runs. Legacy results link to `/clip?...`. |
| Saved searches: no name -> "Name the search before saving it."; save, load, remove all give feedback | Search | LEGACY `/api/searches` (no v1 route exists). |

## Rule -> alert -> review

| Step | Page | Path used |
| --- | --- | --- |
| Add rule with bad line `count((` | Rules & models | LEGACY POST `/api/rules` -> server message "class name needs quotes" shown. |
| Add rule `count("person") >= 2`, edit hold to 3, remove (confirm dialog) | Rules & models | LEGACY. |
| Register module (added disabled), remove module (confirm; POST `/api/modules` without it) | Rules & models | LEGACY. |
| Model capabilities | Rules & models | LEGACY: "not reported by this server version" + configured modules. MOCKED v1: classes, near = approximate, holding = unsupported, provider, validation "unvalidated", artifact hash. |
| Backtest | Rules & models | LEGACY: button disabled, "not available". MOCKED v1: POST `/api/v1/rules/backtest` `{rule, scope}` -> alert windows with "Open in review" (`/review?source=&media=&start_us=&end_us=`, or `/review?run=` if the response carries `query_run_id`). |
| Alert fires, person decides | Live | LEGACY: two real "agreed" alerts from the running rule, Confirm -> POST `/api/confirm` -> "Decision recorded." MOCKED v1: `/api/v1/alerts` rows, POST `/api/v1/alerts/{id}/decision` `{decision:"confirmed"}`, decision and actor shown. |
| Live health | Live | LEGACY real: fps, status, people, skipped per camera. Source with no frames: image error -> "No live preview. This source is not producing frames right now." (tested by aborting `/api/live/street`). MOCKED v1: enabled source no worker reports gets an explicit no-frames tile. |

## Other checks

- Auth: `/api/v1/auth/me` 404 -> "No login configured — trusted LAN only." (real). MOCKED 401 -> "Sign-in required" with link `/api/v1/auth/login`; MOCKED 200 -> "Signed in as ada · operator".
- Activity: LEGACY `/api/logs` client-side filter with an explicit note; MOCKED v1 `/api/v1/audit?q=&cursor=` paging ("Load more" 4 -> 6), category chips (queries, index jobs, failures, model changes, alert decisions), Index jobs and Alert review tabs.
- Fonts: all five pages load the same IBM Plex link and `app.css`; computed body and input font = `"IBM Plex Sans", "Segoe UI", sans-serif`.
- Mobile 390 px: no horizontal scroll on Sources, Search, Rules & models, Live.
- Console errors seen were only the expected 404 probes of absent `/api/v1` routes and `favicon.ico`.

## Round 2 (v2 shell, CCTV wall, three-pane search) - tested against the real `/api/v1`

Screenshots in `docs/evidence/g/v2/` at 320, 390, 1280, 2560 px (JPEG q70): live, search (+ error marker, Lua), sources, setup, activity, cluster, clip, review. An element-level check found no un-clipped horizontal overflow at 320 or 390 on any of the eight pages.

Real v1 (no mocks) verified:
- Auth `/api/v1/auth/me` -> "Signed in as anonymous · administrator (open-trusted-lan)".
- Search: POST `/api/v1/queries` `{sql, scope}` -> run summary, match table, "Open in review" -> `/review?run=qr_...` (review page loaded). Bad SQL -> `expected SELECT at position 0` with the marker drawn under `SELEC`. Ctrl+Enter runs.
- Lua: POST `{lua, scope}` currently returns `invalid_request: provide sql, or class / min_confidence` (backend not there yet); shown inline with a "Run on classic index" button that runs the same `hit` predicate through `/api/find` (40 moments).
- Sources: POST `/api/v1/sources` with import -> "Import job job_... started", live job card; Disable via upsert. Models: `/api/v1/models` returns `{models:[{model|null, module, status}]}`; Setup/Search/Sources read it. Rule backtest `/api/v1/rules/backtest` returned 0 windows. Audit `/api/v1/audit?q=` returns `{items,next}`; Live rail lists real v1 alerts and Confirm posts a decision.
- Known server-side gap seen: `/api/v1/media/{id}/frame` answers 503 here, so result thumbnails show "n/a".
