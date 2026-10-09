#!/usr/bin/env bash
# Usage: ./linux/status.sh [options]   (see: uv run cluster.py status --help)
cd "$(dirname "$0")/.." && export PATH="$HOME/.local/bin:$PATH"
exec uv run cluster.py status "$@"
