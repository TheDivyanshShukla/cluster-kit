# Demo on four Ubuntu PCs (built on the Mac)

The Mac is where Watch was built and the footage was analysed. For the demo, one Ubuntu PC is the head (panel, database, video wall) and the other three join as workers. The Mac is not needed on the day.

Tested on the Mac: a packed copy started from another folder rewrote its video paths by itself (all 9 recordings), kept all 241,022 detections, streamed video, served every page and answered a search. Not tested here: the Ubuntu build itself and a real four-PC run, so do a full rehearsal on the Ubuntu PCs the day before.

## 1. On the Mac: fill the SSD

Format the SSD as **exFAT**. Ubuntu cannot write APFS, and reading it needs extra tools.

```
bash scripts/pack_demo.sh /Volumes/<SSD-name>/cluster-kit
```

This copies the committed source code, `vision/watch.json`, `modules.json`, `rules.json`, the model the config uses (`yolo26s.onnx`), the footage (744 MB), a consistent copy of the database, sign-in accounts, filmstrips and search thumbnails. It is about 7 GB. Add `--no-thumbs` for about 1.6 GB, but search results then show no pictures. Build output, backups and the old database copies are left out.

The copied `auth.json` keeps your Mac accounts, so sign in on Ubuntu with the same admin password.

## 2. Head PC (Ubuntu PC 1)

Copy the folder off the SSD; do not run from the SSD (exFAT loses file permissions and is slower for the database).

```
cp -r /media/$USER/<SSD-name>/cluster-kit ~/cluster-kit
cd ~/cluster-kit
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev ffmpeg python3 curl
curl https://sh.rustup.rs -sSf | sh -s -- -y && . ~/.cargo/env
cargo build --release --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw
```

The build needs internet (Rust packages and the ONNX Runtime library, downloaded once). A phone hotspot is enough. It takes several minutes.

Open the ports if the firewall is on (`sudo ufw status`):

```
sudo ufw allow 7710/tcp
sudo ufw allow 7711/udp
```

Start the head, reachable from the other PCs:

```
CW_BIND=0.0.0.0 rust/target/release/cw head
```

Open http://127.0.0.1:7710 and sign in. Note this PC's address (`hostname -I`).

`CW_AUTH=off` (no sign-in) also works, but then the head only listens on its own PC and the workers cannot join. Use it only for a one-PC demo.

## 3. Worker PCs (Ubuntu PCs 2, 3 and 4)

Build once on the head and copy the result, so the workers need no Rust and no internet. From the head PC, put these on the SSD:

```
mkdir -p /media/$USER/<SSD-name>/worker
cp rust/target/release/cw rust/target/release/libonnxruntime*.so* /media/$USER/<SSD-name>/worker/
```

On each worker:

```
sudo apt install -y ffmpeg
cp -r /media/$USER/<SSD-name>/worker ~/cw && cd ~/cw
LD_LIBRARY_PATH=. ./cw join <head-ip> --code <code>
```

Get the pairing code from the head's **Join a PC** page (`/join`). The worker downloads the model and settings from the head and streams recordings from it, so no footage is copied to the workers.

The copied program runs on the same or a newer Ubuntu release. If the PCs run different releases, build on the oldest one. If a worker has the same release and internet, `curl -fsSL http://<head-ip>:7710/api/v1/join/install.sh | sh` also works (it builds from source, so it needs Rust).

## 4. Check before the audience arrives

- `/analytics` lists four computers (the head's own worker plus three).
- The wall (`/`) plays nine cameras with boxes. The replayed cameras do not run the detector (`analyze: false`), so the wall works even with no workers.
- A search on `/find` returns matches with pictures.
- Run the **Benchmark** panel on `/analytics` once with one PC, then all four, to show the speed-up. Ubuntu workers run the detector on the CPU, which is slower per frame than the Mac's Core ML.

Then follow the click path in docs/DEMO.md (start the head about a minute before presenting).

## If something goes wrong

| Symptom | Fix |
| --- | --- |
| `cw: error while loading shared libraries: libonnxruntime` | run from the folder with the `.so` files, with `LD_LIBRARY_PATH=.` |
| `GLIBC_2.xx not found` on a worker | that PC runs an older Ubuntu; build on it or on the oldest PC |
| Worker cannot reach the head | check `ping <head-ip>`, the ufw rules, and that the head was started with `CW_BIND=0.0.0.0` |
| Cameras say "no picture" | `ffmpeg -version` on the head; check `vision/footage/` was copied |
| Search results without pictures | the SSD was packed with `--no-thumbs` |
| Forgot the admin password | stop the head, move `vision/data/auth.json` aside, start again and create the admin with the bootstrap token (docs/RUNNING.md, section 4) |

The PCs do not need internet on the day; they only need one network between them (one switch, router or hotspot).
