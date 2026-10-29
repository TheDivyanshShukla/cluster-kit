# Watch operations guide

Status: describes what exists on `main` plus what the auth module (`rust/src/watch/auth/`) provides once the
coordinator wires it. Anything marked **(not wired)** is built and unit-tested but not reachable over HTTP yet.
Do not treat this deployment as production-ready: see `docs/RELEASE_CHECKLIST.md` for open blockers.

## Build

```sh
cargo build --release --manifest-path rust/Cargo.toml --bin cw --no-default-features --features watch
# or: ./kit rs watch ...   (builds on first use)
```

Requires FFmpeg and ffprobe on `PATH` (on macOS Homebrew: `/opt/homebrew/bin`). The ONNX runtime is
downloaded at build time by the `ort` crate; use `--offline` only when the registry cache is warm.

## Run: one PC

```sh
./kit rs watch fetch     # sample clips + yolov5n.onnx into vision/
./kit rs watch head      # serves the panel on :7710 and starts a local worker
# open http://127.0.0.1:7710
```

Direct binary: `cw head [--port 7710] [--name lab] [--no-local-worker]`.

## Run: cluster

```sh
# PC 1 (head, also works):
cw head
# every other PC on the same LAN (discovers the head by UDP beacon on 7711):
cw worker
# or explicitly:
cw worker --head 192.168.1.20
```

Ports: **7710/tcp** panel and worker API, **7711/udp** discovery beacon. Workers must reach 7710 on the head.
The beacon is broadcast only on the local subnet. Across subnets use `--head <ip>`.

## Directories and `CW_ROOT`

All state lives under one root: `$CW_ROOT` if set, else the checkout (the directory containing `kit`), else the
directory above the binary that holds `vision/`.

| Path | Contents |
| --- | --- |
| `vision/watch.json` | sources (`cameras`), processing settings |
| `vision/modules.json`, `vision/rules.json`, `vision/searches.json` | detectors, rules, saved searches |
| `vision/models/` | ONNX model files |
| `vision/footage/` | approved media root for recordings |
| `vision/data/seen.jsonl` | durable detections (current JSONL index) |
| `vision/data/events.jsonl`, `audit.jsonl` | alerts and the audit trail |
| `vision/data/thumbs/` | exact-frame JPEG thumbnails |
| `vision/data/auth.json` | users and worker-token hashes, mode 0600 **(not wired)** |
| `vision/data/*.db` | SQLite metadata store (storage agent; WAL) when it lands |

Use an isolated root for experiments: `CW_ROOT=$(mktemp -d)` plus a copy of `vision/modules.json`, `rules.json`
and symlinks to `models/` and `footage/`. `scripts/watch_acceptance.sh` does exactly this.

## Models and licensing

`cw fetch` (`./kit rs watch fetch`) downloads, with no checksum pinning today:

- `yolov5n.onnx` from the Ultralytics YOLOv5 v7.0 release. **YOLOv5 is AGPL-3.0**. Running it as a network
  service and offering it to users triggers AGPL source-offer obligations for the combined work; get legal
  review before any external deployment. Alternative detectors must be added as ONNX files with an explicit
  licence, and `docs/MODELS.md` (agent C) tracks the model registry and validation status.
- Two sample clips from `intel-iot-devkit/sample-videos` (check their licence before redistribution).

The shipped model is COCO-80: it has no weapon class and no "holding" relation. "Weapon" and "fight" modules
without a model file report unsupported; they must never be read as "none found".

For offline installs copy the `.onnx` into `vision/models/` and verify it yourself (`shasum -a 256`); record the
hash with the deployment. Pinned checksums in `cw fetch` are an open item.

## Media roots

Only files under `vision/footage/` (plus directories listed in `CW_MEDIA_ROOTS`, a `:`-separated path list)
may be served or indexed once auth is wired. `auth::allowed_media` canonicalises the path, rejects `..` and any
symlink that resolves outside an approved root. Today `head.rs` serves whatever path a source points at
(**blocker**, see checklist).

## Retention

There is **no automatic deletion**. `seen.jsonl`, `events.jsonl`, `audit.jsonl` and `thumbs/` grow without bound;
the in-memory index is capped at 100,000 rows but the files are not. Until the storage agent's retention rules
land, retention is a manual operator decision: stop the head, archive the files, start fresh. Audit records must
not be deleted by ordinary users; keep `audit.jsonl` backups for your required retention period.

## Backup and recovery

- **Stop-the-world backup (always correct):** stop `cw head`, copy `vision/` (excluding `models/` and `footage/`
  if you can re-obtain them), start the head.
- **JSONL files** are append-only. A crash can leave a truncated last line; the loader skips unparsable lines
  but this has not been crash-tested (checklist: not run).
- **SQLite (when the storage agent lands):** the database runs in WAL mode, so `vision/data/*.db` has `-wal`
  and `-shm` side files. Do not `cp` a live database. Use `sqlite3 vision/data/<db> ".backup 'backup.db'"`
  while the head runs, or stop the head and copy all three files together.
- Legacy JSONL import is meant to be restartable and non-destructive: take a backup first and keep the JSONL
  files after migration.
- **Restore:** stop the head, put the files back, start the head. Sessions are in memory and are lost on every
  restart (everyone must log in again). Worker tokens and users survive (`auth.json`).
- `auth.json` is a secret (password hashes). Back it up like a credential store (encrypted, 0600).

## Authentication **(not wired)**

Implemented in `rust/src/watch/auth/mod.rs`; the coordinator integration steps are in the agent H report.

- **Modes.** No users and `CW_AUTH` unset: *open-trusted-lan*, every request is an anonymous administrator so
  the local demo keeps working; the head should bind `127.0.0.1` in this mode unless `CW_BIND` is set.
  `CW_AUTH=required` or any configured user: *enforced*. A corrupt `auth.json` fails closed (locked).
- **Bootstrap.** With no users the head prints a one-time bootstrap token on stderr
  (`auth: ... bootstrap token <hex> (shown once)`) or reads `CW_BOOTSTRAP_TOKEN` (min 16 chars, never echoed).
  Create the first administrator once:

  ```sh
  curl -X POST http://127.0.0.1:7710/api/v1/auth/bootstrap -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"at least 12 chars","bootstrap_token":"<token>"}'
  ```

  After that, bootstrap is refused (409). Passwords: 12 to 256 characters, PBKDF2-HMAC-SHA256, 210,000
  iterations, random 16-byte salt.
- **Sessions.** `POST /api/v1/auth/login` sets `cw_session` (HttpOnly, SameSite=Strict, add `CW_COOKIE_SECURE=1`
  behind TLS). 12 h idle expiry, sliding, 7 d absolute; logout invalidates; held in memory.
  Five failed logins per user or per IP in 5 minutes returns 429.

- **Signing in from the browser.** Open `/login` (every page links there when a sign-in is needed). After signing in,
  "Change password" on the same page calls `POST /api/v1/auth/password` with `{"current","new"}`.
- **Remote workers once auth is on.** An admin creates a token with `POST /api/v1/auth/worker-tokens {"name":"pc2"}`
  (shown once). Start the worker with `CW_WORKER_TOKEN=<token> cw worker --head <ip>`. The head's own local worker
  gets an ephemeral token automatically. With users configured the head binds `0.0.0.0` (override with `CW_BIND`).
- **Webcams.** Each worker reports its capture devices by index and name (macOS avfoundation, Linux `/dev/video*`,
  Windows dshow). A webcam source's location is the device index (e.g. `0`); only a worker that has that device
  runs it. `CW_DEVICES="0,FaceTime HD Camera"` overrides discovery.
- **Roles.** `read_only`: dashboards, state, sources list, query results, no footage. `reviewer`: plus media,
  thumbnails, live view, review decisions, exports. `operator`: plus source/rule/model/settings changes and the
  audit log. `administrator`: plus users (`POST /api/v1/auth/users`), worker tokens
  (`POST /api/v1/auth/worker-tokens`) and source credentials.
- **Workers** authenticate with `Authorization: Bearer cwt_...` (only a SHA-256 is stored). A worker token never
  works on user routes and a user session never works on worker routes (`/api/index`, `/api/v1/index`,
  `/api/work`, `/api/stats`, `POST /api/events`, `POST /api/preview/*`). Start a remote worker with the token in
  its environment once agent B's worker reads `CW_WORKER_TOKEN`.
- **Audit.** Use `auth::audit_record` so credentials are redacted from `detail` and `targets`.

## TLS

The head speaks plain HTTP. For anything beyond a trusted LAN put a TLS reverse proxy in front and bind the head
to loopback (`CW_BIND=127.0.0.1`). Caddy example:

```caddyfile
watch.example.org {
    reverse_proxy 127.0.0.1:7710 {
        flush_interval -1      # MJPEG live view and SSE must not be buffered
    }
}
```

Set `CW_COOKIE_SECURE=1` so the session cookie gets `Secure`. The proxy must pass `Range` and `Host`/`Origin`
unchanged (the auth module rejects cross-origin mutations by comparing `Origin` to `Host`).

## Measured limits

None are claimed here. Record real numbers (hardware, model, provider, resolution, sampling, FPS, p50/p95
latency, coverage) in `docs/evidence/` before quoting any. Known structural limits from the code: the in-memory
index keeps at most 100,000 detections, request bodies are cut at 2 MB, one index batch is capped at 10,000
detections and its thumbnail at 300 KB, `/api/media` serves at most 8 MiB per range request, and every query
re-reads `seen.jsonl` from disk.

## Acceptance harness

`scripts/watch_acceptance.sh` builds `cw`, runs it against an isolated temporary root and prints PASS, FAIL or
SKIP per check. Environment: `CW_BIN` (skip build), `CW_SKIP_BUILD=1`, `INDEX_TIMEOUT` (default 600 s),
`ACCEPT_REINDEX=0` (skip the restart-with-worker duplicate check). Latest run: `docs/evidence/h/`.
