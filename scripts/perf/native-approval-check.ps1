# Phase 5 live check of the native UI (docs/native-ui/PHASES.md, Phase 5 "Done"): a real turn
# against the mock provider asks for an answer to a question and for an approval of a flagged
# command; our own hermes-native shows both panels, answers them through the composer's model
# (`--dev-answer`), and the turn goes on to its end with what was answered. Run twice: the command
# allowed, then denied. Everything is ours: a synthetic data folder (target\native-live, seeded by
# the sidebar's seed test), mock_llm.py on its own port (47902), our build in target\claude. The
# approved command is `chmod 777 live-check-note.txt` in a folder of the synthetic data: PowerShell
# does not know it, so allowing it changes nothing. Nothing is flashed or notified (a run that
# answers by itself makes no notification, and the folder's native-ui.json has them off). A window
# opens for a few seconds; no pointer or keyboard input is used.
#   powershell -NoProfile -File scripts/perf/native-approval-check.ps1 [-Exe <hermes-native.exe>] [-Keep] [-NoBuild]
# Prints "native-approval-check: ok" or the reason it failed (exit 1).
param([string]$Exe = '', [switch]$Keep, [switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$home_ = Join-Path $repo 'target\native-live'
$port = 47902
$failed = $null
$mock = $null

function Fail($why) { if (-not $script:failed) { $script:failed = $why } }

try {
  # 0. The app with the element dump (`--dump` needs `--features dev`), in our own target folder.
  $env:CARGO_TARGET_DIR = 'target/claude'
  if (-not $Exe) { $Exe = Join-Path $repo 'target\claude\debug\hermes-native.exe' }
  if (-not $NoBuild) {
    Push-Location $repo
    $build = & cmd /c "cargo build -p hermes-native --features dev --bins 2>&1" | Out-String
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { throw "build failed: $build" }
  }
  if (-not (Test-Path $Exe)) { throw "no build at $Exe" }
  $Exe = (Resolve-Path $Exe).Path
  if ($Exe -notlike (Join-Path $repo 'target\claude\*')) { throw 'the build must be under target\claude' }

  # 1. A synthetic data folder of our own (never %APPDATA%), with notifications off.
  if (Test-Path $home_) { Remove-Item -Recurse -Force $home_ }
  New-Item -ItemType Directory $home_ | Out-Null
  $env:HERMES_NATIVE_SEED = $home_
  Push-Location $repo
  $seed = & cmd /c "cargo test -q -p hermes-native --lib seed_synthetic_data_folder -- --ignored --nocapture 2>&1" | Out-String
  Pop-Location
  Remove-Item Env:HERMES_NATIVE_SEED
  if ($seed -notmatch 'seeded \d+ threads') { throw "seeding failed: $seed" }
  '{"notifications": false}' | Set-Content -Path (Join-Path $home_ 'native-ui.json') -Encoding ascii

  # 2. The mock provider (the interpreter itself, so stopping it stops it).
  $python = (& "$env:USERPROFILE\.local\bin\uv.exe" run --no-project python -c "import sys;print(sys.executable)").Trim()
  $mock = Start-Process -FilePath $python -PassThru -WindowStyle Hidden -ArgumentList @(
    (Join-Path $repo 'scripts\perf\mock_llm.py'), '--port', $port, '--tokens', '60', '--tps', '400')
  $up = $false
  for ($i = 0; $i -lt 50 -and -not $up; $i++) {
    try { Invoke-RestMethod "http://127.0.0.1:$port/v1/models" -TimeoutSec 1 | Out-Null; $up = $true } catch { Start-Sleep -Milliseconds 200 }
  }
  if (-not $up) { throw 'the mock provider did not start' }

  # 3. Two turns of our own app: the command allowed once, then denied.
  foreach ($choice in @('once', 'deny')) {
    $dump = Join-Path $home_ "layout-$choice.txt"
    $err = Join-Path $home_ "native-stderr-$choice.txt"
    $env:HERMES_RS_HOME = $home_
    $app = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $err -ArgumentList @(
      '--dev', '--dev-provider', "http://127.0.0.1:$port/v1", '--dev-send', '"please ask for a colour and run the command"',
      '--dev-answer', $choice, '--dump', $dump, '--quit-after', '150')
    Remove-Item Env:HERMES_RS_HOME
    if (-not $app.WaitForExit(170000)) { Stop-Process -Id $app.Id -Force; throw "[$choice] the app did not quit" }
    $log = Get-Content $err -Raw
    $lines = ($log -split "`r?`n") | Where-Object { $_ -like 'dev-send:*' }
    $lines | ForEach-Object { "  [$choice] $_" }

    # 4. What it saw: both questions asked and answered, the panels on screen, the turn's end.
    $failure = $lines | Where-Object { $_ -like 'dev-send: failed*' }
    if ($failure) { Fail "[$choice] $failure"; continue }
    if (-not ($lines | Where-Object { $_ -like 'dev-send: asked question:*Which colour*' })) { Fail "[$choice] the question was not asked" }
    if (-not ($lines | Where-Object { $_ -like 'dev-send: asked approval:*chmod 777*' })) { Fail "[$choice] the approval was not asked" }
    if (-not ($lines | Where-Object { $_ -like 'dev-send: answering approval*' })) { Fail "[$choice] the approval was not answered" }
    $panels = @{ 'question' = @('question-panel', 'ask-choice-0', 'ask-send'); 'approval' = @('approval-panel', 'approval-command', 'approval-once', 'approval-deny') }
    foreach ($kind in $panels.Keys) {
      $file = "$dump.$kind"
      if (-not (Test-Path $file)) { Fail "[$choice] no element dump of the $kind panel"; continue }
      $text = Get-Content $file -Raw
      foreach ($name in $panels[$kind]) {
        if ($text -notmatch [regex]::Escape($name)) { Fail "[$choice] the $kind panel's dump has no '$name'" }
      }
    }
    $reply = $lines | Where-Object { $_ -like 'dev-send: reply *' } | Select-Object -First 1
    $stored = $lines | Where-Object { $_ -like 'dev-send: stored *' } | Select-Object -First 1
    if (-not $reply) { Fail "[$choice] no streamed reply"; continue }
    if ($reply -notmatch 'ANSWERS colour=blue') { Fail "[$choice] the question's answer did not reach the model: $reply" }
    $verdict = if ($choice -eq 'once') { 'command=ran' } else { 'command=denied' }
    if ($reply -notmatch $verdict) { Fail "[$choice] the command's outcome is not '$verdict': $reply" }
    if (-not $stored) { Fail "[$choice] the reply was not read back" }
    elseif ($stored.Substring(17) -ne $reply.Substring(16)) { Fail "[$choice] stored differs: $stored" }
  }
} catch {
  Fail $_.Exception.Message
} finally {
  if ($mock -and -not $mock.HasExited) { Stop-Process -Id $mock.Id -Force }
  if (-not $Keep -and (Test-Path $home_)) {
    Start-Sleep -Milliseconds 500
    Remove-Item -Recurse -Force $home_ -ErrorAction SilentlyContinue
  }
}
if ($failed) { "native-approval-check: FAILED: $failed"; exit 1 }
'native-approval-check: ok'
