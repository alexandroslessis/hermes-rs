# Sets the app's version in the three places that must agree, so a release can be tagged:
#
#   powershell -File scripts/bump-version.ps1 -Version 0.3.0 [-Commit]
#
# - crates/desktop/tauri.conf.json  ("version": the installer's file name and Windows' "Installed apps")
# - crates/desktop/Cargo.toml       (the package version)
# - Cargo.lock                      (the hermes-desktop entry, so `cargo build --locked` stays valid)
#
# With -Commit it commits them as "Version X.Y.Z". Run it from a clean working tree.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$Commit
)

$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    Write-Error "The version must look like 1.2.3 (got '$Version')."
}
$root = Split-Path -Parent $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)

# Replaces the first match of $pattern in the file; fails when there is none, so a changed file
# layout is noticed instead of silently skipped. Line endings are kept as they are.
function Set-First([string]$Path, [string]$Pattern, [string]$Replacement) {
    $text = [System.IO.File]::ReadAllText($Path)
    $regex = New-Object System.Text.RegularExpressions.Regex($Pattern)
    if (-not $regex.IsMatch($text)) { Write-Error "No match for $Pattern in $Path" }
    [System.IO.File]::WriteAllText($Path, $regex.Replace($text, $Replacement, 1), $utf8)
}

Set-First (Join-Path $root 'crates\desktop\tauri.conf.json') '"version":\s*"[^"]*"' "`"version`": `"$Version`""
Set-First (Join-Path $root 'crates\desktop\Cargo.toml') '(?m)^version = "[^"]*"' "version = `"$Version`""
Set-First (Join-Path $root 'Cargo.lock') '(name = "hermes-desktop"\r?\nversion = ")[^"]*(")' "`${1}$Version`${2}"

Write-Output "Version is now $Version in tauri.conf.json, crates/desktop/Cargo.toml and Cargo.lock."
if ($Commit) {
    git -C $root add crates/desktop/tauri.conf.json crates/desktop/Cargo.toml Cargo.lock
    git -C $root commit -m "Version $Version"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} else {
    Write-Output "Review with: git diff   (then commit, or re-run with -Commit)"
}
