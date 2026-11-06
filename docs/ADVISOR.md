# Advisor

The advisor watches the same numbers as the alerts (time breakdown, node losses, tunables, hierarchy, logs, disk) and proposes fixes. Each suggestion has several **options**; each option says what it should do, what it costs, how risky it is, how fast it works, how the estimate was made and how to tell it worked. You can preview, apply, undo or dismiss from the console, the CLI or the API. Applied options are followed up: ten minutes later the advisor compares what it predicted with what it measured and keeps a hit rate per rule.

Readers: **operators** use sections 1 to 5; **admins** set policy (section 3); **reviewers** read sections 1, 4 and 8 for how predictions are checked.

Code: `rust/src/advisor.rs` (pure rules and tracking, unit tested) and the end of `rust/src/api.rs` (gathering the observation, applying through the normal handlers, storing state).

## 1. How it works

- Every 10 s the primary head builds one observation, runs the rules, and shows a suggestion after its condition has held for the rule's **hold time** (0 s to 30 min, listed below). A suggestion stays until its condition has been gone for 60 s. Standbys run no advisor and show nothing until they promote.
- Options are ranked by effect per cost (`score`). The best real option has `recommended: true` (marked `*` in the CLI). **Do nothing** is always last and says what happens if you leave things as they are.
- Numbers come from the head's own measurements (breakdown counters, loss events, disk, tunables, the hierarchy tree). Where a model fills a gap (chunk wait scales with 1/chunk length, power draw from the `sim.rs` constants, lab-head load from the `sim.rs` cost model) the option's `confidence` says so and shows the inputs.
- Severity: `warn` (act soon), `tip` (an improvement), `info`. The list is sorted warn, tip, info, then by id. A change in the list is pushed over SSE as an `advice` event.
- State (on or off, per-kind policy, snoozes, the last 200 applied options) lives in the head database (meta `advisor`) and replicates to standbys with the other settings.

## 2. The fields of an option

| Field | Meaning |
|---|---|
| `id`, `title` | Short name (`chunk_up`, `drain`, ...) and a sentence. Use the id with `apply ID OPTION`. |
| `action` | What one click does: set a tunable (`tune`), change node state (`node_state`: drain or cordon), set the standby target (`ha_target`), set power mode (`power`), or `doc` (nothing to click). Every click goes through the same handler as the normal endpoint (`PUT /tunables`, `POST /nodes/state`, `PUT /ha/settings`, `PUT /power`). |
| `steps` | Manual steps for things the head cannot do itself (replace a cable, re-upload a file). May come with an action. |
| `expected_effect` | What should happen, with numbers where there are some. |
| `metric`, `metric_from`, `metric_to` | The measured number the option should move, from where to where. Used for the follow-up check. Options without a metric are never judged. |
| `cost` | What you give up. |
| `risk` | `low`, `med` or `high`. |
| `reversible` | True if undo restores the old value. Deleting data (`parts_keep_days`, `events_keep_days`) is not reversible. |
| `auto_safe` | True if policy `auto` may run it: reversible, inside the tunable's bounds, or a drain, cordon or eco switch. |
| `time_to_effect` | `immediately`, `next job`, `about 20 s`, `hours`, ... |
| `confidence` | Measured, model or rule of thumb, with the inputs. After three or more judged outcomes it also states how often this rule's predictions held. |
| `success_check` | How you will know it worked. |
| `recommended` | The top-ranked real option. |
| "Do nothing" (`id: nothing`) | Always present, always last, never recommended; its effect line is the consequence of leaving it. |

## 3. Policy: off, suggest, auto

- A global switch (default on) and a policy **per kind** (default `suggest`). Admin only: `PUT /api/v1/advice/settings`, `ck admin advice on|off`, `ck admin advice policy KIND off|suggest|auto|default`, or the console Policy tab. Kinds are listed in section 6; the policy key is the suggestion's `kind`.
- `off`: that kind is never shown. `suggest`: shown, you decide. `auto`: the advisor applies the recommended option itself if it has a one-click action, is auto-safe and reversible, at most once per 10 minutes per suggestion; it logs `advice.auto` with actor `advisor`, and you can undo it like any other.
- Never automatic: options with a risk of data loss, options without an action, and any kind whose last 10 judged predictions held under 50 % (with at least 3 judged). Its options then say "confidence lowered, auto off".
- Turning the advisor off shows nothing and runs nothing; stored history stays.

## 4. Preview, apply, undo, dismiss, and tracking

| Step | CLI | API | Role |
|---|---|---|---|
| Preview: what would change, nothing changes | `ck admin advice preview ID [OPTION]` | `POST /api/v1/advice/{id}/preview` `{"option"}` | operator |
| Apply | `ck admin advice apply ID [OPTION]` | `POST /api/v1/advice/{id}/apply` `{"option"}` | admin |
| Undo: restore the old value | `ck admin advice undo ID` | `POST /api/v1/advice/{id}/undo` | admin |
| Dismiss: hide for a while | `ck admin advice dismiss ID [HOURS]` | `POST /api/v1/advice/{id}/dismiss` `{"snooze_h"}` | operator |

Without an option the recommended one is used. Apply re-checks the suggestion against fresh data first and answers 409 if it no longer applies; a second apply within the follow-up window answers 409 ("undo it first"). Applying snoozes the suggestion for the follow-up window (10 min). Undo snoozes it for 1 h. Dismiss defaults to 24 h and is clamped to 0 to 720 h. A preview of a tunable change shows the current value, the new value and whether the new value is inside the tunable's range.

**Predicted against observed.** At apply time the advisor stores the metric before and the predicted value. After 10 minutes it reads the metric again and logs `advice.outcome`: the option **held** if the observed gain is at least half of the predicted gain, **missed** otherwise (also missed if the metric is unavailable or the option had none). Undone options are not judged. `GET /api/v1/advice/history` lists every applied option (newest first, 200 kept) with who, when, auto or not, before, predicted, observed and the verdict, and `accuracy` per kind (share of the last 10 judged that held; absent below 3). The console shows this under Applied and outcomes.

Audit events: `advice.applied`, `advice.auto`, `advice.undone`, `advice.dismissed`, `advice.outcome`, `advice.settings`.

## 5. Where to use it

- **Console `/advisor`** (nav "Advisor", with a count badge). Three tabs: Suggestions (cards with the options, effect, cost, risk, time to effect, confidence, check; Preview, Apply, Dismiss and a Undo for what was just applied), Applied and outcomes (history with verdicts and Undo), Policy (the switch and a selector per kind). Viewers can read; operators can preview and dismiss; admins can apply, undo and change policy. The page says which of these you may do.
- **CLI** `ck admin advice` (list), `history`, `preview|apply|undo|dismiss`, `policy`, `on|off` as above.
- **API** `GET /api/v1/advice` (viewer; `enabled`, `policy`, `suggestions`, `accuracy`), `GET /api/v1/advice/history` (viewer), `PUT /api/v1/advice/settings` `{"enabled", "policy": {"KIND": "off|suggest|auto|null"}}` (admin; null resets to default), and the `POST /api/v1/advice/{id}/{preview|apply|undo|dismiss}` calls above. Full list in [API.md](API.md).

## 6. Suggestion kinds and their options

Kinds are the policy keys. "Hold" is how long the condition must persist before it is shown. "Auto-safe: yes" means the recommended option may run under policy `auto`.

| Kind (id) | Situation and detection | Options | Action | Auto-safe | Hold |
|---|---|---|---|---|---|
| `head_hot` / `dispatch_wait` (id `head_hot` or `dispatch_wait`) | Alert `head_hot`, or PCs wait for the head (dispatch wait over 15 % of PC time in the breakdown of running or recent jobs) | `chunk_up`: `target_chunk_s` x2. `pipeline_up`: `pipeline` +1. `lab_heads_auto`: `hier_mode` 0 to 2 (only with lab heads off and a lab with at least `lab_head_min_pcs` PCs; root load before and after from the sim cost model). `lab_heads_manual`: `hier_mode` 0 to 1, then pin per lab | tunable | yes | 30 s |
| `lab_head` (id `lab_head:LAB`) | A pinned lab head is partitioned, released or absent | `move` to the best PC (`ck admin hier pin`); `unpin` | steps | no | 20 s |
| `lease_length` (ids `lease_long`, `lease_short`) | Lab heads on and a lab head lost in the last hour with `lease_s` over 60 s; or a lab head went silent in the last hour with `lease_s` under 60 s | `lease_down`: halve `lease_s`; `lease_up`: double `lease_s` | tunable | yes | 120 s |
| `flaky_pc` (id `flaky_pc:NODE`) | Alert `flaky_pc`, or a PC with more than `alert_flaky_losses` `node.lost` events in 24 h | `drain`; `cordon`; `lost_after`: `lost_after_s` x2 (only with 3 or more flaky PCs and `lost_after_s` at most 5); `investigate` | node state, tunable, steps | drain, lost_after yes | 0 |
| `slow_pc` (id `slow_pc:NODE`) | Alert `slow_pc` | `cordon`; `investigate` | node state, steps | cordon yes | 0 |
| `no_standby` | Alert `no_standby`, or no standby configured with 3 or more PCs online | `add_standby`: `target_standbys` 0 to 1; `recruit`: make an eligible PC available (names the best PC); `lower_target`; (the options depend on whether a target is set) | HA target, steps | no | 60 s if none was ever configured, else 0 |
| `standby_behind` | Alert `standby_behind` | `ring`: `ring_max` x2; `link`: check link and disk | tunable, steps | ring yes | 0 |
| `reserve_at_risk` | Alert `reserve_at_risk` | `add_pcs`; `approve` (if pending PCs exist); `lower_keep` | steps | no | 0 |
| `quota_wait` (id `quota_wait:USER`) | A running job has `wait_reason=quota` while 2 or more PCs are idle | `raise_quota` (`PUT /users/NAME/quota`) | steps | no | 60 s |
| `disk` | Head disk 85 % full or under 5 GB free | `parts`: `parts_keep_days` to 30; `events`: `events_keep_days` to 7; `free`: by hand | tunable, steps | no (deletes data) | 0 |
| `eco` | 4 or more PCs online, no job running, power mode off | `eco`: eco mode (kWh a day from the sim constants) | power mode | yes | 30 min |
| `blob_peer` | 6 or more PCs online and a running job with blobs in direct mode | `peer`: submit with `blob_mode=peer` | steps | no | 30 s |
| `job_fail` (id `job_fail:REASON`) | Jobs failed in the last day, grouped by error text | by reason: `output_cap` (`out_up`: `out_max_mb` x2, auto-safe; `trim`), `attempts` (`retry_up`: `max_attempts` +2, auto-safe; `fix_job`), `timeout` (`smaller`), `blob` (re-upload), `no_pcs` (loosen the selector), `quota` (raise it), other (`read` the failure) | tunable, steps | output_cap and attempts yes | 0 |
| `log_event` (id `log:KIND`) | A warn or error log kind, or head event, in the last hour: `store.corrupt`, `store.missing`, `job.error`, `event.error`, `node.error`, `task.timeout`, `node.denied`, `ha.mixed_versions`, `blob.corrupt`, `blob.fetch_failed`, `blob.port`, `blob.standby_copy_failed`, `blob.standby_missing`, `power.error`, `power.sleep_failed`, `standby.apply_error`, `standby.link_end` | `fix`: follow the steps for that kind | steps | no | 0 |

A unit test fails if an alert kind of `research.rs` has no rule, and if a kind in `LOG_FIXES` yields no steps.

## 7. Reading a suggestion: an example

`ck admin advice` prints each suggestion as a severity, a title, its id and policy, the numbers that triggered it, and the options: `*` marks the recommended one, then effect, cost, risk, time to effect, confidence and the success check. To act on `head_hot` with the lab-head option:

```
ck admin advice preview head_hot lab_heads_auto     # shows hier_mode 0 -> 2, valid
ck admin advice apply head_hot lab_heads_auto       # admin; logs advice.applied
ck admin advice undo head_hot                       # puts hier_mode back to 0
```

## 8. Limits

Output-cap hits that do not fail a job (agents truncate silently, the head sees nothing) and idle PCs while a job is held by its own `max_pcs` or placement are not detected. Estimates come from the head's counters and a few simple models, not from a full `sim.rs` run (only its power constants and the lab-head cost model are used). Prediction hit rates need at least 3 judged outcomes per kind, so a new install trusts nothing automatic until it has history. The `lab_heads_auto` and `lab_heads_manual` options are offered only while lab heads are off. Only `--only` jobs pause the hierarchy (see [HIERARCHY.md](HIERARCHY.md) section 7).
