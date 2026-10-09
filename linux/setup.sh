#!/usr/bin/env bash
# One-time setup on an Ubuntu PC: installs uv, the pinned Python and all packages.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v uv >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/uv" ]; then
  echo ">> Installing uv ..."
  command -v curl >/dev/null || sudo apt-get install -y curl
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"

echo ">> Installing Python $(cat .python-version) + packages with uv ..."
uv sync --frozen

read -r -p ">> Disable sleep/suspend on this PC (recommended for the cluster)? [y/N] " a
if [[ "$a" =~ ^[Yy]$ ]]; then
  sudo systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target
  echo "   Sleep disabled. Undo later with: sudo systemctl unmask sleep.target suspend.target hibernate.target hybrid-sleep.target"
fi

uv run python -c "import sys, dask; print(f'>> OK  Python {sys.version.split()[0]}  dask {dask.__version__}')"
echo ">> Done. Head PC:  ./linux/head.sh     Other PCs:  ./linux/worker.sh"
