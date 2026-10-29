# Lua queries

`POST /api/v1/queries` accepts `"lua"` instead of `"sql"` (not both; `class` / `min_confidence` are rejected with it).

```json
{"lua": "<script>", "scope": {"source_ids": [], "media_ids": [], "from_utc_us": null, "to_utc_us": null},
 "gap_us": 2000000, "pre_us": 3000000, "post_us": 3000000, "limit": 100000, "actor": null}
```

The response is the same as for SQL: `{"run", "matches", "next", "unit", "distinct_tracks", "values"}`.
`run.normalized` is `lua:<sha256 of script>`; the same script, scope and snapshot gives the same run id.

## API

- `db.detections{class=, classes={}, min_confidence=, source_ids={}, media_ids={}, model_version=, track_id=, from_us=, to_us=}`
  rows: `observation_id detection_id source_id media_id class confidence model_version track_id media_time_us capture_time_utc_us`
- `db.frames{...}`: `observation_id source_id media_id media_time_us min_confidence counts relations`.
  `counts.person`, `relations['holding:cell phone']`. Unknown or unsupported counts are `nil`, never 0.
- `db.relations{predicate='near'|'holding', ...}`: `relation_id observation_id detection_id predicate object_class track_id confidence`.
  `holding` without an installed Holding-capable model fails with `unsupported`.
- `db.tracks{...}`: `key track_id media_id class confidence start_us end_us observations`.
- Each returns an iterator: `for row in db.detections{...} do ... end`. Scans are paged and counted against the same row/time budget as SQL.
- `emit(row | observation_id)` adds evidence; windows are built like SQL (gap merge, pre/post context, same-frame dedupe). Emit a row you have already read.
- `collect(it)`, `count(it | table)`, `distinct(it | table, key)` (`track_id` is scoped per media).
- `return` a number, string, boolean or table: it goes to `values`.

Sandbox: base/math/string/table only (no io, os, package, debug, load, require). 16 MiB memory, 100M VM instructions, 5 s.
Instruction/time/scan overruns return what was emitted plus a `partial results` warning. Memory overruns and script errors
are `runtime_error` (message has `lua:<line>:`); syntax errors are `parse_error`.

## Examples

1. Frames where at least 20% of people hold a phone (needs a Holding model):
```lua
for f in db.frames{} do
  local p, h = f.counts.person, f.relations['holding:cell phone']
  if p and h and p > 0 and h / p >= 0.2 then emit(f) end
end
```

2. People who share a frame with a phone, nested loops:
```lua
local phones = collect(db.detections{class = 'cell phone', min_confidence = 0.5})
for p in db.detections{class = 'person'} do
  for _, ph in ipairs(phones) do
    if ph.observation_id == p.observation_id then emit(p) emit(ph) end
  end
end
```

3. Aggregate only: distinct person tracks that stay longer than 10 seconds:
```lua
local n = 0
for t in db.tracks{class = 'person'} do
  if t.end_us - t.start_us > 10000000 then n = n + 1 emit(t) end
end
return {long_tracks = n}
```
