#!/usr/bin/env bash
# Usage: ./linux/worker.sh [options]   (see: uv run cluster.py worker --help)
cd "$(dirname "$0")/.." && export PATH="$HOME/.local/bin:$PATH"
exec uv run cluster.py worker "$@"
