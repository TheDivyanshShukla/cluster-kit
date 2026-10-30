#!/usr/bin/env bash
# Ubuntu worker PC: copy the worker kit (made by ubuntu_head.sh) off the drive and join the head.
# Usage: bash /media/$USER/DEV/ubuntu_worker.sh <head-ip> <pairing-code>
set -euo pipefail
HEAD=${1:?usage: ubuntu_worker.sh <head-ip> <pairing-code>}
CODE=${2:?pairing code from http://<head-ip>:7710/join}
DRIVE=$(cd "$(dirname "$0")" && pwd)
[ -x "$DRIVE/worker/cw" ] || [ -f "$DRIVE/worker/cw" ] || { echo "No worker kit on the drive: run ubuntu_head.sh on the head PC first."; exit 1; }
command -v ffmpeg >/dev/null || sudo apt install -y ffmpeg
mkdir -p ~/cw && cp "$DRIVE"/worker/* ~/cw/ && chmod +x ~/cw/cw
cd ~/cw
LD_LIBRARY_PATH=. ./cw join "$HEAD" --code "$CODE"
