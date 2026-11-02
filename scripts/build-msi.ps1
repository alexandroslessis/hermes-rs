# Builds the Windows installer: target\hermes-rs-<version>-x64.msi.
# Needs the WiX Toolset v4+ command line (`wix`), e.g. `dotnet tool install --global wix`.
# Publishing: attach the .msi to a GitHub release tagged v<version>; the app's updater finds it.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    Write-Error 'The WiX Toolset command line (wix) is not installed. Install it with: dotnet tool install --global wix'
}

$manifest = Get-Content 'crates\app\Cargo.toml' -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    Write-Error 'No version in crates\app\Cargo.toml'
}
$version = $Matches[1]

cargo build --release -p hermes-rs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$out = "target\hermes-rs-$version-x64.msi"
wix build 'installer\hermes-rs.wxs' -arch x64 -d "Version=$version" -d 'Exe=target\release\hermes-rs.exe' -o $out
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output "Built $out"
