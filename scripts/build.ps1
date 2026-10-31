[CmdletBinding()]
param([switch]$Package, [switch]$WithAgent, [string]$TargetDirectory = 'target')
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    $buildDirectory = if ([System.IO.Path]::IsPathRooted($TargetDirectory)) { $TargetDirectory } else { Join-Path $projectRoot $TargetDirectory }
    & cargo build --locked --release -p rshell --target-dir $buildDirectory
    if ($LASTEXITCODE -ne 0) { throw "Release build failed (exit $LASTEXITCODE)" }
    if ($WithAgent) { & (Join-Path $PSScriptRoot 'build-agent-sdk.ps1') -OutputDirectory (Join-Path $buildDirectory 'release') }
    $executable = Join-Path $buildDirectory 'release\rshell.exe'
    Write-Host "Executable: $executable"
    if ($Package) {
        $distDirectory = Join-Path $projectRoot 'dist'
        $version = [regex]::Match((Get-Content 'Cargo.toml' -Raw), '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"([^"]+)"').Groups[1].Value
        $flavor = if ($WithAgent) { 'full' } else { 'portable' }
        $bundleName = "RShell-$version-windows-x64-$flavor"
        $bundleDirectory = Join-Path $distDirectory $bundleName
        if (Test-Path -LiteralPath $bundleDirectory) { throw "Package directory exists: $bundleDirectory" }
        New-Item -ItemType Directory -Path (Join-Path $bundleDirectory 'docs') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $bundleDirectory 'licenses') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $bundleDirectory 'packaging\windows') -Force | Out-Null
        Copy-Item -LiteralPath $executable -Destination $bundleDirectory
        if ($WithAgent) { Copy-Item -LiteralPath (Join-Path $buildDirectory 'release\agent-sdk') -Destination $bundleDirectory -Recurse }
        Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $bundleDirectory
        Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $bundleDirectory
        $planDocument = Join-Path $projectRoot 'Rust重写Xshell工具方案.md'
        if (Test-Path -LiteralPath $planDocument) {
            Copy-Item -LiteralPath $planDocument -Destination $bundleDirectory
        } else {
            Write-Warning "Plan document missing; portable package omits $planDocument"
        }
        Copy-Item -Path (Join-Path $projectRoot 'docs\*.md') -Destination (Join-Path $bundleDirectory 'docs')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'packaging\windows\README.md') -Destination (Join-Path $bundleDirectory 'packaging\windows')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'packaging\windows\PORTABLE.md') -Destination (Join-Path $bundleDirectory 'START-HERE.md')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'crates\rshell-app\assets\fonts\LICENSE.txt') -Destination (Join-Path $bundleDirectory 'licenses\Geist-OFL.txt')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'crates\rshell-app\assets\lucide\LICENSE') -Destination (Join-Path $bundleDirectory 'licenses\Lucide.txt')
        $zipPath = Join-Path $distDirectory ($bundleName + '.zip')
        Compress-Archive -LiteralPath $bundleDirectory -DestinationPath $zipPath
        $hash = Get-FileHash -LiteralPath $zipPath -Algorithm SHA256
        "$($hash.Hash)  $([System.IO.Path]::GetFileName($zipPath))" | Set-Content -LiteralPath ($zipPath + '.sha256') -Encoding ascii
        Write-Host "Portable test package: $zipPath"
    }
} finally { Pop-Location }
