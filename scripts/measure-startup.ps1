<#
.SYNOPSIS
  Measures the "first frame from cache" budget end to end: process creation
  to the first frame that draws history rows, with the snapshot cache warm.
.DESCRIPTION
  Opens <Repo> in the History view <Runs>+1 times (the first run fills the
  cache and is reported separately as the cold start). Each run uses its own
  throwaway settings dir and GITKWAKEN_STARTUP_LOG; the app writes
  milestones (first_frame, history_rows[_from_cache]) with unix-ms stamps,
  which are compared with the process creation time.
.EXAMPLE
  powershell -File scripts/measure-startup.ps1 target\lead\release\gitkwaken.exe bench-data\gitgit 5
#>
param(
  [Parameter(Mandatory)] [string]$Exe,
  [Parameter(Mandatory)] [string]$Repo,
  [int]$Runs = 5
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$Repo = (Resolve-Path $Repo).Path
if (-not (Test-Path (Join-Path $Repo '.git'))) { throw "$Repo is not a repository root" }
$work = Join-Path ([IO.Path]::GetTempPath()) "gk-startup-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$script = Join-Path $work 'exit.script'
Set-Content -Encoding ascii $script "wait 2500`nexit"

$env:GITKWAKEN_CONFIG_DIR = Join-Path $work 'config'
$env:GITKWAKEN_START_VIEW = 'history'
$env:GITKWAKEN_NO_UPDATE_CHECK = '1'
$env:GK_DX11_INPUT_SCRIPT = $script

function Measure-Run([int]$i) {
  $log = Join-Path $work "run$i.log"
  Remove-Item $log -ErrorAction SilentlyContinue
  $env:GITKWAKEN_STARTUP_LOG = $log
  $p = Start-Process -FilePath $Exe -ArgumentList "`"$Repo`"" -PassThru
  $created = [DateTimeOffset]::new($p.StartTime).ToUnixTimeMilliseconds()
  if (-not $p.WaitForExit(30000)) { $p.Kill(); throw "run $i did not exit" }
  $marks = @{}
  foreach ($line in Get-Content $log) {
    $f = $line -split ' '
    $marks[$f[0]] = [pscustomobject]@{ main = [double]$f[1]; total = [double]$f[2] - $created }
  }
  $marks
}

$cold = Measure-Run 0
$rows = @()
for ($i = 1; $i -le $Runs; $i++) {
  $m = Measure-Run $i
  $hit = $m['history_rows_from_cache']
  if (-not $hit) { $hit = $m['history_rows']; $label = 'live' } else { $label = 'cache' }
  $rows += [pscustomobject]@{
    run = $i; source = $label
    first_frame_ms = $m['first_frame'].total
    history_rows_ms = $hit.total
    from_main_ms = $hit.main
  }
}
"cold (no cache): first frame {0} ms, history rows {1} ms" -f $cold['first_frame'].total, `
  ($cold['history_rows'], $cold['history_rows_from_cache'] | Where-Object { $_ } | Select-Object -First 1).total
$rows | Format-Table -AutoSize
$sorted = $rows.history_rows_ms | Sort-Object
"median process start -> history rows from cache: {0} ms (best {1}, worst {2})" -f `
  $sorted[[int][math]::Floor($sorted.Count / 2)], $sorted[0], $sorted[-1]
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
