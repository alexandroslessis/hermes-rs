# Builds Hermes's webapp bundle (the desktop frontend in browser mode) that hermes-desktop serves.
#
#   powershell -File scripts/build-hermes-webapp.ps1 [-Hermes ..\hermes-agent] [-Commit c419cf7c]
#
# Exports Hermes at the pinned commit into target/hermes-src (the Hermes checkout is not touched)
# and runs Hermes's own build (apps/desktop/scripts/build-webapp.mjs) with the portable Node in
# tools/node. Output: target/hermes-src/apps/desktop/dist-webapp, where hermes-desktop looks for it
# (or set HERMES_UI_DIST).
param(
    [string]$Hermes = (Join-Path $PSScriptRoot '..\..\hermes-agent'),
    # The commit the hermes-rs backend ports; the frontend must match it.
    [string]$Commit = 'c419cf7c'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$node = Join-Path $root 'tools\node'
if (-not (Test-Path (Join-Path $node 'node.exe'))) {
    Write-Error "Node 26+ is expected in tools\node (the official win-x64 zip, unpacked)."
}

$src = Join-Path $root 'target\hermes-src'
if (Test-Path $src) { Remove-Item -Recurse -Force $src }
New-Item -ItemType Directory -Force $src | Out-Null
# git archive | tar: a clean copy of the commit, nothing written into the Hermes checkout.
git -C $Hermes archive $Commit | tar -x -C $src
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$env:PATH = "$node;$env:PATH"
Push-Location (Join-Path $src 'apps\desktop')
try {
    node scripts/build-webapp.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
Write-Output "Built $(Join-Path $src 'apps\desktop\dist-webapp')"
