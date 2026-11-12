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
# target\hermes-src sits inside the hermes-rs repository. Left alone, git finds that repository,
# reads the paths of a `diff --git` patch as relative to ITS root and silently skips every file
# (exit code 0; "Skipped patch" is printed only with --verbose): 0022 onwards were never applied
# that way. The ceiling stops the search at target\, so the export is not inside a repository and
# paths are relative to it; --verbose lets us refuse a patch that still skips a file.
$env:GIT_CEILING_DIRECTORIES = Split-Path -Parent $src
foreach ($patch in Get-ChildItem $patches -Filter *.patch | Sort-Object Name) {
    # git writes its progress to stderr; Windows PowerShell turns that into errors under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $out = git -C $src apply --verbose --whitespace=nowarn $patch.FullName 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { Write-Error "$($patch.Name) doesn't apply to Hermes ${Commit}:`n$out" }
    if ($out -match 'Skipped patch') { Write-Error "$($patch.Name) was skipped by git apply:`n$out" }
    Write-Output "Applied $($patch.Name)"
}
Remove-Item Env:GIT_CEILING_DIRECTORIES

$env:PATH = "$node;$env:PATH"
Push-Location (Join-Path $src 'apps\desktop')
try {
    node scripts/build-webapp.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
Write-Output "Built $(Join-Path $src 'apps\desktop\dist-webapp')"
