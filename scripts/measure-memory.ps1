<#
.SYNOPSIS
  Memory of GitKwaken after it settles: working set and private bytes.
.DESCRIPTION
  Opens <Repo> (or the Welcome page when -Repo is empty) <Runs> times with a
  throwaway settings dir, waits -SettleMs, samples the process every 500 ms
  and reports the last sample before exit (median over runs), plus the peak
  working set. -Warp forces the D3D11 WARP rasterizer, which separates the
  GPU driver's user-mode memory from the app's own.
.EXAMPLE
  powershell -File scripts/measure-memory.ps1 target\release\gitkwaken.exe bench-data\demo
#>
param(
  [Parameter(Mandatory)] [string]$Exe,
  [string]$Repo = '',
  [string]$View = 'status',
  [int]$Runs = 3,
  [int]$SettleMs = 12000,
  [switch]$Warp
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$work = Join-Path ([IO.Path]::GetTempPath()) "gk-mem-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
Set-Content -Encoding ascii (Join-Path $work 's.script') "wait $SettleMs`nexit"
$env:GITKWAKEN_CONFIG_DIR = Join-Path $work 'cfg'
$env:GITKWAKEN_START_VIEW = $View
$env:GITKWAKEN_NO_UPDATE_CHECK = '1'
$env:GK_DX11_INPUT_SCRIPT = Join-Path $work 's.script'
if ($Warp) { $env:GK_DX11_WARP = '1' } else { Remove-Item Env:GK_DX11_WARP -ErrorAction SilentlyContinue }
$rows = foreach ($i in 1..$Runs) {
  $args = @()
  if ($Repo) { $args = @("`"$((Resolve-Path $Repo).Path)`"") }
  $p = if ($args) { Start-Process $Exe -ArgumentList $args -PassThru } else { Start-Process $Exe -PassThru }
  $ws = 0; $priv = 0; $peak = 0; $pws = 0
  $t0 = [Diagnostics.Stopwatch]::StartNew()
  while (-not $p.HasExited) {
    if ($pws -eq 0 -and $t0.ElapsedMilliseconds -gt ($SettleMs - 4000)) {
      # Task Manager's "Memory" column: private working set.
      $c = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$($p.Id)" -ErrorAction SilentlyContinue
      if ($c) { $pws = [double]$c.WorkingSetPrivate }
    }
    Start-Sleep -Milliseconds 500
    try {
      $p.Refresh()
      if ($p.WorkingSet64 -gt 0) { $ws = $p.WorkingSet64; $priv = $p.PrivateMemorySize64; $peak = $p.PeakWorkingSet64 }
    } catch {}
  }
  [pscustomobject]@{ ws_mb = [math]::Round($ws / 1MB, 1); private_mb = [math]::Round($priv / 1MB, 1); peak_ws_mb = [math]::Round($peak / 1MB, 1); private_ws_mb = [math]::Round($pws / 1MB, 1) }
}
$rows | Format-Table -AutoSize
function Med($v) { $s = $v | Sort-Object; $s[[int][math]::Floor($s.Count / 2)] }
"median: private working set (Task Manager) {4} MB, working set {0} MB, private bytes {1} MB, peak working set {2} MB{3}" -f (Med $rows.ws_mb), (Med $rows.private_mb), (Med $rows.peak_ws_mb), $(if ($Warp) { ' (WARP)' } else { '' }), (Med $rows.private_ws_mb)
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
