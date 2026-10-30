#!/usr/bin/env bash
# Copy everything the demo needs onto a drive, to run it on Linux PCs.
# Usage: bash scripts/pack_demo.sh /Volumes/SSD/cluster-kit [--no-thumbs]
# Source code comes from git (committed files only), so build output, backups and local junk stay behind.
set -euo pipefail
DEST=${1:?usage: pack_demo.sh DEST [--no-thumbs]}
cd "$(dirname "$0")/.."
mkdir -p "$DEST"
git archive HEAD | tar x -C "$DEST"
mkdir -p "$DEST/vision/models" "$DEST/vision/data"
cp vision/watch.json vision/modules.json vision/rules.json "$DEST/vision/"
# Only the models the config refers to.
for m in $(grep -ho 'vision/models/[A-Za-z0-9._-]*\.onnx' vision/modules.json | sort -u); do cp "$m" "$DEST/$m"; done
rsync -a vision/footage "$DEST/vision/"
# Consistent copy of the database while a head may be running.
sqlite3 vision/data/watch.db ".backup '$DEST/vision/data/watch.db'"
cp vision/data/auth.json vision/data/audit.jsonl "$DEST/vision/data/" 2>/dev/null || true
rsync -a vision/data/sprites "$DEST/vision/data/"
[ "${2:-}" = --no-thumbs ] || rsync -a vision/data/thumbs "$DEST/vision/data/"
du -sh "$DEST"
