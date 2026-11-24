# Phase 4 live check of the native UI (docs/native-ui/PHASES.md, Phase 4 "Done"): a real turn
# against the mock provider streams into our own hermes-native and reads back the same from the
# store. Everything is ours: a synthetic data folder (target\native-live, seeded by the sidebar's
# seed test), mock_llm.py on its own port (47901), our build in target\claude. The app drives the
# turn itself (`--dev --dev-send`): no pointer or keyboard input. A window opens for a few seconds.
#   powershell -NoProfile -File scripts/perf/native-thread-check.ps1 [-Exe <hermes-native.exe>] [-Keep]
# Prints "native-thread-check: ok" or the reason it failed (exit 1).
param([string]$Exe = '', [switch]$Keep)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $Exe) { $Exe = Join-Path $repo 'target\claude\debug\hermes-native.exe' }
if (-not (Test-Path $Exe)) { "no build at $Exe (cargo build -p hermes-native --bins, CARGO_TARGET_DIR=target/claude)"; exit 1 }
$Exe = (Resolve-Path $Exe).Path
if ($Exe -notlike (Join-Path $repo 'target\claude\*')) { "the build must be under target\claude"; exit 1 }
$home_ = Join-Path $repo 'target\native-live'
$port = 47901
$failed = $null
$mock = $null

function Fail($why) { if (-not $script:failed) { $script:failed = $why } }

try {
  # 1. A synthetic data folder of our own (never %APPDATA%).
  if (Test-Path $home_) { Remove-Item -Recurse -Force $home_ }
  New-Item -ItemType Directory $home_ | Out-Null
  $env:CARGO_TARGET_DIR = 'target/claude'
  $env:HERMES_NATIVE_SEED = $home_
  Push-Location $repo
  $seed = & cargo test -q -p hermes-native --lib seed_synthetic_data_folder -- --ignored --nocapture 2>&1 | Out-String
  Pop-Location
  Remove-Item Env:HERMES_NATIVE_SEED
  if ($seed -notmatch 'seeded \d+ threads') { throw "seeding failed: $seed" }

  # 2. The mock provider (the interpreter itself, so stopping it stops it).
  $python = (& "$env:USERPROFILE\.local\bin\uv.exe" run --no-project python -c "import sys;print(sys.executable)").Trim()
  $mock = Start-Process -FilePath $python -PassThru -WindowStyle Hidden -ArgumentList @(
    (Join-Path $repo 'scripts\perf\mock_llm.py'), '--port', $port, '--tokens', '300', '--tps', '400')
  $up = $false
  for ($i = 0; $i -lt 50 -and -not $up; $i++) {
    try { Invoke-RestMethod "http://127.0.0.1:$port/v1/models" -TimeoutSec 1 | Out-Null; $up = $true } catch { Start-Sleep -Milliseconds 200 }
  }
  if (-not $up) { throw 'the mock provider did not start' }

  # 3. Our app drives one turn and quits.
  $err = Join-Path $home_ 'native-stderr.txt'
  $env:HERMES_RS_HOME = $home_
  $app = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $err -ArgumentList @(
    '--dev', '--dev-provider', "http://127.0.0.1:$port/v1", '--dev-send', 'hello from the live check', '--quit-after', '150')
  Remove-Item Env:HERMES_RS_HOME
  if (-not $app.WaitForExit(170000)) { Stop-Process -Id $app.Id -Force; throw 'the app did not quit' }
  $log = Get-Content $err -Raw
  $lines = ($log -split "`r?`n") | Where-Object { $_ -like 'dev-send:*' }
  $lines | ForEach-Object { "  $_" }

  # 4. What it saw: a streamed reply with the mock's text, and the same reply read back.
  $failure = $lines | Where-Object { $_ -like 'dev-send: failed*' }
  if ($failure) { Fail $failure }
  $reply = $lines | Where-Object { $_ -like 'dev-send: reply *' } | Select-Object -First 1
  $stored = $lines | Where-Object { $_ -like 'dev-send: stored *' } | Select-Object -First 1
  if (-not $reply) { Fail 'no streamed reply' }
  elseif ($reply -notmatch 'Section 1') { Fail "the reply is not the mock's: $reply" }
  if (-not $stored) { Fail 'the reply was not read back' }
  elseif ($stored.Substring(17) -ne $reply.Substring(16)) { Fail "stored differs: $stored" }
} catch {
  Fail $_.Exception.Message
} finally {
  if ($mock -and -not $mock.HasExited) { Stop-Process -Id $mock.Id -Force }
  if (-not $Keep -and (Test-Path $home_)) {
    Start-Sleep -Milliseconds 500
    Remove-Item -Recurse -Force $home_ -ErrorAction SilentlyContinue
  }
}
if ($failed) { "native-thread-check: FAILED: $failed"; exit 1 }
'native-thread-check: ok'
