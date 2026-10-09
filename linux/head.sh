#!/usr/bin/env bash
# Usage: ./linux/head.sh [options]   (see: uv run cluster.py head --help)
cd "$(dirname "$0")/.." && export PATH="$HOME/.local/bin:$PATH"
exec uv run cluster.py head "$@"
