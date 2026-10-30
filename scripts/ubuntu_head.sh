#!/usr/bin/env bash
# Ubuntu head PC: copy the demo off the drive, build cw (needs internet once), put a worker kit on the drive, start the head.
# Usage: bash /media/$USER/DEV/ubuntu_head.sh
set -euo pipefail
DRIVE=$(cd "$(dirname "$0")" && pwd)
HOME_DIR=~/cluster-kit
if [ ! -d "$HOME_DIR" ]; then cp -r "$DRIVE/cluster-kit" "$HOME_DIR"; fi
cd "$HOME_DIR"
if [ ! -x rust/target/release/cw ]; then
  sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev ffmpeg python3 curl
  command -v cargo >/dev/null || { curl https://sh.rustup.rs -sSf | sh -s -- -y; }
  . ~/.cargo/env 2>/dev/null || true
  cargo build --release --manifest-path rust/Cargo.toml --no-default-features --features watch --bin cw
fi
# Worker kit: the program plus the ONNX Runtime library it loads.
mkdir -p "$DRIVE/worker"
cp rust/target/release/cw "$DRIVE/worker/"
find rust/target/release -maxdepth 1 -name 'libonnxruntime*.so*' -exec cp {} "$DRIVE/worker/" \;
command -v ufw >/dev/null && sudo ufw status | grep -q active && { sudo ufw allow 7710/tcp; sudo ufw allow 7711/udp; }
echo "Head address: $(hostname -I | awk '{print $1}')   panel: http://127.0.0.1:7710   pairing code: /join"
CW_BIND=0.0.0.0 rust/target/release/cw head
