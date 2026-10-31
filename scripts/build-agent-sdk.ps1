[CmdletBinding()]
param([string]$OutputDirectory = 'target\debug', [string]$Python = 'python', [switch]$InstallDependencies)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$sdkRoot = Join-Path $projectRoot 'agent-sdk'
$workRoot = Join-Path $projectRoot 'target\agent-sdk-build'
$venvPython = Join-Path $projectRoot 'target\agent-sdk-venv\Scripts\python.exe'
$destinationRoot = if ([IO.Path]::IsPathRooted($OutputDirectory)) { [IO.Path]::GetFullPath($OutputDirectory) } else { [IO.Path]::GetFullPath((Join-Path $projectRoot $OutputDirectory)) }

function Invoke-SdkPython([string[]]$Arguments) {
    & $venvPython @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Agent SDK build step failed (exit $LASTEXITCODE)" }
}

if (-not (Test-Path -LiteralPath $venvPython)) {
    & $Python -m venv (Join-Path $projectRoot 'target\agent-sdk-venv')
    if ($LASTEXITCODE -ne 0) { throw 'Python 3.10+ x64 is required to build the SDK component' }
    $InstallDependencies = $true
}
if ($InstallDependencies) {
    Invoke-SdkPython @('-m', 'pip', 'install', '--disable-pip-version-check', '--only-binary=:all:', '-r', (Join-Path $sdkRoot 'requirements.txt'))
}
New-Item -ItemType Directory -Path $workRoot -Force | Out-Null
$previousBytecode = $env:PYTHONPYCACHEPREFIX
try {
    $env:PYTHONPYCACHEPREFIX = Join-Path $workRoot 'pycache'
    Invoke-SdkPython @('-m', 'unittest', 'discover', '-s', (Join-Path $sdkRoot 'tests'))
    Invoke-SdkPython @('-m', 'PyInstaller', '--noconfirm', '--onedir', '--console', '--name', 'rshell-agent-sdk', '--distpath', (Join-Path $workRoot 'bundle'), '--workpath', (Join-Path $workRoot 'work'), '--specpath', $workRoot, '--collect-all', 'claude_agent_sdk', '--collect-data', 'mcp', '--hidden-import', 'anyio._backends._asyncio', '--recursive-copy-metadata', 'claude-agent-sdk', (Join-Path $sdkRoot 'bridge.py'))
    $bundle = Join-Path $workRoot 'bundle\rshell-agent-sdk'
    $component = Join-Path $bundle 'rshell-agent-sdk.exe'
    & $component --self-test
    if ($LASTEXITCODE -ne 0) { throw 'Bundled SDK self-test failed' }
    Invoke-SdkPython @((Join-Path $sdkRoot 'build_manifest.py'), $bundle)
    $destination = Join-Path $destinationRoot 'agent-sdk'
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Copy-Item -Path (Join-Path $bundle '*') -Destination $destination -Recurse -Force
    Write-Host "Agent SDK component: $(Join-Path $destination 'rshell-agent-sdk.exe')"
} finally { $env:PYTHONPYCACHEPREFIX = $previousBytecode }
