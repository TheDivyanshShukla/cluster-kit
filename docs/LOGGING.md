# Logging

Every head, agent, standby and `ck run` writes a structured log next to its normal console output. Console output (`println!`) is unchanged, so scripts that grep the head's stdout keep working.

## Files

`<CK_DATA or ck-data>/logs/<role>-YYYYMMDD.jsonl` (UTC day). Roles: `head`, `agent`, `standby`, `client`. One JSON object per line. When a file reaches `log_max_mb` it is renamed to `<role>-YYYYMMDD.<unix_ms>.jsonl` and a new one starts; the oldest files of that role beyond `log_keep` are deleted.

Fixed prefix on every line: `ts_ms` (unix ms), `level` (`error|warn|info|debug`), `role`, `node` (node id), `kind` (dotted name), then the fields of that kind. Lines from `say!` carry `msg` (the text printed on the console).

## What is logged (kind prefixes)

| Prefix | Examples |
|---|---|
| `job.` | `job.submitted`, `job.started`, `job.complete`, `job.done`, `job.failed`, `job.cancelled`, `job.resume` |
| `chunk.` | `chunk.requeue`, `chunk.failed`, `chunk.speculate`, `chunk.speculate_won`, `chunk.overlap`; with `log_chunks=1` (debug): `chunk.dispatch`, `chunk.done` (head), `chunk.start`, `chunk.end` (agent, `ms`, `unacked`, `unacked_bytes`) |
| `node.` | `node.joined`, `node.connect`, `node.disconnect`, `node.lost`, `node.state`, `node.admitted`, `node.denied` |
| `admission.` | `admission.denied` |
| `ha.` | `ha.standby_join`, `ha.standby_leave`, `ha.lag` (when the lag size class changes), `ha.quorum`, `ha.fence`, `ha.switchover`, `ha.promote`, `ha.recruit`, `ha.candidate` |
| `standby.` | `standby.follow`, `standby.snapshot`, `standby.apply_error`, `standby.link_end`, `standby.handover` |
| `power.` | `power.sleep`, `power.wake`, `power.eco` (decision with node list), `power.wol`, `power.error` |
| `tune.` | `tune.changed` (old -> new), `tune.sent` |
| `api.request` | `method`, `path` (no query string), `status`, `ms`, `user`, `ip`. Writes are `info`, reads `debug` (`warn` when status >= 400). No bodies. |
| `agent.` | `agent.connect`, `agent.head_lost`, `agent.head_switch`, `agent.admitted`, `agent.presence`, `agent.calibration`, `agent.resend`, `agent.cap`, `agent.unacked_drop`, `agent.standby_start` |
| `client.` | `client.submit`, `client.reattach`, `client.head_lost` |
| `log.dropped` | `count`: lines lost because the queue was full |

All head events stored in SQLite (`/api/v1/events`) are logged by the same call (`Store::log_event`) with `actor` and `detail`.

## Knobs (`ck admin tune`, group Logging)

| Key | Default | Meaning |
|---|---|---|
| `log_level` | 3 | 0 off, 1 error, 2 warn, 3 info, 4 debug |
| `log_chunks` | 0 | 1 = log every chunk dispatch/result (about 190 bytes each; roughly 1.5 MB/s at 2500 chunks/s) |
| `log_max_mb` | 64 | size at which a file rotates |
| `log_keep` | 14 | files kept per role |
| `log_flush_ms` | 200 | write-out interval; also the most a crash can lose |

Changes apply within one flush interval. Agents get the values from the head.

## Overhead

Callers do one `try_send` into a bounded queue (16384 lines); a writer thread formats, scrubs and writes. A full queue counts a drop and never blocks. Measured on the head with 200 virtual agents, `ck run --synth 1000 --n 40000000 --chunk 5000 --policy fixed`: 11.8 M/s with `log_chunks=1` against 11.8 M/s with logging off (6 runs each, interleaved), about 5.2 MB of log per run.

## Reading

```
ck logs --role head --since 10m --kind node. --level info [--limit N] [--follow]   # local files
curl -H "Authorization: Bearer $TOKEN" "$HEAD/api/v1/logs?since_ms=...&kind=ha.&level=warn&limit=200"   # operator role
jq -c 'select(.kind|startswith("chunk.")) | {job,a,b,ms}' ck-data/logs/head-*.jsonl
python3 -c 'import sys,json;[print(l["ts_ms"],l["kind"]) for l in map(json.loads,sys.stdin) if l["level"]=="warn"]' < ck-data/logs/head-*.jsonl
```

`--kind` is a prefix match. `ck logs` reads the files of the PC it runs on (run it on the head, or use the API).

## Privacy

Never written in clear: tokens, passwords, node keys, join codes, cookies. Any field whose name contains `token`, `password`, `secret`, `key`, `join_code`, `cookie`, `bearer`, `auth` or `hmac` is replaced by `h:` plus 8 hex characters of its SHA-256 (nested objects too). API bodies and query strings are never logged. Call sites should not put secrets into `msg`/`detail` text; the scrub only sees field names.
