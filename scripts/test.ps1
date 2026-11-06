[CmdletBinding()]
param([switch]$WithCredentials, [switch]$Smoke)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
function Invoke-Cargo([string[]]$CargoArguments) {
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArguments -join ' ') failed (exit $LASTEXITCODE)" }
}
Push-Location $projectRoot
try {
    Invoke-Cargo @('fmt', '--all', '--', '--check')
    Invoke-Cargo @('clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings')
    Invoke-Cargo @('test', '--locked', '--workspace', '--all-targets')
    if ($WithCredentials) {
        Invoke-Cargo @('test', '--locked', '-p', 'rshell-core', 'encrypted_credentials_roundtrip')
        Invoke-Cargo @('test', '--locked', '-p', 'rshell-core', 'jumpserver_credentials_roundtrip', '--', '--ignored')
        Invoke-Cargo @('test', '--locked', '-p', 'rshell', 'saved_password_auto_auth_mfa_and_failure_stop', '--', '--ignored')
    }
    if ($Smoke) {
        Invoke-Cargo @('build', '--locked', '-p', 'rshell')
        $artifactDirectory = Join-Path $projectRoot 'target\test-artifacts'
        $buildDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $projectRoot 'target' }
        New-Item -ItemType Directory -Path $artifactDirectory -Force | Out-Null
        $previousConfig = $env:RSHELL_CONFIG_DIR
        $previousScreenshot = $env:RSHELL_SCREENSHOT
        try {
            $env:RSHELL_CONFIG_DIR = Join-Path $artifactDirectory ('smoke-config-' + [Guid]::NewGuid().ToString('N'))
            foreach ($mode in @('demo', 'tabs-fit', 'tabs-overflow', 'split', 'local', 'serial', 'ai-settings', 'ai-chat', 'ai-chat-long', 'ai-chat-maximized', 'ai-chat-font-large', 'appearance', 'session', 'session-options', 'sftp', 'import', 'telnet', 'host-keys', 'user-keys', 'hidden-chrome', 'history-settings', 'tunnels', 'tunnels-light', 'tunnel-status')) {
                $env:RSHELL_SCREENSHOT = Join-Path $artifactDirectory ("smoke-$mode.png")
                $startedAt = [DateTime]::UtcNow
                $arguments = @('--smoke-test')
                if ($mode -in @('demo', 'tabs-fit', 'tabs-overflow', 'split', 'serial', 'ai-settings', 'ai-chat', 'appearance', 'session', 'session-options', 'sftp', 'import', 'telnet', 'host-keys', 'user-keys', 'hidden-chrome', 'history-settings', 'tunnels', 'tunnels-light', 'tunnel-status')) { $arguments += '--demo' }
                if ($mode -in @('tunnels', 'tunnels-light')) { $arguments += '--smoke-tunnels' }
                if ($mode -eq 'tunnels-light') { $arguments += '--smoke-light' }
                if ($mode -eq 'tunnel-status') { $arguments += '--smoke-tunnel-status' }
                if ($mode -in @('host-keys', 'user-keys', 'hidden-chrome', 'history-settings')) { $arguments += "--smoke-$mode" }
                if ($mode -eq 'split') { $arguments += '--smoke-split' }
                if ($mode -eq 'tabs-fit') { $arguments += '--smoke-tabs-fit' }
                if ($mode -eq 'tabs-overflow') { $arguments += '--smoke-tabs-overflow' }
                if ($mode -eq 'import') { $arguments += '--smoke-import' }
                if ($mode -eq 'telnet') { $arguments += '--smoke-telnet' }
                if ($mode -eq 'appearance') { $arguments += '--smoke-appearance' }
                if ($mode -eq 'sftp') { $arguments += '--smoke-sftp' }
                if ($mode -eq 'session') { $arguments += '--smoke-session' }
                if ($mode -eq 'session-options') { $arguments += '--smoke-session-options' }
                if ($mode -eq 'serial') { $arguments += '--smoke-serial' }
                if ($mode -eq 'ai-chat') { $arguments += '--smoke-ai-chat' }
                if ($mode -eq 'ai-chat-long') { $arguments += @('--demo', '--smoke-ai-chat', '--smoke-ai-long-draft') }
                if ($mode -eq 'ai-chat-maximized') { $arguments += @('--demo', '--smoke-ai-chat', '--smoke-ai-long-draft', '--smoke-ai-maximized') }
                if ($mode -eq 'ai-chat-font-large') { $arguments += @('--demo', '--smoke-ai-chat', '--smoke-ai-long-draft', '--smoke-ai-font-large') }
                if ($mode -eq 'ai-settings') { $arguments += '--ai-settings' }
                $process = Start-Process -FilePath (Join-Path $buildDirectory 'debug\rshell.exe') -ArgumentList $arguments -WindowStyle Hidden -PassThru
                if (-not $process.WaitForExit(30000)) {
                    $process.Kill()
                    throw "Window smoke test timed out ($mode)"
                }
                if ($process.ExitCode -ne 0) { throw "Window smoke test failed ($mode, exit $($process.ExitCode))" }
                if (-not (Test-Path -LiteralPath $env:RSHELL_SCREENSHOT)) { throw 'Smoke screenshot was not produced' }
                if ((Get-Item -LiteralPath $env:RSHELL_SCREENSHOT).LastWriteTimeUtc -lt $startedAt) { throw 'Smoke screenshot is stale' }
                Write-Host "Screenshot: $env:RSHELL_SCREENSHOT"
            }
        } finally {
            $env:RSHELL_CONFIG_DIR = $previousConfig
            $env:RSHELL_SCREENSHOT = $previousScreenshot
        }
    }
    Write-Host 'All requested checks passed.'
} finally { Pop-Location }
