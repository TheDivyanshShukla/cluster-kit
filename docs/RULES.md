# Watch rules and alerts

Rules run on `FrameSummary` (one per inference output; every rule sees the same one). Duration is
elapsed MEDIA time in milliseconds (`hold_ms`), never a frame count. Unknown is not zero: a class the
models did not analyze, an unsupported relation, a blurry frame, or a zero denominator makes the
condition Unknown, and `unknown_policy` (default `break`) decides what that does to a hold window.

## Condition kinds

- `{"dsl": "count(\"cell phone\") >= 4"}` legacy DSL (`seen`, `count`, `ratio`, `lua:`), evaluated three-valued.
  These count VISIBLE objects, not phone holders.
- `{"lua": "<source>"}` typed Lua. Either `return function(ctx) ... end` or a bare expression over `ctx`.

## ctx (Lua)

| call | returns |
| --- | --- |
| `ctx:count(class)` | distinct objects of the class, or `nil` when Unknown/Unsupported |
| `ctx:count_people()` | `count("person")` |
| `ctx:count_relation(pred, class)` | distinct subjects of `pred:class` (`pred` = `'holding'` or `'near'`), `nil` if unsupported |
| `ctx:ratio_people_with(pred, class)` | distinct subjects / people in the SAME observation; `nil` if either is unknown or people = 0 |
| `ctx.source_id`, `ctx.zone`, `ctx.media_time_us` | scope facts |

`nil` propagates: a script that compares a `nil` makes the rule Unknown (never false). Return `nil`
yourself to say "cannot tell". The sandbox has no io/os/package/require/load/dofile/debug, 4 MiB of
memory and 500k VM instructions per evaluation; violations become diagnostics (`budget_exceeded`,
`memory_limit`, `rule_error`) and the rule fails closed (no alert).

## Executable examples

These blocks are compiled and run by the tests in `rust/src/watch/alerts/tests.rs` against
`contracts/frame_summary.v1.json` (60 people, 14 phones, 12 holding). `expect` is the result.

Phone-holder share of people at least 20% (12/60 = 0.20). Needs a relation module that supports
`holding`; with the current models (`near` only) this is Unknown, not false.

<!-- expect: true -->
```lua
return function(ctx)
  local r = ctx:ratio_people_with('holding', 'cell phone')
  if r == nil then return nil end
  return r >= 0.20
end
```

The same on a frame whose models cannot say who is holding (`holding` unsupported) stays Unknown, see tests.

Four or more visible phones (objects, not holders):

<!-- expect: true -->
```lua
ctx:count('cell phone') >= 4
```

Crowd of at least 50 people (pair with `hold_ms = 10000` for "for 10 s"):

<!-- expect: true -->
```lua
ctx:count_people() >= 50
```

Crowd and many holders, with an explicit unknown guard:

<!-- expect: true -->
```lua
return function(ctx)
  local holders = ctx:count_relation('holding', 'cell phone')
  if holders == nil then return nil end
  return ctx:count_people() >= 50 and holders >= 4
end
```

A class that was not analyzed is Unknown, not "zero knives":

<!-- expect: unknown -->
```lua
ctx:count('knife') == 0
```

## Rule spec

```json
{"id":"crowd-10s","version":"1","name":"Crowd for 10 s","scope":{"source_ids":["classroom-a"],"zone":null},
 "condition":{"lua":"ctx:count_people() >= 50"},"hold_ms":10000,"cooldown_ms":60000,
 "severity":"warning","min_confidence":0.4,"unknown_policy":"break","max_gap_ms":5000,"enabled":true}
```

- `hold_ms`: condition true continuously over >= this elapsed media time.
- `max_gap_ms` (default 5000): consecutive observations further apart break the window (dropped frames, stalls).
- `cooldown_ms`: after an alert resolves no new alert starts for this long.
- `min_confidence`: summaries built with a lower threshold are Unknown for the rule.
- `unknown_policy`: `break` (default; unknown ends a pending window and resolves an active alert), `hold`
  (tolerate unknown up to `max_gap_ms` since the last known observation), `ignore` (discard unknown observations).
- `version`: changing it resets that rule's state explicitly (pending dropped, active alerts resolved). Use
  `RuleSpec::fingerprint()` if you have no better version.

## Lifecycle and decisions

`Pending -> Active -> Resolved` is internal machine state; `Active` and `Resolved` alerts are emitted. The alert id is
`stable_id(rule id, version, source, media, zone, start_us)`, so retries, failover replays and restarts produce the same
id and cannot duplicate. `Decision` (`unreviewed|confirmed|rejected`) is separate and set only through `decide`;
machine updates never overwrite it. Alert `start_us`/`end_us` are media time on `clock = {kind: media, media_id}`
and are directly usable as a timeline interval.

## Legacy migration

`RuleSpec::from_legacy` maps `hold` (a FRAME count) to `hold * 1000` ms and sets `migration_warning`: it assumes about
one sampled frame per second and must be reviewed. Legacy rules use `unknown_policy = hold` (they froze on unusable frames).
