# Advisor

The advisor watches the same numbers as the alerts (time breakdown, node losses, tunables, logs, disk) and proposes fixes. Each suggestion lists several options, each with an expected effect, a cost, a risk, a time to effect, how the estimate was made and how success will be checked. Code: `rust/src/advisor.rs` (pure rules, tested) and the end of `rust/src/api.rs` (gathering and applying).

## How it works

- Every 10 s the head builds one observation, runs `advise`, and shows a suggestion after its condition held for the rule's hold time (0 to 30 min); it stays until the condition was gone for 60 s.
- Options are ranked by effect per cost. The best real option is `recommended`. "Do nothing" is always last and says what happens then.
- Numbers come from the head's own measurements (breakdown counters, loss events, disk, tunables). Where a model is used (chunk wait scales with 1/chunk length, power draw from `sim.rs` constants) the option's `confidence` says so and shows the inputs.
- One click (`apply`) runs the same handler as the normal endpoint (`PUT /tunables`, `POST /nodes/state`, `PUT /ha/settings`, `PUT /power`), writes the event `advice.applied` and snoozes the suggestion for 10 min.
- Follow-up: after 10 min the advisor compares the option's metric with the prediction and logs `advice.outcome` (held if at least half the predicted gain showed). `GET /advice/history` lists every applied option with its outcome and the hit rate per rule. A rule whose last 10 judged predictions held under 50 % (at least 3) is marked "confidence lowered" and loses `auto`.
- Undo: `POST /advice/{id}/undo` restores the old value. Preview: `POST /advice/{id}/preview` shows what would change and changes nothing.

## Controls

- Global switch and per-kind policy `off | suggest | auto` (admin, `PUT /advice/settings`), stored in the head database (meta `advisor`). Default: on, `suggest`.
- `auto` runs the recommended option by itself only if it is auto-safe (reversible, inside the tunable's bounds, or a drain/cordon/eco switch), at most once per 10 min per suggestion, and logs `advice.auto` (undo as above). Options with a risk of data loss (deleting parts or events) or without an action are never auto.
- Dismiss with a snooze (`POST /advice/{id}/dismiss {"snooze_h"}`, operator).

## Catalogue

Kinds are the policy keys. Auto-safe: yes means policy `auto` may do it.

| Situation | How detected | Suggested solution (options) | Action | Auto-safe |
|---|---|---|---|---|
| head_hot alert, or PCs wait for the head (dispatch_wait > 15 % of PC time) | alert `head_hot`; breakdown `dispatch_wait` over running/recent jobs | longer chunks (`target_chunk_s` x2); deeper queue (`pipeline` +1); lab heads (`hier_mode` 2 automatic, or 1 and pin by hand; expected root load from the sim cost model) when a lab has `lab_head_min_pcs` or more PCs | tunable | yes |
| Pinned lab head partitioned, released or absent | hierarchy tree | move the lab head to the best PC; unpin | steps | no |
| Lab head lost in the last hour and `lease_s` over 60 s | events `labhead.leave` | halve `lease_s` | tunable | yes |
| Lab head went silent in the last hour and `lease_s` under 60 s | events `labhead.partition` | double `lease_s` | tunable | yes |
| flaky_pc alert / node.lost > `alert_flaky_losses` in 24 h | events `node.lost` | drain; cordon; `lost_after_s` x2 (3 or more flaky PCs); look at the PC | node state, tunable, steps | drain yes |
| slow_pc alert | alert, breakdown tail share | cordon; find the cause | node state, steps | yes |
| no_standby alert, or no standby configured with 3+ PCs | alert; HA settings | add one standby (`target_standbys` 0 -> 1); make a PC eligible (best PC named from head ranking); lower the target | HA target, steps | no |
| standby_behind alert | alert | raise `ring_max` x2; check link and disk | tunable, steps | yes |
| reserve_at_risk alert | alert | bring PCs online; approve pending PCs; lower keep_n | steps | no |
| Job waits on a quota while PCs idle | `wait_reason=quota`, idle PCs | raise the user's quota | steps (PUT /users/NAME/quota) | no |
| Disk 85 % full or under 5 GB free | `df` on CK_DATA | `parts_keep_days` -> 30; `events_keep_days` -> 7; free space by hand | tunable, steps | no (deletes data) |
| Many idle PCs, no jobs, power off (30 min) | power settings, nodes | eco mode (kWh/day from sim.rs constants) | power mode | yes |
| Blobs sent from the head to 6+ PCs in direct mode | job spec `blob_mode`, `head_bytes_sent` | submit with `blob_mode=peer`, pre-stage | steps | no |
| Job failed: output cap | error text "output" | `out_max_mb` x2; print less | tunable, steps | yes |
| Job failed: every try used | error "tries"/"attempt" | `max_attempts` +2; fix the program | tunable, steps | yes |
| Job failed: chunk time limit | error "timeout", event `task.timeout` | smaller chunks; check the PC | steps | no |
| Job failed: blob | error "blob" | re-upload; check blob.corrupt | steps | no |
| Job failed: no PC matches | error "selector"/"no pc" | loosen the selector (`ck select`) | steps | no |
| Job failed: quota | error "quota" | raise the quota | steps | no |
| Job failed: other | any other error | read `ck job show`, `ck logs --kind chunk.failed` | steps | no |
| store.corrupt, store.missing | warn log | restore newest backup, fail over | steps | no |
| job.error, event.error, node.error | warn log | check disk space | steps | no |
| task.timeout | event | smaller chunks, check the PC | steps | no |
| node.denied | event | new join code; approve the PC | steps | no |
| ha.mixed_versions | warn log | finish the rolling upgrade | steps | no |
| blob.corrupt, blob.fetch_failed, blob.port, blob.standby_copy_failed, blob.standby_missing | warn log | re-upload, check network/ports, standby disk | steps | no |
| power.error, power.sleep_failed | warn log | check WoL / rights; keep PCs awake | steps | no |
| standby.apply_error, standby.link_end | warn log | full copy, check link | steps | no |

A unit test fails if an alert kind of `research.rs` has no rule, and if a kind in `LOG_FIXES` yields no steps.

## Not covered yet

Output-cap hits that do not fail a job (agents truncate silently, the head sees nothing), idle PCs while a job is held by its own `max_pcs`/placement (not in the job row the API exposes), "enable lab heads" (feature not present), and estimates from a full `sim.rs` run (only its power constants are used). Advisor state is kept only on the primary; a standby shows nothing until it promotes.
