# Adding an Ubuntu PC

A program built on the Mac will not run on Ubuntu (different operating system, often a different CPU), so Ubuntu builds its own copy from the source files. You do not copy any built files.

## What to transfer

| You want to run | Put on the Ubuntu PC | Notes |
| --- | --- | --- |
| Everything (simplest) | the whole repository | `git clone`, see below |
| Rust SHA-256 engine (`kit rs worker`) | `kit`, `rust/` (without `target*`), `python/` and `examples/` if you run the original `simple_sha256.py` | the worker runs `examples/simple_sha256.py` from its own folder |
| Rust engine with the head sending the script | `kit` and `rust/` only | use `ck run --push examples/sha256_chunk.py`; the head sends the script, so no `examples/` or `python/` |
| Dask (Python) engine (`kit py worker`) | `kit`, `python/` | `kit setup py` installs uv and Python 3.12 |
| Watch video worker | nothing | one command on Ubuntu downloads everything from the head |

Never copy: `rust/target*` (build output), `vision/data` (database), `vision/footage` and `vision/models` (large; Watch workers download the model from the head and stream recordings from it), `.git`, `.claude`, or any `*.bak*` files.

## Option 1: clone (recommended)

On the Ubuntu PC:

```
sudo apt update
sudo apt install -y git curl build-essential pkg-config libssl-dev ffmpeg python3
git clone -b feature/watch-vision-platform https://github.com/TheDivyanshShukla/cluster-kit.git
cd cluster-kit
```

## Option 2: copy over the network (no GitHub)

From the Mac, in the folder that contains `cluster-kit/`:

```
rsync -av --exclude '.git' --exclude '.claude' --exclude 'rust/target*' --exclude 'vision/data' \
  --exclude 'vision/footage' --exclude 'vision/models' --exclude '*.bak*' --exclude '*.mp4' \
  cluster-kit/ user@ubuntu-ip:~/cluster-kit/
```

Smallest copy for the SHA-256 engine only (the head sends the script with `--push`):

```
cd cluster-kit
tar czf - kit rust/Cargo.toml rust/Cargo.lock rust/src | ssh user@ubuntu-ip 'mkdir -p ~/cluster-kit && tar xzf - -C ~/cluster-kit'
```

Then on Ubuntu install the packages from option 1 (without `git`).

## Build on Ubuntu

```
./kit setup rs          # installs Rust if missing, then builds ck (SHA engine) and cw (video system); several minutes
./kit setup py          # only if you will use the Dask engine
```

If you only need the SHA engine and want to skip the video system and GPU libraries:

```
cargo build --release --manifest-path rust/Cargo.toml --bin ck --no-default-features
```

## Open the ports

Only needed if Ubuntu's firewall is on (`sudo ufw status`). Replace the subnet with yours.

```
sudo ufw allow from 192.168.0.0/16 to any port 7700 proto tcp    # SHA engine head and workers
sudo ufw allow from 192.168.0.0/16 to any port 7701 proto udp    # SHA engine discovery
sudo ufw allow from 192.168.0.0/16 to any port 7710 proto tcp    # Watch panel and API
sudo ufw allow from 192.168.0.0/16 to any port 7711 proto udp    # Watch discovery
```

(`kit` also opens and closes its own ports while it runs.)

## Run the SHA-256 example across the Mac and Ubuntu

1. Mac (head): `./kit rs head`
2. Ubuntu: `./kit rs worker` (it finds the head by itself; if not, `rust/target/release/ck worker --head <mac-ip>`)
3. Mac: `python examples/simple_sha256.py --cluster --n 5e8` and read the line `Ran on: cluster, Rust engine (2 workers)`.

Ubuntu must have the kit folder with `python/` and `examples/` for step 3 (the worker runs the script from its own folder). To skip that, let the head send a stand-alone script:

```
rust/target/release/ck run --n 5e8 --from 1 --chunk 1e6 --push examples/sha256_chunk.py | awk '/^[0-9]+$/ {s+=$1} END {print s}'
```

The Ubuntu PC then needs only the built `ck` and `python3`.

## Run the Watch video system on Ubuntu

On the Mac start the head so other PCs can reach it: `CW_BIND=0.0.0.0 rust/target/release/cw head`.

On Ubuntu, with the apt packages above (including `ffmpeg`), either:

```
curl -fsSL http://<mac-ip>:7710/api/v1/join/install.sh | sh
```

or, from a cloned repository after `./kit setup rs`:

```
rust/target/release/cw join <mac-ip>          # add --code <code> if the head has sign-in on
```

The worker downloads the detector model from the head and streams recordings from it, so no video files or shared folder are needed. See docs/CLUSTER_DEMO.md and docs/RUNNING.md.

## Check it works

- SHA engine: `rust/target/release/ck ping --head <mac-ip>` prints the round-trip time; the head's dashboard (http://<mac-ip>:7702) lists the Ubuntu worker.
- Watch: open http://<mac-ip>:7710/analytics; the Ubuntu PC appears under the joined computers.

## Mixed machines: things to know

- The head and workers can differ in operating system and CPU. Each builds its own binary.
- Ubuntu workers run the detector on the CPU (the Core ML speed-up is Apple only), so they are slower per frame than the Mac.
- An Ubuntu worker running the original `simple_sha256.py` needs `uv` (installed by `./kit setup py`) because the Rust engine runs it with `uv run`; with `--push` it needs only `python3`.
