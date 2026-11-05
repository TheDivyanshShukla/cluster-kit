# Audit round 2 (T55, agent g6)

Method: secured fleets (join codes, node keys, sealed links) on one Mac, ports 9660-9699, `CK_POWER_DRY=1`. Scripts in `scripts/stress/`
(`CK_BIN=... STRESS_DIR=... CK_PORT=... python3 -I scripts/stress/<name>.py`). Run against `git archive HEAD` and again against the
working tree with the wave 14 changes (g1 output cap, g2, g5). Patches are in the g6 scratchpad (`standby-pidfile.patch`, `alerts-limit.patch`).

## Results that passed (no finding)
| Test | Script | Result |
| --- | --- | --- |
| 20 min soak, 25 SSE viewers, 3 agents, synth job | `soak_viewers.py` | head RSS 11.2 -> 12.7 MiB, fds 50 -> 53, threads 94 -> 96, 29.4k -> 29.3k numbers/s (about 14.6 chunks/s), 0 viewer errors, API 1-5 ms. No drift. |
| kill -9 one agent mid job | `chaos_secured.py agent9` | PASS, exact count |
| kill -STOP 20 s then -CONT one agent | `chaos_secured.py agentstop` | PASS, exact count |
| kill -9 primary, standby promotes (same ports) | `chaos_secured.py primary9` | PASS, exact count, 28 s (HEAD) / 60 s (tree) |
| 100k changes replayed to a stopped standby | `repl_100k.py stop` | caught up in 0.5 s, standby 13 MiB, primary peak 146 MiB (ring of 100k entries, about 1.2 KiB each; bounded by `ring_max`) |
| 10,000-item list job | `limits.py list10k` | done in 1 s, every item once, head 20 MiB |
| 1 GB of stdout | `limits.py stdout` | stopped at once: "output over the 64 MiB limit", head 16 MiB (needs T50; HEAD has the old behaviour) |

## Findings
1. **Orphan standby heads (real, medium).** `agent.rs` `start_standby` (about line 275) keeps its "already started" guard in a per-process
   static. After `kill -9` or a restart of the agent, the standby it spawned keeps running (it is a child, not killed with the parent),
   and the new agent process starts a second one on the same `CK_DATA/standby`. Seen: after cleanup of killed agents, standby heads survived
   and later promoted themselves and took the ports; `ha.standbys` listed extra entries. Patch `standby-pidfile.patch`: a pid file in the
   standby directory, skip the spawn when that pid is alive (unix). Builds on the tree. Owner g1 (agent.rs).
2. **GET /alerts silently shows 500 of 1,000 (low).** `api.rs:542` (tree) passes a fixed limit 500. Measured: 1,000 `alert_open` 34 ms,
   1,000 close 18 ms, but the list shows 500. Patch `alerts-limit.patch`: `?limit=` (default 500, max 5000) and `"truncated": bool`.
   Owner g5 (api). Console: show "500+" when `truncated`.
3. **Cmd jobs have no shell (doc gap).** `cmd` is split on spaces and executed directly, so `a | b` fails with "exit 1: head: 2: No such file".
   Say so in the job form and in BULK.md/SDK docs, or accept `sh -c`.
4. **One join code covers at most 500 PCs** (`api.rs` join_code, `uses` 1 to 500): a 1,000 virtual PC test needs two codes. By design; note in OPERATIONS.md.
5. **Single machine test limits (not product bugs).** A SIGSTOPped primary keeps its ports, so the standby needs its own; the UDP beacon port
   then clashes ("a higher epoch will not be noticed"), agents only find the new head through `CK_HEADS`, and the resumed old primary stays at
   epoch 1 and dispatches (no fencing without beacons). Fencing relies only on UDP beacons: if beacons are blocked between VLANs,
   an old primary that comes back is not fenced. Needs two PCs to test (lab pilot). `chaos_secured.py primarystop` is kept for that.
6. **Clock skew: not tested** (no libfaketime here). By code reading, `head_json` `since_ms` and node `last_seen_ms` use the local head clock and
   are replicated as-is, so after a takeover between heads with skewed clocks "since" values and last-seen ages are off by the skew; agents
   send no timestamps the head trusts. Test on two PCs with `date -s +3min`.

## Notes
- Overlaps with T50/T51/T54: finding on output cap is fixed by T50; replay/ring numbers are for the current T51 code.
- `scripts/stress` is ruff clean with the repo's `ruff.toml`.
