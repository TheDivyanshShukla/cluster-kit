# Run with Windows PowerShell 5.1 or pwsh. No installers or Windows settings are changed.
$ErrorActionPreference = 'Stop'
$sourcePath = Join-Path $PSScriptRoot '../provision.ps1'
$source = Get-Content -LiteralPath $sourcePath -Raw
$tokens = $null; $parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($sourcePath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
foreach ($name in 'Sync-CkSource', 'Install-Msvc') {
    $fn = $ast.Find({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if (-not $fn) { throw "Missing production helper: $name" }
    . ([scriptblock]::Create($fn.Extent.Text))
}
function Assert($ok, $message) { if (-not $ok) { throw $message } }
$root = Join-Path ([IO.Path]::GetTempPath()) ('ck windows [test] ' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $root | Out-Null
$originalTemp = $env:TEMP
$LogDir = Join-Path ([IO.Path]::GetTempPath()) ('ck-windows-log-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $LogDir | Out-Null
try {
    $fixture = Join-Path $root 'fixture/repository/rust'
    New-Item -ItemType Directory -Path $fixture -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $fixture 'Cargo.toml') -Value '[package]'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = Join-Path $root 'fixture.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory((Join-Path $root 'fixture'), $archive)
    $script:downloadArchive = $true
    $script:signatureStatus = 'Valid'
    $script:signer = 'CN=Microsoft Corporation, O=Microsoft Corporation, C=US'
    $script:installerExit = 0
    $script:launches = 0
    $script:msvcReady = $false
    $script:wingetPresent = $false
    $script:wingetWorks = $false
    function Invoke-WebRequest {
        param($Uri, $OutFile, [switch]$UseBasicParsing, $ErrorAction)
        if ($script:downloadArchive) { Copy-Item -LiteralPath $archive -Destination $OutFile }
        else { Set-Content -LiteralPath $OutFile -Value 'fixture installer' }
    }
    function robocopy {
        param($Source, $Destination)
        Get-ChildItem -LiteralPath $Source | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $Destination -Recurse -Force }
        $global:LASTEXITCODE = 1
    }
    function Get-Command {
        param($Name, $ErrorAction)
        if ($Name -eq 'winget' -and $script:wingetPresent) { [pscustomobject]@{ Name = 'winget' } }
    }
    function winget { if ($script:wingetWorks) { $script:msvcReady = $true }; $global:LASTEXITCODE = 1 }
    function Have-Msvc { $script:msvcReady }
    function Get-AuthenticodeSignature {
        param($FilePath)
        [pscustomobject]@{ Status = $script:signatureStatus; SignerCertificate = [pscustomobject]@{ Subject = $script:signer } }
    }
    function Start-Process {
        param($FilePath, $ArgumentList, [switch]$Wait, [switch]$PassThru, $ErrorAction)
        Assert ($Wait -and $PassThru) 'Installer must wait and check its result'
        Assert ($ArgumentList -contains '--norestart') 'Installer must not reboot the PC'
        Assert ($ArgumentList -contains 'Microsoft.VisualStudio.Workload.VCTools') 'C++ workload missing'
        $script:launches++
        if ($script:installerExit -eq 0) { $script:msvcReady = $true }
        [pscustomobject]@{ ExitCode = $script:installerExit }
    }
    $Repo = 'fixture/repository'; $online = $true
    $StageDir = Join-Path $root 'stage [1]'
    $env:TEMP = Join-Path $root 'missing-short-user-path'
    $destination = Join-Path $root 'Desktop [lab]/cluster-kit'
    $hash = Sync-CkSource $StageDir $destination 'main'
    Assert (Test-Path -LiteralPath (Join-Path $destination 'rust/Cargo.toml')) 'Source sync failed for spaces/brackets and an invalid TEMP'
    Assert ($hash -match '^[0-9a-f]{10}$') 'Source hash missing'
    Write-Host 'PASS: source sync with spaces, brackets and invalid user TEMP'

    $script:downloadArchive = $false
    $rejected = $false
    try { Sync-CkSource (Join-Path $root 'bad-source') $destination 'main' | Out-Null } catch { $rejected = $true }
    Assert $rejected 'An invalid archive must fail before source replacement'
    Assert (Test-Path -LiteralPath (Join-Path $destination 'rust/Cargo.toml')) 'Invalid archive damaged existing source'
    Write-Host 'PASS: malformed archive rejected without replacing existing source'
    Assert (Install-Msvc) 'Direct installer must work without winget'
    Assert ($script:launches -eq 1) 'Direct fallback did not run exactly once'
    Assert (Install-Msvc) 'Already installed tools should succeed'
    Assert ($script:launches -eq 1) 'Already installed tools were reinstalled'
    Write-Host 'PASS: absent winget and already installed tools'

    $script:msvcReady = $false; $script:wingetPresent = $true
    Assert (Install-Msvc) 'Broken winget must fall back to direct installer'
    Assert ($script:launches -eq 2) 'Broken winget did not use the direct fallback'
    Write-Host 'PASS: broken winget fallback'

    $script:msvcReady = $false; $script:wingetWorks = $true
    Assert (Install-Msvc) 'Working winget should install the tools'
    Assert ($script:launches -eq 2) 'Working winget unnecessarily used the fallback'
    Write-Host 'PASS: working winget avoids fallback'
    $script:wingetWorks = $false

    $script:msvcReady = $false; $script:signatureStatus = 'NotSigned'
    Assert (-not (Install-Msvc)) 'Unsigned installer must be refused'
    Assert ($script:launches -eq 2) 'Unsigned installer was launched'
    Write-Host 'PASS: unsigned installer rejected'

    $script:signatureStatus = 'Valid'; $script:installerExit = 1603
    Assert (-not (Install-Msvc)) 'Failed installer must not report success'
    Assert ($script:launches -eq 3) 'Failed installer was not exercised'
    Write-Host 'PASS: installer failure reported'

    $script:installerExit = 3010
    Assert (-not (Install-Msvc)) 'Restart-required installer must not proceed to cargo'
    Assert ($script:launches -eq 4) 'Restart-required installer was not exercised'
    Write-Host 'PASS: required restart does not start a build'

    function Install-Msvc { $false }
    function Have-Cargo { $true }
    function Bad($message) { $script:lastProblem = $message }
    function Build-Ck { throw 'Cargo must not run without MSVC' }
    function Stop-Ck { throw 'Worker must not stop before prerequisites are ready' }
    $SourceReady = $true
    $Bin = Join-Path $root 'missing.exe'; $Stamp = Join-Path $root 'missing.stamp'; $zipHash = ''
    $start = $source.IndexOf('$cur = if (Test-Path $Stamp)')
    $end = $source.IndexOf("Step 'firewall and ssh'", $start)
    . ([scriptblock]::Create($source.Substring($start, $end - $start)))
    Assert ($script:lastProblem -match 'MSVC|C\+\+') 'Missing MSVC not reported'
    Write-Host 'PASS: cargo and worker replacement gated on prerequisites'
    $SourceReady = $false
    . ([scriptblock]::Create($source.Substring($start, $end - $start)))
    Assert ($script:lastProblem -match 'source') 'Bad source must block the build'
    Write-Host 'PASS: failed source sync blocks cargo'
} finally {
    $env:TEMP = $originalTemp
    Remove-Item -LiteralPath $root -Recurse -Force
    Remove-Item -LiteralPath $LogDir -Recurse -Force
}
