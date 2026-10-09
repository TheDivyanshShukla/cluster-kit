# One-time setup on a Windows PC: installs uv, the pinned Python and all packages.
. "$PSScriptRoot\_common.ps1"

if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Write-Host ">> Installing uv ..."
    powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://astral.sh/uv/install.ps1 | iex"
    $env:Path = "$env:USERPROFILE\.local\bin;$env:Path"
}
Assert-Uv

Write-Host ">> Installing Python $(Get-Content .python-version) + packages with uv ..."
uv sync --frozen
if ($LASTEXITCODE -ne 0) { Write-Host "uv sync failed" -ForegroundColor Red; exit 1 }

$a = Read-Host ">> Disable sleep while plugged in (recommended for the cluster)? [y/N]"
if ($a -match '^[Yy]') {
    powercfg /change standby-timeout-ac 0
    powercfg /change hibernate-timeout-ac 0
    Write-Host "   Sleep disabled on AC power. Undo: Settings > System > Power."
}

uv run python -c "import sys, dask; print(f'>> OK  Python {sys.version.split()[0]}  dask {dask.__version__}')"
Write-Host ">> Done. Head PC: windows\head.bat    Other PCs: windows\worker.bat"
