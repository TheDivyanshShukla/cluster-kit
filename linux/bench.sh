#!/usr/bin/env bash
# Usage: ./linux/bench.sh [options]   (see: uv run cluster.py bench --help)
cd "$(dirname "$0")/.." && export PATH="$HOME/.local/bin:$PATH"
exec uv run cluster.py bench "$@"
