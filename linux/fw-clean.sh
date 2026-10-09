#!/usr/bin/env bash
# Usage: ./linux/fw-clean.sh [options]   (see: uv run cluster.py fw-clean --help)
cd "$(dirname "$0")/.." && export PATH="$HOME/.local/bin:$PATH"
exec uv run cluster.py fw-clean "$@"
