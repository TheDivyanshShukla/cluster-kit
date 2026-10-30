# Running Watch

Watch is the `cw` program in `rust/`: a head that serves the web panel and keeps the database, plus workers that decode video and run the detector. One PC runs both. More PCs can join. The commands below were run on macOS (Apple silicon); Linux and Windows differences are noted.

## 1. Prerequisites

| Need | Why | Check |
| --- | --- | --- |
| Rust (stable) | builds `cw` | `cargo --version` (install: https://rustup.rs) |
| FFmpeg and ffprobe on PATH | decoding, thumbnails, export | `ffmpeg -version` |
| Internet on the first build | downloads the ONNX Runtime library once | |
| A detector model in `vision/models/` | the object detector | see section 3 |

macOS: `brew install ffmpeg`. Ubuntu: `sudo apt install ffmpeg`. Windows: install the Visual Studio C++ build tools, then `winget install ffmpeg`.

Python is not needed for Watch (only for the older Dask engine and for exporting a model).

## 2. Build

From the repository root.

macOS on Apple silicon (uses the Neural Engine / GPU through Core ML, about 10 to 20 times faster than CPU for YOLO models):

```
cargo build --release --manifest-path rust/Cargo.toml --no-default-features --features watch,coreml --bin cw
```

Linux, Windows, or Intel Mac (CPU):

```
cargo build --release --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw
```

The binary is `rust/target/release/cw` (`cw.exe` on Windows). The first build takes several minutes.

Shortcut that also installs Rust and the other engines: `./kit setup rs` then `./kit rs build` (Windows: `kit setup`, `kit rs build`). Note that `kit` builds the CPU version.

## 3. Models and sample footage

The default `vision/modules.json` points at `vision/models/yolo26s.onnx`. Get a model one of two ways.

Option A, download the small starter model and two sample clips (about 10 MB):

```
rust/target/release/cw fetch
```

This puts `yolov5n.onnx` in `vision/models/` and two clips in `vision/footage/`. Then open Setup in the panel and change the module's model path to `vision/models/yolov5n.onnx` (or edit `vision/modules.json`).

Option B, export a newer model (YOLO26s was the best speed and accuracy on the test footage; see docs/MODELS.md for measurements and licences, YOLO is AGPL-3.0):

```
uv venv .venv-model && uv pip install -p .venv-model ultralytics onnx onnxruntime onnxslim
.venv-model/bin/python -c "from ultralytics import YOLO; YOLO('yolo26s.pt').export(format='onnx', imgsz=640, opset=17, simplify=True)"
mv yolo26s.onnx vision/models/
```

Footage is not stored in git. Put any `.mp4` into `vision/footage/` (or point a source at any path).

## 4. Start the head

```
rust/target/release/cw head
```

Open http://127.0.0.1:7710. The head starts a local worker automatically, so one PC is enough.

Useful variants:

| Command | What it does |
| --- | --- |
| `CW_AUTH=off rust/target/release/cw head` | development: no sign-in, reachable only from this PC |
| `rust/target/release/cw head --port 7800` | another port |
| `rust/target/release/cw head --no-local-worker` | head only; workers on other PCs do the work |
| `CW_BIND=0.0.0.0 rust/target/release/cw head` | reachable from other PCs on the network (needed for them to join) |
| `CW_ROOT=/tmp/watch-test rust/target/release/cw head` | keep config and data in another folder (needs `vision/` inside it, copy `modules.json` and `rules.json`, symlink `models` and `footage`) |

Sign-in: with no users configured the head runs in open mode and listens on loopback only. The first start prints a bootstrap token. Create the first administrator:

```
curl -X POST http://127.0.0.1:7710/api/v1/auth/bootstrap -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"choose-12-or-more-chars","bootstrap_token":"<token from the log>"}'
```

After that, pages sign in at `/login` (change the password there), and `CW_AUTH=off` is the way to skip it in development.

## 5. Add cameras and recordings

Use the Sources page, or edit `vision/watch.json` (see `vision/watch.example.json`):

```json
{ "configured": true,
  "cameras": [
    { "id": "lobby", "name": "Lobby", "url": "vision/footage/lobby.mp4", "kind": "archive" },
    { "id": "gate",  "name": "Gate",  "url": "rtsp://192.168.1.20:554/stream", "kind": "live" },
    { "id": "demo",  "name": "Demo",  "url": "vision/footage/demo.mp4", "kind": "file", "start": 0.85, "analyze": false }
  ] }
```

| kind | meaning |
| --- | --- |
| `archive` | index a recording once, then it is finished |
| `live` | an RTSP or HTTP stream, analysed continuously |
| `webcam` | a capture device on a worker; the location is its number (`0`) |
| `file` | a recording replayed in a loop at real time, like a live camera |
| `image` | a still image or a folder of images |

`start` is the position (0 to 1) a replayed file starts at. `analyze: false` replays a recording that was already analysed without running the detector again; the wall draws the stored detections.

## 6. Add more PCs

On the head PC start with `CW_BIND=0.0.0.0` (and sign-in on if the network is shared). Then on each other PC either:

```
curl -fsSL http://<head-ip>:7710/api/v1/join/install.sh | sh        # macOS or Linux
cw join                                                              # finds the head by itself, or: cw join <head-ip> --code <code>
rust/target/release/cw worker --head <head-ip>                       # older way, no pairing
cw deploy user@host --head <head-ip>                                 # push to a machine you can ssh into (add --dry-run to preview)
```

The `/join` page on the head shows the command and a pairing code. Ports: 7710 TCP (panel and API), 7711 UDP (discovery). Full multi-PC steps: docs/CLUSTER_DEMO.md. Sharded search (each PC keeps its own database) is off by default: `CW_SHARDS=1` on the head and every worker (docs/DISTRIBUTED.md).

## 7. Tests and checks

```
cargo test --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw          # unit tests
bash scripts/watch_acceptance.sh                                                                     # end-to-end on an isolated data folder
bash scripts/cluster_segments_check.sh                                                               # two workers split a recording
bash scripts/cluster_bench.sh                                                                        # benchmark with 1..N local workers
bash scripts/shard_chaos.sh                                                                          # head + 3 workers, kill one mid-query
```

The acceptance and cluster scripts start their own heads on spare ports with their own data folders; they never touch a head you are running or `vision/data`.

## 8. Where things are

| Path | What |
| --- | --- |
| `vision/watch.json` | sources and processing settings |
| `vision/modules.json`, `vision/rules.json` | detector modules, alert rules |
| `vision/data/watch.db` | the database (SQLite, WAL mode) |
| `vision/data/thumbs`, `sprites`, `exports` | thumbnails, filmstrips, rendered clips |
| `vision/data/auth.json` | users and worker tokens (hashed), keep private |

Back up the database while the head runs with `sqlite3 vision/data/watch.db ".backup backup.db"`. Old-format journals are not imported unless you start with `CW_IMPORT_LEGACY=1`.

## 9. Environment variables

| Variable | Effect |
| --- | --- |
| `CW_AUTH=off` | no sign-in (head stays on loopback) |
| `CW_AUTH=required` | always require sign-in |
| `CW_BIND=<ip>` | address the head listens on (`0.0.0.0` for the network) |
| `CW_ROOT=<dir>` | use this folder as the project root |
| `CW_THREADS=<n>` | detector threads per worker |
| `CW_WORKER_TOKEN=<t>` | credential a worker sends to an authenticated head |
| `CW_DEVICES="0,Name"` | list capture devices by hand (otherwise found automatically) |
| `CW_MEDIA=remote` | worker reads recordings from the head over HTTP |
| `CW_SHARDS=1` | sharded storage and federated search |
| `CW_IMPORT_LEGACY=1` | import old `seen.jsonl` journals at start |
| `CW_BOOTSTRAP_TOKEN` | fixed bootstrap token (16+ characters) instead of a random one |
| `CW_COOKIE_SECURE=1` | mark the session cookie Secure (behind HTTPS) |

## 10. Troubleshooting

| Symptom | Fix |
| --- | --- |
| "Model file not found on this PC" | do section 3, or change the module's model path in Setup |
| Pages show but cameras say "no picture" | check `ffmpeg -version`; check the source path exists |
| Other PCs cannot reach the head | start the head with `CW_BIND=0.0.0.0`; open TCP 7710 and UDP 7711 in the firewall |
| A search is slow | scope it to one or two cameras; searches over everything stop at 100,000 rows |
| Port already in use | `cw head --port <other>` |
| Build fails on Windows | install the Visual Studio C++ build tools |
| Want a clean start | stop the head, move `vision/data/` aside, start again (sources stay in `vision/watch.json`) |
