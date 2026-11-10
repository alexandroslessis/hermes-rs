# Builds the Windows installer and publishes it as a GitHub Release.
#
#   powershell -File scripts/release.ps1 [-Notes notes.md] [-Draft] [-DryRun]
#                                        [-SkipBuild] [-TargetDir <folder>]
#
# What it does, in order (it stops at the first problem, before building anything it can):
#   1. Checks: the GitHub CLI (`gh`) is installed and signed in; the working tree is clean; you are
#      on `main` and everything is pushed; the version in tauri.conf.json, crates/desktop/Cargo.toml
#      and Cargo.lock agree; the tag `v<version>` doesn't exist yet.
#   2. Builds Hermes's frontend (scripts/build-hermes-webapp.ps1) and the installer (Tauri's NSIS
#      bundle), into -TargetDir (default target\release-build; theirs never locks a running app).
#   3. Writes a SHA-256 file next to the installer and creates the release `v<version>` on the
#      commit you are on, with both files attached. Release notes: -Notes <file>, else GitHub's
#      generated list of what changed since the last release.
#
# The version comes from crates/desktop/Cargo.toml (raise it first with scripts/bump-version.ps1).
# -DryRun does the checks that don't need `gh`, builds nothing, and prints what it would run.
# -SkipBuild publishes an installer that is already in -TargetDir (a rerun after a failed upload).
# -Draft creates the release unpublished, for a last look on github.com.
param(
    [string]$Notes = '',
    [switch]$Draft,
    [switch]$DryRun,
    [switch]$SkipBuild,
    [string]$TargetDir = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (-not $TargetDir) { $TargetDir = Join-Path $root 'target\release-build' }

function Fail([string]$Message) { Write-Error $Message }
function Step([string]$Message) { Write-Output "==> $Message" }

# --- the version, and that every file agrees on it ---------------------------------------------
$cargoToml = Get-Content (Join-Path $root 'crates\desktop\Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version = "([^"]+)"') { Fail 'No version in crates/desktop/Cargo.toml.' }
$version = $Matches[1]
$tauriConf = Get-Content (Join-Path $root 'crates\desktop\tauri.conf.json') -Raw | ConvertFrom-Json
if ($tauriConf.version -ne $version) {
    Fail "tauri.conf.json says $($tauriConf.version), Cargo.toml says $version. Run scripts/bump-version.ps1."
}
$lock = Get-Content (Join-Path $root 'Cargo.lock') -Raw
if ($lock -notmatch '(?m)name = "hermes-desktop"\r?\nversion = "([^"]+)"' -or $Matches[1] -ne $version) {
    Fail "Cargo.lock disagrees with Cargo.toml ($version). Run scripts/bump-version.ps1."
}
$tag = "v$version"
Step "Releasing Hermes $version as $tag"

# --- the repository is in a shippable state ----------------------------------------------------
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'main') { Fail "You are on '$branch'. Releases are made from main." }
if (git status --porcelain) { Fail 'The working tree has uncommitted changes. Commit or stash them first.' }
git fetch --quiet origin main --tags
if ($LASTEXITCODE -ne 0) { Fail 'git fetch failed (network, or no access to origin).' }
$head = (git rev-parse HEAD).Trim()
if ($head -ne (git rev-parse origin/main).Trim()) {
    $ahead = (git rev-list --count origin/main..HEAD).Trim()
    $behind = (git rev-list --count HEAD..origin/main).Trim()
    Fail "main and origin/main differ ($ahead ahead, $behind behind). Push or pull so the release is built from what is on GitHub."
}
if (git tag --list $tag) { Fail "The tag $tag already exists. Raise the version (scripts/bump-version.ps1)." }
if (git ls-remote --tags origin "refs/tags/$tag") { Fail "The tag $tag already exists on GitHub." }
if ($Notes -and -not (Test-Path $Notes)) { Fail "Notes file not found: $Notes" }

# --- the GitHub CLI ----------------------------------------------------------------------------
$gh = Get-Command gh -ErrorAction SilentlyContinue
if (-not $gh -and -not $DryRun) {
    Fail 'The GitHub CLI is not installed. Install it (winget install GitHub.cli), then run: gh auth login'
}
if ($gh -and -not $DryRun) {
    gh auth status *> $null
    if ($LASTEXITCODE -ne 0) { Fail 'The GitHub CLI is not signed in. Run: gh auth login' }
}

# --- build -------------------------------------------------------------------------------------
$installer = Join-Path $TargetDir "release\bundle\nsis\Hermes_${version}_x64-setup.exe"
if ($SkipBuild) {
    if (-not (Test-Path $installer)) { Fail "-SkipBuild, but there is no installer at $installer" }
} elseif ($DryRun) {
    Step "Would build the webapp and the installer into $TargetDir"
} else {
    Step 'Building Hermes''s frontend'
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build-hermes-webapp.ps1')
    if ($LASTEXITCODE -ne 0) { Fail 'The frontend build failed.' }
    Step 'Building the app and the installer (the first build takes 10+ minutes)'
    $env:CARGO_TARGET_DIR = $TargetDir
    $nodeDir = Join-Path $root 'tools\node'
    if (Test-Path $nodeDir) { $env:PATH = "$nodeDir;$env:PATH" }
    Push-Location (Join-Path $root 'crates\desktop')
    try {
        npx -y '@tauri-apps/cli@2' build
        if ($LASTEXITCODE -ne 0) { Fail 'The installer build failed.' }
    } finally {
        Pop-Location
    }
    if (-not (Test-Path $installer)) { Fail "The build finished but $installer is not there." }
}

# --- publish -----------------------------------------------------------------------------------
$files = @($installer)
$checksum = "$installer.sha256"
if (Test-Path $installer) {
    $hash = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLower()
    [System.IO.File]::WriteAllText($checksum, "$hash  $(Split-Path -Leaf $installer)`n")
    $files += $checksum
    Step ("Installer: {0} ({1:N1} MB), sha256 {2}" -f (Split-Path -Leaf $installer), ((Get-Item $installer).Length / 1MB), $hash)
}
$arguments = @('release', 'create', $tag) + $files + @('--title', "Hermes $version", '--target', $head)
if ($Notes) { $arguments += @('--notes-file', $Notes) } else { $arguments += '--generate-notes' }
if ($Draft) { $arguments += '--draft' }

if ($DryRun) {
    Step 'Dry run: nothing was built or published. It would run:'
    Write-Output "gh $($arguments -join ' ')"
    exit 0
}
Step "Creating the release $tag on commit $($head.Substring(0, 7))"
& gh @arguments
if ($LASTEXITCODE -ne 0) { Fail 'gh release create failed. The installer is built: fix the cause and rerun with -SkipBuild.' }
Step 'Done:'
gh release view $tag --json url --jq .url
