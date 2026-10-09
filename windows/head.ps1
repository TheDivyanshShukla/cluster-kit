# Starts the cluster head. Self-elevates so it can open (and later close) firewall ports.
. "$PSScriptRoot\_common.ps1"
Invoke-Elevated $PSCommandPath $args
Assert-Uv
$host.UI.RawUI.WindowTitle = "pc-cluster head  (Ctrl+C to stop)"
uv run cluster.py head @args
