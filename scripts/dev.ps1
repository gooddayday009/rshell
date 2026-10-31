[CmdletBinding()]
param(
    [switch]$Demo,
    [string]$ConfigDirectory
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $ConfigDirectory) { $ConfigDirectory = Join-Path $projectRoot 'target\local-config' }
$previousConfig = $env:RSHELL_CONFIG_DIR
Push-Location $projectRoot
try {
    $env:RSHELL_CONFIG_DIR = [System.IO.Path]::GetFullPath($ConfigDirectory)
    Write-Host "RShell config: $env:RSHELL_CONFIG_DIR"
    $buildDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $projectRoot 'target' }
    $cargoArguments = @('run', '--locked', '-p', 'rshell')
    if ($Demo) { $cargoArguments += @('--', '--demo') }
    & cargo @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw "RShell exited with code $LASTEXITCODE" }
} finally {
    $env:RSHELL_CONFIG_DIR = $previousConfig
    Pop-Location
}
