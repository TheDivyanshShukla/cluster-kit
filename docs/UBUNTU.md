# Adding an Ubuntu PC

A program built on the Mac will not run on Ubuntu (different operating system, often a different CPU), so Ubuntu builds its own copy from the source files. You do not copy any built files.

## What to transfer

| You want to run | Put on the Ubuntu PC | Notes |
| --- | --- | --- |
| Everything (simplest) | the whole repository | `git clone`, see below |
| Rust SHA-256 engine (`kit rs worker`) | `kit`, `rust/` (without `target*`), `python/` and `examples/` if you run the original `simple_sha256.py` | the worker runs `examples/simple_sha256.py` from its own folder |
| Rust engine with the head sending the script | `kit` and `rust/` only | use `ck run --push examples/sha256_chunk.py`; the head sends the script, so no `examples/` or `python/` |
| Dask (Python) engine (`kit py worker`) | `kit`, `python/` | `kit setup py` installs uv and Python 3.12 |

Never copy: `rust/target*` (build output), `python/.venv`, `.git`, `.claude`, or any `*.bak*` files.

## Option 1: clone (recommended)

On the Ubuntu PC:

```
sudo apt update
sudo apt install -y git curl build-essential python3
git clone https://github.com/TheDivyanshShukla/cluster-kit.git    # add -b <branch> for work not merged into main yet
cd cluster-kit
```

## Option 2: copy over the network (no GitHub)

From the Mac, inside `cluster-kit/` (copies only the kit's own folders, whatever else lies around):

```
rsync -av --exclude 'target*' --exclude '.venv' --exclude '*.bak*' kit rust python examples user@ubuntu-ip:~/cluster-kit/
```

Smallest copy for the SHA-256 engine only (the head sends the script with `--push`):

```
cd cluster-kit
tar czf - kit rust/Cargo.toml rust/Cargo.lock rust/src | ssh user@ubuntu-ip 'mkdir -p ~/cluster-kit && tar xzf - -C ~/cluster-kit'
```

Then on Ubuntu install the packages from option 1 (without `git`).

## Build on Ubuntu

```
./kit setup rs          # installs Rust if missing, then builds ck; a few minutes
./kit setup py          # only if you will use the Dask engine
```

To build without GPU support:

```
cargo build --release --manifest-path rust/Cargo.toml --no-default-features
```

## Option 3: SSD, Ubuntu with no internet

Tested on the Mac with `cargo build --offline`: the SHA engine builds from a vendored copy in 7 seconds and a pushed job gives the right answer. The Ubuntu PC still needs a Rust compiler (`rustc`, `cargo`, `build-essential`) and `python3` installed; put the `.deb` files or the rustup offline installer on the SSD if it has no internet at all.

On the Mac, make a trimmed SHA-engine-only crate with its dependencies inside it (about 290 MB, 21 MB compressed):

```
D=$(mktemp -d) && mkdir -p $D/src $D/.cargo
cat > $D/Cargo.toml <<'EOF'
[package]
name = "ck"
version = "0.1.0"
edition = "2021"
[dependencies]
rayon = "1"
sha2 = { version = "0.10", features = ["compress"] }
sysinfo = "0.30"
[[bin]]
name = "ck"
path = "src/main.rs"
EOF
cp rust/src/main.rs rust/src/dash.html rust/src/sha.wgsl $D/src/ && cp rust/Cargo.lock $D/
cp examples/sha256_chunk.py $D/
(cd $D && cargo vendor vendor && printf '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n' > .cargo/config.toml)
tar czf /Volumes/SSD/ck-offline.tgz -C $D .
```

(Match the dependency versions to `rust/Cargo.toml`; if a feature or file is missing the build says which.) On Ubuntu:

```
mkdir ck && tar xzf /media/$USER/SSD/ck-offline.tgz -C ck && cd ck
cargo build --release --offline
```

For the whole repository instead, copy the clone from option 1 to the SSD with `git bundle create /Volumes/SSD/cluster-kit.bundle --all` and `git clone cluster-kit.bundle` on Ubuntu; building it still needs internet (or `cargo vendor` run on the Mac first).

## Mac and Ubuntu on different networks

An SSD moves files only. Running as one cluster needs both PCs on the same network. Without a router both can share:

- Ethernet cable between them, static addresses. Mac: System Settings, Network, Ethernet, Manual, `192.168.50.1`, mask `255.255.255.0`. Ubuntu: `sudo ip addr add 192.168.50.2/24 dev <eth-name>` (find it with `ip link`). Then use `192.168.50.1` as `<mac-ip>` below.
- Or the same phone hotspot on both. Some hotspots isolate clients; if `ping` fails, use the cable.

Until then each PC runs on its own: the Mac head with its own local worker, and Ubuntu as a separate head (`ck head`). Results are not combined.

## Open the ports

Only needed if Ubuntu's firewall is on (`sudo ufw status`). Replace the subnet with yours.

```
sudo ufw allow from 192.168.0.0/16 to any port 7700 proto tcp    # SHA engine head and workers
sudo ufw allow from 192.168.0.0/16 to any port 7701 proto udp    # SHA engine discovery
sudo ufw allow from 192.168.0.0/16 to any port 7702 proto tcp    # SHA engine dashboard
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

## Check it works

`rust/target/release/ck ping --head <mac-ip>` prints the round-trip time; the head's dashboard (http://<mac-ip>:7702) lists the Ubuntu worker.

## Mixed machines: things to know

- The head and workers can differ in operating system and CPU. Each builds its own binary.
- An Ubuntu worker running the original `simple_sha256.py` needs `uv` (installed by `./kit setup py`) because the Rust engine runs it with `uv run`; with `--push` it needs only `python3`.
