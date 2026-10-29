# Review UI (`/review`)

Video-editor style historical review. Files: `rust/src/watch/ui/review.html`, `review.js`, `review.css`, `timeline.js`, `timeline.css`, `fixtures/review.json`. Browser-native, no build step, no CDN.

- `/review?run=<query_run_id>` loads a real query run. Optional `&match=<id>` and `&review=<id>` restore the open match and a saved selection.
- `/review?fixture=1` loads `ui/fixtures/review.json` under a large orange **TEST DATA** banner. Nothing is sent to the API in this mode.

## Fixture mode (exact behaviour)

- Lanes: Hall day 1 (UTC), Hall import (media-relative clock), Street (UTC), Perimeter (24 h, ~2950 generated matches), Lobby (media missing), Gallery (40 still images), Dock (live, not recorded).
- Matches (about 3010) are generated in `review.js` from a seed per lane (`gen` in the JSON). Detections for `/observations` are generated the same way, so boxes are NOT real detections.
- Playable video: media with `fixture_stream` point at the legacy route `/api/media/<camera id>` (`hall`, `street`; `vision/footage/*.mp4`, Range supported). Perimeter has `fixture_wrap_us`: it claims 24 h but plays the 54 s Street file, seeking to `t mod 54 s`. The clock shows the claimed 24 h time.
- Filmstrip tiles in fixture mode are captured from a hidden `<video>` (one seek per tile) instead of `/thumbnails` sprites.
- Reviews, manifests and exports are simulated in memory (`fixApi` in `review.js`). "Simulate 409" makes the next save conflict. A review title containing `FAIL` makes the simulated export fail.
- Run head only against an isolated root: `CW_ROOT=<tmp>` with `vision/` copied, `footage` and `models` symlinked and `rust/src/watch/ui` symlinked to the repo ui dir.

## Layout

Dense VMS chrome: summary bar (sources, scope, results, coverage, models, pending, duration, snapshot); left Results panel (virtualized list, filters, group by camera, sort); centre player with canvas overlay; right panel with Details and Query tabs; bottom timeline (about 40% of height) with ruler, lanes, matched-only sequence rail and selected-clips rail. Left panel, right panel and timeline are resizable (drag, or focus the splitter and use arrow keys) and collapsible (Results / Panel buttons, `«` `»`). Sizes persist in localStorage when available. At 760 px and below the layout is player-first with Player / Results / Details / Query tabs and a horizontally scrollable timeline; tested at 320 px.

The Query tab shows the run SQL or Lua with syntax highlighting, read-only. Editing lives on `/find`.

## Time model

Each lane has its own origin (recording start), so lanes never imply simultaneity. UTC lanes show their capture start in the gutter. Media-relative recordings are labelled "media time — capture clock unknown". "align UTC" aligns lanes that have a capture clock by their UTC start (media-relative lanes are never aligned). The playhead is drawn through its own lane only. Matched interval = solid strip with filmstrip; context = dashed brown band; trim = orange brackets (selected clips only); context handles = grey bars. "Matched only" removes gaps between matches per lane and says so on the ruler.

Frame accuracy: browser seeking is not frame-exact. Requested times are snapped to the media timebase (`MediaAsset.timebase`), and after each seek the status line shows requested vs actual `currentTime`, delta in ms, timebase and the frame duration (measured with `requestVideoFrameCallback` when available, otherwise assumed from fixture fps or 30 fps and labelled "assumed").

Overlays: observations are fetched in 30 s chunks and drawn only when `|obs time - currentTime| <= half the sample step` (step = median spacing of observations in the chunk). Otherwise the canvas is cleared and the HUD says why. Relations are labelled approximate. Track trails use observations in the previous 4 s. Zones draw only when the media asset carries zones.

## Keyboard map

| Key | Action |
| --- | --- |
| Space | play / pause |
| J or `[` / K or `]` | previous / next match |
| `,` / `.` | frame step back / forward (paused only) |
| Left / Right | seek 1 s (Shift: 5 s) |
| `+` / `-`, Ctrl+wheel | zoom timeline |
| F / Shift+F | fit selection / fit all |
| O | original-time / matched-only view |
| M / L | mute / loop trim range |
| `<` / `>` | slower / faster |
| B, Shift+L, C, R, T, Z | overlays: boxes, labels, confidence, relations, tracks, zones |
| A | add or remove current match in selection |
| V / X / E / U | confirmed / rejected / reviewed / unreviewed |
| Ctrl+S | save selection |
| Alt+Left / Right (clips rail) | reorder focused selected clip |
| Delete (clips rail) | remove clip |
| ? | keyboard map |

## Walkthrough

1. Open `/review?run=<id>`. Summary bar shows result count, coverage, models, pending, duration.
2. Click a strip, a result row or a sequence block: its media loads and seeks to the match in-point. Click inside the current strip to seek within it. Click or drag the ruler to scrub.
3. Play with Space; the playhead follows. J/K move between matches.
4. Ctrl+wheel or +/- to zoom, F to fit the match. Drag grey handles for context, orange handles for trim (adds the match to the selection).
5. Toggle overlays. Mark Decision, type a note, add to selection. Drag clips in the "Selected clips" rail to reorder (or Alt+arrows).
6. Save selection, download the evidence manifest, or start a rendered export (progress, cancel, error shown). A 409 on save shows a conflict callout with "Reload latest" and "Keep mine, save on top".

## API calls per action

| Action | Call |
| --- | --- |
| load | `GET /api/v1/queries/{id}`, `GET /queries/{id}/timeline`, `GET /queries/{id}/matches?limit=500` (then `&cursor=` pages in background), `GET /models`, `GET /media/{id}` per lane |
| play | `GET /media/{id}/stream` (browser Range requests) |
| filmstrip | `GET /media/{id}/thumbnails?start_us&end_us&step_us` for visible strips only (max 4 in flight, newest first, stale requests dropped) |
| overlay / evidence | `GET /media/{id}/observations?start_us&end_us` |
| evidence image | match `thumbnail`, else `GET /media/{id}/frame?t_us=` |
| save | `POST /reviews {title, items, notes}`, then `PATCH /reviews/{id} {version, title, items, notes}`; `409` shows conflict UI |
| restore | `GET /reviews/{id}` |
| manifest | `POST /reviews/{id}/manifest` (saves first), downloaded as JSON |
| export | `POST /exports {review_id, review_version, kind:"sequence", reencode:true}`, poll `GET /exports/{id}`, `POST /exports/{id}/cancel` |

All errors read the `{error: ApiError}` envelope and show `code: message`.

## Performance evidence

`docs/evidence/f/perf.json`: Chrome, 2950 strips on one lane. 240-frame horizontal scroll: p95 frame 17 ms (60 Hz), 0 long tasks, max draw 7 ms with all 2950 strips in range, under 2 ms zoomed. DOM stays at about 460 nodes (list rows and strips are windowed; the timeline is a canvas).
