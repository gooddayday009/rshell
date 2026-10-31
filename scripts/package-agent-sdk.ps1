[CmdletBinding()]
param([string]$Repository = 'gooddayday009/rshell-components', [string]$ComponentVersion = '0.2.174-r1')
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if ($Repository -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' -or $ComponentVersion -notmatch '^[A-Za-z0-9_.-]+$') { throw 'Invalid repository or component version' }
$asset = "RShell-Agent-SDK-$ComponentVersion-windows-x64.zip"
$archive = Join-Path $projectRoot "dist\$asset"
if (Test-Path -LiteralPath $archive) { throw 'Component archive exists. Use a new version for a changed component.' }
$stage = Join-Path $projectRoot ('target\agent-component-release\' + [Guid]::NewGuid().ToString('N'))
& (Join-Path $PSScriptRoot 'build-agent-sdk.ps1') -OutputDirectory $stage
New-Item -ItemType Directory -Path (Join-Path $projectRoot 'dist') -Force | Out-Null
Compress-Archive -LiteralPath (Join-Path $stage 'agent-sdk') -DestinationPath $archive
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
$descriptor = [ordered]@{
    version = $ComponentVersion
    url = "https://github.com/$Repository/releases/download/agent-sdk-$ComponentVersion/$asset"
    sha256 = $hash
    size = (Get-Item -LiteralPath $archive).Length
}
$utf8 = [System.Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $projectRoot 'agent-sdk\component.json'), ($descriptor | ConvertTo-Json) + "`n", $utf8)
[IO.File]::WriteAllText(($archive + '.sha256'), "$hash  $asset`n", $utf8)
Write-Host "Component archive: $archive"
Write-Host 'Pinned descriptor updated: agent-sdk/component.json. Publish this exact asset before distributing the application.'
