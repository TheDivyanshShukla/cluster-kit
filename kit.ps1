# Cluster Kit launcher (Windows). Usually run through kit.bat:  kit help
$ErrorActionPreference = 'Continue'
$Root = $PSScriptRoot
Set-Location $Root
$env:Path = "$env:USERPROFILE\.local\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
$CK = Join-Path $Root 'rust\target\release\ck.exe'

$engine = if ($args.Count -gt 0) { $args[0] } else { 'help' }
$cmd = if ($args.Count -gt 1) { $args[1] } else { '' }
$rest = if ($args.Count -gt 2) { $args[2..($args.Count - 1)] } else { @() }

function Show-Usage {
    @'
Cluster Kit

  kit setup [all|py|rs]      one-time install on this PC (default: all)

  Python engine (Dask, dashboard :8787)
  kit py head                master PC
  kit py worker              every other PC (auto-discovers the head)
  kit py status              who is connected
  kit py bench               1 -> N PC scaling benchmark (CSV + chart)
  kit py run examples\dask_sha256.py --n 1e7    run a Dask job script (see examples\)

  Rust engine (raw TCP, dashboard :7702)
  kit rs head [--gpu]        master PC (--gpu: SHA jobs on the GPU)
  kit rs worker [--gpu]      every other PC
  kit rs run --n 1e9 --sha 00000          built-in SHA-256 prefix search
  kit rs run --n 1e8 --cmd examples\primes.exe  any language: prog START END per chunk (see examples\)
  kit rs ping --head IP                   round-trip latency
  kit rs build               rebuild after changing rust\

  kit fw-clean               remove leftover firewall rules (both engines)
  kit tune [on|off]          low-latency network tuning (interrupt moderation off, High performance power plan)

Options go after the command, e.g. kit py worker --head 192.168.1.10 --procs 4
'@
}

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Firewall rules need Administrator: re-launch this script elevated in a new window.
function Invoke-Elevated {
    if (Test-Admin) { return }
    Write-Host ">> Asking for Administrator rights (needed for firewall rules)..."
    $quoted = $args | ForEach-Object { "`"$_`"" }
    Start-Process powershell -Verb RunAs -ArgumentList (@('-NoProfile', '-ExecutionPolicy', 'Bypass', '-NoExit', '-File', "`"$PSCommandPath`"") + $quoted)
    exit
}

function Need-Uv { if (-not (Get-Command uv -ErrorAction SilentlyContinue)) { Write-Host "uv missing. Run: kit setup" -ForegroundColor Red; exit 1 } }
function Need-Ck { if (-not (Test-Path $CK)) { Write-Host "ck not built. Run: kit setup rs" -ForegroundColor Red; exit 1 } }
function Py { uv --project "$Root\python" run @args; exit $LASTEXITCODE }  # runs from the repo root

function Setup-Py {
    if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
        Write-Host ">> Installing uv ..."
        powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://astral.sh/uv/install.ps1 | iex"
    }
    Write-Host ">> Installing Python $(Get-Content python\.python-version) + packages ..."
    Push-Location python; uv sync --frozen; $ok = $LASTEXITCODE -eq 0; Pop-Location
    if (-not $ok) { Write-Host "uv sync failed" -ForegroundColor Red; exit 1 }
    uv --project "$Root\python" run python -c "import sys, dask; print(f'>> OK  Python {sys.version.split()[0]}  dask {dask.__version__}')"
}

function Setup-Rs {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host ">> Installing Rust ..."
        $init = Join-Path $env:TEMP 'rustup-init.exe'
        Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $init
        & $init -y --profile minimal
    }
    Write-Host ">> Building ck (release) ..."
    cargo build --release --manifest-path rust\Cargo.toml
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Build failed. Rust on Windows needs the MSVC C++ build tools:" -ForegroundColor Red
        Write-Host '  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
        Write-Host "then run: kit setup rs"
        exit 1
    }
    Write-Host ">> OK  $CK"
}

switch ($engine) {
    'setup' {
        switch ($(if ($cmd) { $cmd } else { 'all' })) {
            'py' { Setup-Py }
            { $_ -in 'rs', 'all' } { Setup-Py; Setup-Rs }  # rs uses the Python env for firewall handling
            default { Show-Usage; exit 1 }
        }
        $a = Read-Host ">> Disable sleep while plugged in (recommended for the cluster)? [y/N]"
        if ($a -match '^[Yy]') {
            powercfg /change standby-timeout-ac 0
            powercfg /change hibernate-timeout-ac 0
            Write-Host "   Undo: Settings > System > Power."
        }
        Write-Host ">> Done. Head PC: kit py head  (or rs)   Other PCs: kit py worker  (or rs)"
    }
    'py' {
        Need-Uv
        switch ($cmd) {
            { $_ -in 'head', 'worker', 'fw-clean' } { Invoke-Elevated @args; $host.UI.RawUI.WindowTitle = "kit py $cmd  (Ctrl+C to stop)"; Py python\cluster.py $cmd @rest }
            { $_ -in 'status', 'bench' } { Py python\cluster.py $cmd @rest }
            'run' { Py @rest }
            default { Show-Usage; exit 1 }
        }
    }
    'rs' {
        switch ($cmd) {
            'build' { cargo build --release --manifest-path rust\Cargo.toml }
            'head' { Need-Uv; Need-Ck; Invoke-Elevated @args; $host.UI.RawUI.WindowTitle = 'kit rs head  (Ctrl+C to stop)'; Py python\cluster.py fwrun --role ck-head --tcp 7700,7702 -- $CK head @rest }
            'worker' { Need-Uv; Need-Ck; Invoke-Elevated @args; $host.UI.RawUI.WindowTitle = 'kit rs worker  (Ctrl+C to stop)'; Py python\cluster.py fwrun --role ck-worker --udp 7701 -- $CK worker @rest }
            { $_ -in 'run', 'ping' } { Need-Ck; & $CK $cmd @rest; exit $LASTEXITCODE }
            default { Show-Usage; exit 1 }
        }
    }
    'fw-clean' { Need-Uv; Invoke-Elevated @args; Py python\cluster.py fw-clean }
    'tune' {
        Invoke-Elevated @args
        $on = $cmd -ne 'off'
        Write-Host ">> The network adapter restarts briefly while this applies."
        Get-NetAdapter -Physical | Where-Object Status -eq 'Up' | ForEach-Object {
            Set-NetAdapterAdvancedProperty -Name $_.Name -RegistryKeyword '*InterruptModeration' -RegistryValue $(if ($on) { 0 } else { 1 }) -ErrorAction SilentlyContinue
            Write-Host "   $($_.Name): interrupt moderation $(if ($on) { 'off' } else { 'on' })"
        }
        powercfg /setactive $(if ($on) { 'SCHEME_MIN' } else { 'SCHEME_BALANCED' })  # High performance / Balanced
        Write-Host ">> Done. Undo: kit tune off"
    }
    default { Show-Usage }
}
