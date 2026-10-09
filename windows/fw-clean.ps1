. "$PSScriptRoot\_common.ps1"
Invoke-Elevated $PSCommandPath $args
Assert-Uv
uv run cluster.py fw-clean @args
