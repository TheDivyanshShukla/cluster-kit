#!/usr/bin/env bash
# Update the Desktop installation and start an open-mode worker with LAN discovery.
set -euo pipefail

DIR="$HOME/Desktop/cluster-kit"
if [ ! -d "$DIR" ]; then
  echo "Cluster Kit folder not found: $DIR" >&2
  exit 1
fi
cd "$DIR"
export PATH="$HOME/.cargo/bin:$PATH"
export CK_FLEET=open CK_AUTH=off

echo "Updating Cluster Kit..."
git pull --ff-only
echo "Building the worker..."
cargo build --release --locked --manifest-path rust/Cargo.toml
if [ ! -x ./rust/target/release/ck ]; then
  echo "ck is not built. Run: sudo CK_FLEET=open CK_AUTH=off bash scripts/provision.sh" >&2
  exit 1
fi

echo "Starting worker; looking for the head on the LAN..."
exec ./rust/target/release/ck worker "$@"
