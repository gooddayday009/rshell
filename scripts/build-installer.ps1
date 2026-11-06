[CmdletBinding()]
param([switch]$WithAgent, [string]$MakensisPath, [string]$TargetDirectory = 'target')
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    if (-not $MakensisPath) {
        $command = Get-Command makensis.exe -ErrorAction SilentlyContinue
        if ($command) { $MakensisPath = $command.Source }
    }
    if (-not $MakensisPath) {
        foreach ($candidate in @('C:\Program Files (x86)\NSIS\Bin\makensis.exe', (Join-Path $projectRoot 'target\nsis-package\tools\Bin\makensis.exe'))) {
            if (Test-Path -LiteralPath $candidate) { $MakensisPath = $candidate; break }
        }
    }
    if (-not $MakensisPath -or -not (Test-Path -LiteralPath $MakensisPath)) { throw 'NSIS makensis.exe was not found. Supply -MakensisPath.' }
    $version = [regex]::Match((Get-Content 'Cargo.toml' -Raw), '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"([^"]+)"').Groups[1].Value
    if (-not $version) { throw 'Workspace version was not found in Cargo.toml' }
    & (Join-Path $PSScriptRoot 'build.ps1') -TargetDirectory $TargetDirectory -WithAgent:$WithAgent
    $buildDirectory = if ([System.IO.Path]::IsPathRooted($TargetDirectory)) { $TargetDirectory } else { Join-Path $projectRoot $TargetDirectory }
    $exe = Join-Path $buildDirectory 'release\rshell.exe'
    $cliExe = Join-Path $buildDirectory 'release\rshell-cli.exe'
    $output = Join-Path $projectRoot 'dist'
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    $sdkDirectory = Join-Path $buildDirectory 'release\agent-sdk'
    $suffix = if ($WithAgent) { '-full' } else { '' }
    $sdkArguments = if ($WithAgent) { @("/DAGENT_SDK_DIR=$sdkDirectory") } else { @() }
    & $MakensisPath @sdkArguments "/DBUNDLE_SUFFIX=$suffix" "/DAPP_VERSION=$version" "/DSOURCE_EXE=$exe" "/DSOURCE_CLI=$cliExe" "/DOUTPUT_DIR=$output" "/DPROJECT_ROOT=$projectRoot" (Join-Path $projectRoot 'packaging\windows\rshell.nsi')
    if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed (exit $LASTEXITCODE)" }
    $installer = Join-Path $output "RShell-$version-windows-x64$suffix-setup.exe"
    if (-not (Test-Path -LiteralPath $installer)) { throw "Installer not produced: $installer" }
    $hash = Get-FileHash -LiteralPath $installer -Algorithm SHA256
    "$($hash.Hash)  $([System.IO.Path]::GetFileName($installer))" | Set-Content -LiteralPath ($installer + '.sha256') -Encoding ascii
    Write-Host "Installer: $installer"
} finally { Pop-Location }
