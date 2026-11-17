# Removes the folders that test runs leave behind in %TEMP%.
#
# Why: tests make their data folders with `tempfile` (names like `.tmpAbC123`). On Windows a
# folder with a file still open cannot be removed, and a full test cycle used to leave ~1.6 GB
# there (a few hundred cycles filled a 300 GB drive, 2026-11-17). `hermes_testdir` now removes
# almost all of them; what is left is a few small database files per run, held open by a thread
# until the test process exits. This script sweeps those.
#
# It touches only directories directly under %TEMP% named `.tmp` + 6 letters/digits, older than
# -OlderThanMinutes (default 10, so a test run in progress is never touched). Dry run unless
# -Delete is given.
#
#   powershell -NoProfile -File scripts/clean-test-temp.ps1            # shows what it would remove
#   powershell -NoProfile -File scripts/clean-test-temp.ps1 -Delete    # removes it
param(
    [switch]$Delete,
    [int]$OlderThanMinutes = 10,
    [string]$Temp = $env:TEMP
)

$ErrorActionPreference = 'Continue'
$cutoff = (Get-Date).AddMinutes(-$OlderThanMinutes)
$dirs = Get-ChildItem -LiteralPath $Temp -Force -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^\.tmp[A-Za-z0-9]{6}$' -and $_.LastWriteTime -lt $cutoff }

$bytes = 0
foreach ($d in $dirs) {
    $bytes += (Get-ChildItem -LiteralPath $d.FullName -Recurse -Force -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
}
'{0} folders, {1:N1} MB under {2}' -f $dirs.Count, ($bytes / 1MB), $Temp

if (-not $Delete) {
    'Dry run: add -Delete to remove them.'
    return
}
$failed = 0
foreach ($d in $dirs) {
    Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $d.FullName) { $failed++ }
}
'Removed {0}, could not remove {1} (a file is still open: close the program that uses it).' -f ($dirs.Count - $failed), $failed
