# Builds Hermes's webapp bundle (the desktop frontend in browser mode) that hermes-desktop serves.
#
#   powershell -File scripts/build-hermes-webapp.ps1 [-Hermes ..\hermes-agent] [-Commit <sha>]
#
# Exports Hermes at the pinned commit (HERMES_COMMIT at the repository root, unless -Commit says
# otherwise) into target/hermes-src (the Hermes checkout is not touched)
# and runs Hermes's own build (apps/desktop/scripts/build-webapp.mjs) with the portable Node in
# tools/node. Output: target/hermes-src/apps/desktop/dist-webapp, where hermes-desktop looks for it
# (or set HERMES_UI_DIST).
param(
    [string]$Hermes = (Join-Path $PSScriptRoot '..\..\hermes-agent'),
    # The commit the hermes-rs backend ports; the frontend must match it.
    [string]$Commit = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Commit) { $Commit = (Get-Content (Join-Path $root 'HERMES_COMMIT') -Raw).Trim() }
$node = Join-Path $root 'tools\node'
if (-not (Test-Path (Join-Path $node 'node.exe'))) {
    Write-Error "Node 26+ is expected in tools\node (the official win-x64 zip, unpacked)."
}

$src = Join-Path $root 'target\hermes-src'
if (Test-Path $src) { Remove-Item -Recurse -Force $src }
New-Item -ItemType Directory -Force $src | Out-Null
# git archive, then tar: a clean copy of the commit, nothing written into the Hermes checkout.
# Through a file, not a pipe: Windows PowerShell pipes text and would corrupt the archive.
$archive = Join-Path $root 'target\hermes-src.tar'
git -C $Hermes archive --format=tar -o $archive $Commit
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
tar -x -f $archive -C $src
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Remove-Item $archive

# hermes-rs's few changes to the frontend (crates/desktop/patches, in order). A patch that no
# longer applies after a Hermes update stops the build: refresh it against the new commit.
$patches = Join-Path $root 'crates\desktop\patches'
foreach ($patch in Get-ChildItem $patches -Filter *.patch | Sort-Object Name) {
    git -C $src apply --whitespace=nowarn $patch.FullName
    if ($LASTEXITCODE -ne 0) { Write-Error "$($patch.Name) doesn't apply to Hermes $Commit" }
    Write-Output "Applied $($patch.Name)"
}

$env:PATH = "$node;$env:PATH"
Push-Location (Join-Path $src 'apps\desktop')
try {
    node scripts/build-webapp.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
Write-Output "Built $(Join-Path $src 'apps\desktop\dist-webapp')"
