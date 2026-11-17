# The native UI's checks (docs/TESTING.md, "Native UI"): build crates/native with the element
# dump, run its unit and layout tests, then the snapshot tests. Prints only what failed and the
# paths of snapshot images to look at (*.diff.png for a changed story, *.actual.png for a new one),
# then one summary line. Exit code 0 when everything passed.
#
#   powershell -NoProfile -File scripts/native/check.ps1            # check
#   powershell -NoProfile -File scripts/native/check.ps1 -Update    # accept the rendered snapshots
#
# Builds into target/claude (never the user's target/debug); our instance's data is
# target/native-test. Nothing here starts a window.
param([switch]$Update)

$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$env:CARGO_TARGET_DIR = Join-Path $root 'target\claude'
$env:HERMES_RS_HOME = Join-Path $root 'target\native-test'
New-Item -ItemType Directory -Force $env:HERMES_RS_HOME | Out-Null
if ($Update) { $env:UPDATE_SNAPSHOTS = '1' } else { Remove-Item Env:UPDATE_SNAPSHOTS -ErrorAction SilentlyContinue }

$logs = Join-Path $env:CARGO_TARGET_DIR 'native-check'
New-Item -ItemType Directory -Force $logs | Out-Null
$snapshots = Join-Path $env:CARGO_TARGET_DIR 'snapshots'
# Images from an earlier run would be reported as this run's.
if (Test-Path $snapshots) { Get-ChildItem $snapshots -Filter '*.png' | Remove-Item -Force }

$failed = @()

function Step([string]$name, [string]$cargoArgs) {
    $log = Join-Path $logs "$name.log"
    # Through cmd so cargo's progress on stderr doesn't turn into PowerShell errors (5.1).
    Push-Location $root
    cmd /c "cargo $cargoArgs > `"$log`" 2>&1"
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -eq 0) { return }
    $script:failed += $name
    Write-Output "FAIL $name (cargo $cargoArgs), log: $log"
    $lines = Get-Content $log
    $errors = $lines | Where-Object { $_ -match '^error(\[E\d+\])?:' -or $_ -match '^\s+--> ' }
    $errors | Select-Object -First 40 | ForEach-Object { Write-Output "  $_" }
    # Each failed test's output sits between `---- <name> stdout ----` and the next blank-line
    # separated `failures:` list.
    $inFailure = $false
    foreach ($line in $lines) {
        if ($line -match '^---- (.+) stdout ----$') { $inFailure = $true; Write-Output "  test $($Matches[1]):"; continue }
        if ($line -match '^failures:$') { $inFailure = $false; continue }
        if ($inFailure -and $line.Trim() -ne '' -and $line -notmatch '^note: run with') { Write-Output "    $line" }
    }
}

Step 'build' 'build -p hermes-native --features dev --bins'
if ($failed.Count -eq 0) {
    Step 'tests' 'test -p hermes-native'
    Step 'snapshots' 'test -p hermes-native --features snapshots'
}

if (Test-Path $snapshots) {
    foreach ($image in Get-ChildItem $snapshots -Filter '*.diff.png') { Write-Output "diff: $($image.FullName)" }
    foreach ($image in Get-ChildItem $snapshots -Filter '*.actual.png') {
        $diff = $image.FullName -replace '\.actual\.png$', '.diff.png'
        if (-not (Test-Path $diff)) { Write-Output "new (no baseline): $($image.FullName)" }
    }
}

if ($failed.Count -eq 0) {
    if ($Update) { Write-Output 'native check: all ok (snapshots accepted: review git diff crates/native/snapshots)' }
    else { Write-Output 'native check: all ok' }
    exit 0
}
Write-Output "native check: FAILED ($($failed -join ', '))"
exit 1
