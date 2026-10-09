# Starts the cluster worker. Self-elevates so it can open (and later close) firewall ports.
. "$PSScriptRoot\_common.ps1"
Invoke-Elevated $PSCommandPath $args
Assert-Uv
$host.UI.RawUI.WindowTitle = "pc-cluster worker  (Ctrl+C to stop)"
uv run cluster.py worker @args
