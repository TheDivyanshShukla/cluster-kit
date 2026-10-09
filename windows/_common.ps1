# Shared helpers, dot-sourced by the other scripts.
$ErrorActionPreference = 'Continue'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$env:Path = "$env:USERPROFILE\.local\bin;$env:Path"

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Re-launch the calling script as Administrator (needed to open/close firewall ports).
function Invoke-Elevated([string]$ScriptPath, [object[]]$ScriptArgs) {
    if (Test-Admin) { return }
    Write-Host ">> Asking for Administrator rights (needed for firewall rules)..."
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-NoExit', '-File', "`"$ScriptPath`"") + $ScriptArgs
    Start-Process powershell -Verb RunAs -ArgumentList $argList
    exit
}

function Assert-Uv {
    if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
        Write-Host "uv not found. Run windows\setup.bat first (as the same user)." -ForegroundColor Red
        exit 1
    }
}
