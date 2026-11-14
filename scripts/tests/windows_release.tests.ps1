$ErrorActionPreference = 'Stop'
$path = Join-Path (Get-Location) 'scripts/provision.ps1'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$fn = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Install-CkRelease' }, $true)
if (-not $fn) { throw 'Missing Install-CkRelease: Windows still requires a local compiler' }
. ([scriptblock]::Create($fn.Extent.Text))
function Assert($ok, $message) { if (-not $ok) { throw $message } }
$root = Join-Path ([IO.Path]::GetTempPath()) ('ck release [test] ' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $Repo = 'fixture/repository'
    $commit = 'a' * 40
    $tag = 'windows-' + $commit
    $fixture = Join-Path $root 'fixture.exe'
    [IO.File]::WriteAllBytes($fixture, [byte[]](77,90,1,2,3,4))
    $sha = (Get-FileHash -LiteralPath $fixture -Algorithm SHA256).Hash.ToLower()
    $script:manifest = @{ commit = $commit; sha256 = $sha; target = 'x86_64-pc-windows-msvc' }
    $script:stops = 0; $script:downloads = 0
    function Stop-Ck { $script:stops++ }
    function Invoke-RestMethod {
        param($Uri, [switch]$UseBasicParsing, $ErrorAction)
        if ($Uri -like 'https://api.github.com/*') { return @([pscustomobject]@{tag_name=$tag; draft=$false; prerelease=$false}) }
        $script:manifest
    }
    function Invoke-WebRequest {
        param($Uri, $OutFile, [switch]$UseBasicParsing, $ErrorAction)
        Assert ($Uri -eq "https://github.com/$Repo/releases/download/$tag/ck-windows-x86_64.exe") 'Unexpected executable download URL'
        $script:downloads++
        Copy-Item -LiteralPath $fixture -Destination $OutFile
    }
    $bin = Join-Path $root 'Desktop [pc]/rust/target/release/ck.exe'
    $stamp = Join-Path (Split-Path $bin) '.ck-built'
    $stage = Join-Path $root 'stage [1]'
    Assert (Install-CkRelease $stage $bin $stamp) 'New release not installed'
    Assert ((Get-FileHash -LiteralPath $bin).Hash.ToLower() -eq $sha) 'Installed binary differs'
    Assert ($script:stops -eq 1) 'Worker must stop only after validation'
    Assert (-not (Install-CkRelease $stage $bin $stamp)) 'Unchanged binary was reinstalled'
    Assert ($script:stops -eq 1 -and $script:downloads -eq 1) 'Unchanged binary should be reused after checksum verification'
    Write-Host 'PASS: first install and verified idempotent reuse, with spaces/brackets'

    Set-Content -LiteralPath $bin -Value 'damaged old binary'
    Assert (Install-CkRelease $stage $bin $stamp) 'Damaged installation must be repaired'
    Assert ($script:stops -eq 2) 'Repair did not replace binary'
    Write-Host 'PASS: corrupt existing binary repaired'

    $script:manifest.sha256 = '0' * 64
    $failed = $false
    try { Install-CkRelease $stage $bin $stamp } catch { $failed = $true }
    Assert $failed 'Checksum mismatch accepted'
    Assert ($script:stops -eq 2) 'Unverified download stopped the worker'
    Assert ((Get-FileHash -LiteralPath $bin).Hash.ToLower() -eq $sha) 'Unverified download replaced good binary'
    Write-Host 'PASS: checksum mismatch preserves running installation'

    $script:manifest.sha256 = $sha; $script:manifest.commit = 'b' * 40
    $failed = $false
    try { Install-CkRelease $stage $bin $stamp } catch { $failed = $true }
    Assert $failed 'Manifest/release identity mismatch accepted'
    Write-Host 'PASS: mismatched release identity rejected'

    $fn = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Install-Task' }, $true)
    . ([scriptblock]::Create($fn.Extent.Text))
    $Fleet = 'open'; $Auth = 'off'; $Dir = $root; $Bin = $bin; $LogDir = $root; $set = @{}
    $script:tasks = @{}; $script:Rebuilt = $false
    function New-ScheduledTaskAction { param($Execute, $Argument, $WorkingDirectory) [pscustomobject]@{Arguments=$Argument} }
    function New-ScheduledTaskTrigger { param([switch]$AtStartup) }
    function Get-ScheduledTask { param($TaskName, $ErrorAction) $script:tasks[$TaskName] }
    function Register-ScheduledTask { param($TaskName, $Action, $Trigger, $Settings, $User, $RunLevel, [switch]$Force) $script:tasks[$TaskName] = [pscustomobject]@{Actions=@($Action); State='Ready'} }
    function Stop-ScheduledTask { param($TaskName, $ErrorAction) if ($script:tasks[$TaskName]) { $script:tasks[$TaskName].State='Ready' } }
    function Start-ScheduledTask { param($TaskName) $script:tasks[$TaskName].State='Running' }
    function Get-Process { }
    function Start-Sleep { }
    function Fixed { }
    function Ok { }
    Install-Task 'ck-worker' 'worker'
    Install-Task 'ck-head' 'head --no-local-worker'
    $script:Rebuilt = $true; $script:stops = 0
    Install-Task 'ck-worker' 'worker'
    Install-Task 'ck-head' 'head --no-local-worker'
    Assert ($script:stops -eq 0) 'Restarting one scheduled role must not stop every ck process'
    Assert ($script:tasks['ck-worker'].State -eq 'Running' -and $script:tasks['ck-head'].State -eq 'Running') 'Both roles must stay running'
    Write-Host 'PASS: updating head and worker tasks does not stop the other role'
    $global:LASTEXITCODE = 0
} finally { Remove-Item -LiteralPath $root -Recurse -Force }
