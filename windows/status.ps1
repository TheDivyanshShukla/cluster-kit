. "$PSScriptRoot\_common.ps1"

Assert-Uv
uv run cluster.py status @args
