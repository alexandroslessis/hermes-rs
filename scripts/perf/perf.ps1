# Performance runs of a separate hermes-desktop instance: its own data folder, port 47810 and
# WebView2 profile (all in target\perf), talking to mock_llm.py, so the user's app, data and
# browser cache are never touched. See README.md here.
#   perf.ps1 -Do start [-Cold]     launch; report ms to the server and to the page's first call
#   perf.ps1 -Do mem               memory of the app's process tree
#   perf.ps1 -Do cpu -Seconds 20   CPU seconds used by the tree over an interval
#   perf.ps1 -Do stop              stop it (only this build's process and its children)
# The build: $env:HERMES_PERF_EXE, else target\claude\release, else target\release.
param([string]$Do = 'mem', [switch]$Cold, [int]$Seconds = 20)
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$root = Join-Path $repo 'target\perf'
$exe = if ($env:HERMES_PERF_EXE) { $env:HERMES_PERF_EXE }
       elseif (Test-Path (Join-Path $repo 'target\claude\release\hermes-desktop.exe')) { Join-Path $repo 'target\claude\release\hermes-desktop.exe' }
       else { Join-Path $repo 'target\release\hermes-desktop.exe' }
$log = Join-Path $root 'app.log'
New-Item -ItemType Directory -Force (Join-Path $root 'home') | Out-Null
$config = Join-Path $root 'home\config.json'
if (-not (Test-Path $config)) {
  '{"providers":[{"id":"mock","name":"Mock","base_url":"http://127.0.0.1:47900/v1"}],"provider":"mock","model":"mock","background_review":false,"checkpoints":false}' |
    Set-Content -Encoding ascii $config
}

function AppProc { Get-Process hermes-desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Select-Object -First 1 }
function Tree($rootPid) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, WorkingSetSize, PrivatePageCount, KernelModeTime, UserModeTime
  $ids = @($rootPid); $added = $true
  while ($added) { $added = $false; foreach ($p in $all) { if ($ids -contains $p.ParentProcessId -and -not ($ids -contains $p.ProcessId)) { $ids += $p.ProcessId; $added = $true } } }
  $all | Where-Object { $ids -contains $_.ProcessId }
}

switch ($Do) {
  'start' {
    if (AppProc) { 'already running'; exit 1 }
    if ($Cold) { Remove-Item -Recurse -Force (Join-Path $root 'wv2') -ErrorAction SilentlyContinue }
    $env:HERMES_RS_HOME = Join-Path $root 'home'
    $env:HERMES_DESKTOP_PORT = '47810'
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $root 'wv2'
    $env:RUST_LOG = 'info,hermes_desktop::wire=debug'
    # DevTools protocol for MY instance only (its own process env), to read the page's timings.
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9223 --disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows'
    Remove-Item $log -ErrorAction SilentlyContinue
    $t0 = [DateTime]::UtcNow
    Start-Process -FilePath $exe -RedirectStandardError $log -RedirectStandardOutput (Join-Path $root 'app.out') | Out-Null
    # Marks after launch: the server up and the page's first call (the app's log, stdout); the
    # page's navigation start and first contentful paint (its own clock, through DevTools).
    $out = Join-Path $root 'app.out'
    $deadline = $t0.AddSeconds(60); $serving = $null; $first = $null
    while ([DateTime]::UtcNow -lt $deadline -and -not $first) {
      Start-Sleep -Milliseconds 100
      if (-not (Test-Path $out)) { continue }
      foreach ($line in (Get-Content $out -ErrorAction SilentlyContinue)) {
        $line = $line -replace "\x1b\[[0-9;]*m", ''
        if ($line -notmatch '^(\S+Z)\s') { continue }
        $at = [DateTime]::Parse($Matches[1]).ToUniversalTime()
        if ($line -match 'serving Hermes' -and -not $serving) { $serving = $at }
        if ($line -match '← ' -and -not $first) { $first = $at }
      }
    }
    Start-Sleep -Seconds 3
    $node = Join-Path $repo 'tools\node\node.exe'
    $page = & $node (Join-Path $PSScriptRoot 'cdp.mjs') "({ origin: performance.timeOrigin, fcp: performance.getEntriesByType('paint').find(p => p.name === 'first-contentful-paint')?.startTime })" | ConvertFrom-Json
    $epoch = [DateTimeOffset]::new($t0).ToUnixTimeMilliseconds()
    $ms = { param($x) if ($x) { [int]($x - $t0).TotalMilliseconds } else { 'none' } }
    $nav = [int]($page.origin - $epoch)
    $fcp = if ($page.fcp) { [int]($page.origin + $page.fcp - $epoch) } else { 'none' }
    "cold=$([bool]$Cold): server up $(& $ms $serving) ms; navigation $nav ms; first call $(& $ms $first) ms; first paint $fcp ms"
  }
  'mem' {
    $p = AppProc; if (-not $p) { 'not running'; exit 1 }
    $t = Tree $p.Id
    $t | Sort-Object WorkingSetSize -Descending | ForEach-Object { '{0,-24} pid {1,6}  ws {2,6:N0} MB  private {3,6:N0} MB' -f $_.Name, $_.ProcessId, ($_.WorkingSetSize / 1MB), ($_.PrivatePageCount / 1MB) }
    'TOTAL  ws {0:N0} MB  private {1:N0} MB  ({2} processes)' -f (($t | Measure-Object WorkingSetSize -Sum).Sum / 1MB), (($t | Measure-Object PrivatePageCount -Sum).Sum / 1MB), @($t).Count
  }
  'cpu' {
    $p = AppProc; if (-not $p) { 'not running'; exit 1 }
    $cpu = { param($t) $h = @{}; foreach ($x in $t) { $h[$x.ProcessId] = @{ n = $x.Name; c = ($x.KernelModeTime + $x.UserModeTime) / 1e7 } }; $h }
    $a = & $cpu (Tree $p.Id); Start-Sleep -Seconds $Seconds; $b = & $cpu (Tree $p.Id)
    $total = 0
    foreach ($k in $b.Keys) { $d = $b[$k].c - $(if ($a[$k]) { $a[$k].c } else { 0 }); $total += $d; if ($d -gt 0.05) { '{0,-24} pid {1,6}  {2,6:N2} s' -f $b[$k].n, $k, $d } }
    'TOTAL {0:N2} CPU-s over {1}s ({2:N0}% of one core)' -f $total, $Seconds, ($total / $Seconds * 100)
  }
  'stop' {
    $p = AppProc; if (-not $p) { 'not running'; exit 0 }
    $ids = (Tree $p.Id).ProcessId
    Stop-Process -Id $p.Id -Force; Start-Sleep -Milliseconds 800
    Get-Process -Id $ids -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    'stopped'
  }
}
