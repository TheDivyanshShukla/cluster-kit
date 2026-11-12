# Provision one Windows lab PC for Cluster Kit. Safe to run again: it checks every item and fixes only what is wrong.
# Needs an ADMINISTRATOR PowerShell. The ESP32 stick in hid/ opens one and types:
#   iex (irm https://raw.githubusercontent.com/TheDivyanshShukla/cluster-kit/main/scripts/provision.ps1)
# Settings (environment variables): CK_HEAD_IP   pin the head (default: workers find it by broadcast)
#   CK_BRANCH (main)   CK_ADMIN_KEY (an ssh public key line to allow in)   CK_ROLE=head (also run the head here)
#   CK_AUTH (off = open mode: no login, no token)   CK_FLEET (open)
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'  # downloads are much faster without the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
function Opt($v, $d) { if ($v) { $v } else { $d } }
$Repo = 'TheDivyanshShukla/cluster-kit'
$Branch = Opt $env:CK_BRANCH 'main'
$Fleet = Opt $env:CK_FLEET 'open'
$Auth = Opt $env:CK_AUTH 'off'
$HeadArg = if ($env:CK_HEAD_IP) { " --head $($env:CK_HEAD_IP)" } else { '' }
$Dir = Join-Path ([Environment]::GetFolderPath('Desktop')) 'cluster-kit'
$Bin = Join-Path $Dir 'rust\target\release\ck.exe'
$Stamp = Join-Path $Dir 'rust\target\release\.ck-built'
$LogDir = Join-Path $env:ProgramData 'cluster-kit'
$Failed = 0
$Problems = @()
function Step($t) { Write-Host "`n== $t" }
function Ok($t) { Write-Host "  ok      $t" -ForegroundColor Green }
function Fixed($t) { Write-Host "  fixed   $t" -ForegroundColor Yellow }
function Bad($t) { Write-Host "  FAIL    $t" -ForegroundColor Red; $script:Failed++; $script:Problems += $t }
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
Start-Transcript -Path (Join-Path $LogDir 'provision.log') -Append | Out-Null

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Write-Host "Cluster Kit provisioning $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') on $env:COMPUTERNAME as $env:USERNAME"
if (-not $admin) { Bad 'not an administrator: open PowerShell with Run as administrator and run this again'; Stop-Transcript | Out-Null; return }

Step 'network'
function Test-Online { try { $c = New-Object Net.Sockets.TcpClient; $r = $c.BeginConnect('github.com', 443, $null, $null); $up = $r.AsyncWaitHandle.WaitOne(8000) -and $c.Connected; $c.Close(); $up } catch { $false } }
$online = $false
foreach ($i in 1..3) { if (Test-Online) { $online = $true; break }; Start-Sleep 3 }
if ($online) { Ok 'github.com reachable' }
else {
  $w32 = (w32tm /query /status 2>&1 | Out-String)  # a wrong clock also breaks https
  w32tm /resync 2>&1 | Out-Null
  if (Test-Online) { $online = $true; Fixed 'clock resynchronised; github.com reachable' } else { Bad 'github.com unreachable: downloads skipped, the PC keeps what it already has' }
}

Step 'no sleep, no screen lock, no timeouts'
foreach ($a in 'standby-timeout-ac', 'standby-timeout-dc', 'monitor-timeout-ac', 'monitor-timeout-dc', 'hibernate-timeout-ac', 'hibernate-timeout-dc', 'disk-timeout-ac', 'disk-timeout-dc') { powercfg /change $a 0 | Out-Null }
powercfg /hibernate off 2>&1 | Out-Null
$pers = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization'
New-Item -Path $pers -Force | Out-Null; Set-ItemProperty -Path $pers -Name NoLockScreen -Value 1 -Type DWord
$au = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU'  # no surprise reboot in the middle of a job
New-Item -Path $au -Force | Out-Null; Set-ItemProperty -Path $au -Name NoAutoRebootWithLoggedOnUsers -Value 1 -Type DWord
Set-ItemProperty -Path 'HKCU:\Control Panel\Desktop' -Name ScreenSaveActive -Value 0
$q = (powercfg /query SCHEME_CURRENT SUB_SLEEP STANDBYIDLE 2>&1 | Out-String)
if ($q -match 'Current AC Power Setting Index: 0x00000000') { Ok 'sleep, screen blanking and lock screen are off' } else { Bad 'could not confirm that sleep is off' }

Step 'git'
$env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
function Have-Git { [bool](Get-Command git -ErrorAction SilentlyContinue) }
if (-not (Have-Git)) {
  if (Get-Command winget -ErrorAction SilentlyContinue) {
    Write-Host '   installing Git with winget (1 to 3 minutes)'
    winget install --id Git.Git -e --silent --accept-package-agreements --accept-source-agreements *>> (Join-Path $LogDir 'git.log')
    $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
  }
  if (-not (Have-Git) -and $online) {
    # no winget (or it failed): take the Git for Windows installer straight from its GitHub release
    try {
      $rel = Invoke-RestMethod 'https://api.github.com/repos/git-for-windows/git/releases/latest' -UseBasicParsing
      $asset = $rel.assets | Where-Object { $_.name -match '^Git-.*-64-bit\.exe$' } | Select-Object -First 1
      $exe = Join-Path $env:TEMP $asset.name
      Invoke-WebRequest $asset.browser_download_url -OutFile $exe -UseBasicParsing
      Start-Process $exe -ArgumentList '/VERYSILENT', '/NORESTART', '/NOCANCEL', '/SP-' -Wait
      $env:Path = "$env:ProgramFiles\Git\cmd;$env:Path"
    } catch { Write-Host "   git installer download failed: $($_.Exception.Message)" }
  }
}
if (Have-Git) { Ok ((git --version) | Out-String).Trim() }
else { Bad 'git is not installed and could not be installed. Install it by hand from https://git-scm.com/download/win (or run: winget install Git.Git), then run this script again' }

Step 'cluster-kit in the Desktop folder'
$zipHash = ''
if ($online) {
  $zip = Join-Path $env:TEMP 'ck-main.zip'; $tmp = Join-Path $env:TEMP 'ck-main'
  try {
    Invoke-WebRequest "https://github.com/$Repo/archive/refs/heads/$Branch.zip" -OutFile $zip -UseBasicParsing
    $zipHash = (Get-FileHash $zip -Algorithm SHA256).Hash.Substring(0, 10).ToLower()
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $src = Get-ChildItem $tmp | Select-Object -First 1
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    robocopy $src.FullName $Dir /E /XD target ck-data /NFL /NDL /NJH /NJS /NP | Out-Null  # keep the built binary and local data
    if ($LASTEXITCODE -lt 8) { Ok "folder is on the latest $Branch ($zipHash)" } else { Bad "copy to $Dir failed" }
    $global:LASTEXITCODE = 0
  } catch { Bad "download failed: $($_.Exception.Message)" }
} else { Bad 'offline: using the folder as it is' }

Step 'Rust and the build tools'
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
function Have-Cargo { [bool](Get-Command cargo -ErrorAction SilentlyContinue) }
if (-not (Have-Cargo) -and $online) {
  $init = Join-Path $env:TEMP 'rustup-init.exe'
  Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $init -UseBasicParsing
  & $init -y --profile minimal 2>&1 | Out-Null
  if (Have-Cargo) { Fixed 'installed Rust' } else { Bad 'Rust install failed' }
} elseif (Have-Cargo) { Ok 'Rust present' }
function Have-Msvc {
  $vw = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  (Test-Path $vw) -and [bool](& $vw -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
}
$Rebuilt = $false
# a running ck.exe locks the file: stop the worker and head tasks and any hand-started copy before the build replaces it
function Stop-Ck {
  foreach ($t in 'ck-worker', 'ck-head') { Stop-ScheduledTask -TaskName $t -ErrorAction SilentlyContinue }
  Get-Process ck -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep 2
}
function Build-Ck([string[]]$extra) {
  & cargo build --release --manifest-path (Join-Path $Dir 'rust\Cargo.toml') @extra *>> (Join-Path $LogDir 'build.log')
  $LASTEXITCODE -eq 0
}
$cur = if (Test-Path $Stamp) { (Get-Content $Stamp -Raw).Trim() } else { '' }
if ((Test-Path $Bin) -and $zipHash -and $cur -eq $zipHash) { Ok "ck is built ($zipHash)" }
elseif (-not (Have-Cargo)) { if (Test-Path $Bin) { Ok 'using the ck that is already here (no Rust to rebuild it)' } else { Bad 'no ck.exe and no Rust to build one' } }
else {
  Write-Host '   building ck: 2 to 10 minutes (the ESP stick can be unplugged now)'
  $sw = [Diagnostics.Stopwatch]::StartNew()
  # Rust on Windows links with the MSVC C++ tools (and bundled SQLite compiles C): install them before building
  if (-not (Have-Msvc)) {
    if (Get-Command winget -ErrorAction SilentlyContinue) {
      Write-Host '   the C++ build tools are missing: installing them (several GB, 10 to 20 minutes)'
      winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent --accept-package-agreements --accept-source-agreements --override '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended' *>> (Join-Path $LogDir 'msvc.log')
    }
    if (-not (Have-Msvc)) { Bad 'C++ build tools (MSVC) are missing and could not be installed. Install "Visual Studio Build Tools" with the "Desktop development with C++" workload from https://visualstudio.microsoft.com/visual-cpp-build-tools/, then run this script again' }
  }
  Stop-Ck
  $built = Build-Ck @()
  if (-not $built) { Write-Host '   again without GPU support'; $built = Build-Ck @('--no-default-features') }
  if ($built -and (Test-Path $Bin)) { Set-Content $Stamp $zipHash; $script:Rebuilt = $true; Fixed "built ck in $([int]$sw.Elapsed.TotalSeconds) s" }
  else { Bad "ck build failed, see $LogDir\build.log"; Get-Content (Join-Path $LogDir 'build.log') -Tail 8 | ForEach-Object { Write-Host "        $_" } }
}

Step 'firewall and ssh'
if (-not (Get-NetFirewallRule -DisplayName 'Cluster Kit' -ErrorAction SilentlyContinue)) {
  New-NetFirewallRule -DisplayName 'Cluster Kit' -Direction Inbound -Action Allow -Profile Any -Protocol TCP -LocalPort 7700-7703 | Out-Null
  New-NetFirewallRule -DisplayName 'Cluster Kit UDP' -Direction Inbound -Action Allow -Profile Any -Protocol UDP -LocalPort 7701 | Out-Null
  Fixed 'opened ports 7700-7703 (tcp) and 7701 (udp)'
} else { Ok 'cluster ports are open' }
$cap = Get-WindowsCapability -Online -Name 'OpenSSH.Server*' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cap -and $cap.State -ne 'Installed' -and $online) { Add-WindowsCapability -Online -Name $cap.Name | Out-Null }
if (Get-Service sshd -ErrorAction SilentlyContinue) {
  Set-Service sshd -StartupType Automatic; if ((Get-Service sshd).Status -ne 'Running') { Start-Service sshd }
  if (-not (Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue)) { New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' -Direction Inbound -Action Allow -Protocol TCP -LocalPort 22 | Out-Null }
  if ((Get-Service sshd).Status -eq 'Running') { Ok 'ssh server running' } else { Bad 'ssh server would not start' }
  if ($env:CK_ADMIN_KEY) {
    $f = Join-Path $env:ProgramData 'ssh\administrators_authorized_keys'
    $body = ($env:CK_ADMIN_KEY -split ' ')[1]
    if (-not ((Test-Path $f) -and (Select-String -Path $f -SimpleMatch $body -Quiet))) {
      Add-Content -Path $f -Value $env:CK_ADMIN_KEY
      icacls $f /inheritance:r /grant 'Administrators:F' /grant 'SYSTEM:F' | Out-Null
      Fixed 'admin ssh key added'
    } else { Ok 'admin ssh key allowed' }
  }
} else { Bad 'ssh server is not installed (Windows capability OpenSSH.Server)' }

Step 'worker (starts at boot, restarts if it stops)'
if (Test-Path $Bin) {
  [Environment]::SetEnvironmentVariable('CK_FLEET', $Fleet, 'Machine')
  [Environment]::SetEnvironmentVariable('CK_AUTH', $Auth, 'Machine')
  [Environment]::SetEnvironmentVariable('CK_PRESENCE', 'off', 'Machine')
  $set = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
  function Install-Task($name, $taskArgs) {
    $log = Join-Path $LogDir "$name.log"
    $cmd = "/c set CK_FLEET=$Fleet&& set CK_AUTH=$Auth&& set CK_PRESENCE=off&& `"$Bin`" $taskArgs >> `"$log`" 2>&1"
    $act = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument $cmd -WorkingDirectory $Dir
    $old = Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    $same = $old -and ($old.Actions[0].Arguments -eq $cmd)
    if (-not $same) {
      Register-ScheduledTask -TaskName $name -Action $act -Trigger (New-ScheduledTaskTrigger -AtStartup) -Settings $set -User 'SYSTEM' -RunLevel Highest -Force | Out-Null
      Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
      Get-Process ck -ErrorAction SilentlyContinue | Stop-Process -Force  # copies started by hand would hold the ports
      Start-Sleep 2; Start-ScheduledTask -TaskName $name; Fixed "task $name installed and started"
    } elseif ($script:Rebuilt) {
      Stop-Ck; Start-ScheduledTask -TaskName $name; Fixed "task $name restarted on the new build"
    } elseif ((Get-ScheduledTask -TaskName $name).State -ne 'Running') { Start-ScheduledTask -TaskName $name; Fixed "task $name started" }
    else { Ok "task $name is running" }
  }
  Install-Task 'ck-worker' "worker$HeadArg"
  if ($env:CK_ROLE -eq 'head') { Install-Task 'ck-head' 'head --no-local-worker' }
  $line = ''
  foreach ($i in 1..30) {
    $logf = Join-Path $LogDir 'ck-worker.log'
    if (Test-Path $logf) { $line = Get-Content $logf -Tail 300 | Where-Object { $_ -match 'connected to head|lost head|cannot reach head|looking for head' } | Select-Object -Last 1 }
    if ($line -match 'connected to head') { break }
    Start-Sleep 1
  }
  if ($line -match 'connected to head') { Ok $line } else { Bad "not connected to a head ($line). Start the head; if broadcast is blocked set CK_HEAD_IP" }
} else { Bad 'no ck.exe, so no worker' }

Step 'result'
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1).IPAddress
Write-Host "   $env:COMPUTERNAME   $ip   build $(if (Test-Path $Stamp) { (Get-Content $Stamp -Raw).Trim() } else { 'none' })"
if ($Failed -eq 0) { Write-Host '   DONE: all good, move to the next PC' -ForegroundColor Green }
else {
  Write-Host "   $Failed PROBLEM(S), each one above and in $LogDir\provision.log:" -ForegroundColor Red
  $i = 0; foreach ($p in $Problems) { $i++; Write-Host "   $i. $p" -ForegroundColor Red }
}
[Console]::Beep(880, 200)
Stop-Transcript | Out-Null
