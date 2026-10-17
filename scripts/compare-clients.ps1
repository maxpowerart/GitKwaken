<#
.SYNOPSIS
  Stopwatch comparison of repository load time: GitKwaken vs other Git clients vs git CLI.

.DESCRIPTION
  For each client and repository the client is started with the repository path
  as argument and observed for -ObserveSec seconds, sampling the CPU time of
  its process tree (including git.exe children) every ~100 ms:
    window: time until the main window exists and has a title;
    loaded: time until 97% of the startup CPU work is done, where startup work
            is the CPU time above the steady background rate measured over the
            last 3 s (some clients never go fully idle). That approximates
            "repository shown and all background loading done".
  A window-only screenshot (PrintWindow, never the rest of the desktop) is
  saved per run to bench-data\clients\shots as proof the repository was shown.
  Each measurement is repeated -Runs times; the median is reported. The git
  CLI rows time `git status` and `git log --graph` (output discarded).

  Clients are only measured when their executable is found:
    GitKwaken  -GitKwaken <exe> (default: target\release\gitkwaken.exe)
    SourceGit  -SourceGit <exe>; scripts\compare-clients.ps1 -FetchSourceGit
               downloads the portable zip (winget id sourcegit-scm.SourceGit),
               checks its SHA-256 and unpacks it to bench-data\clients\sourcegit
               with a "data" folder so it runs in portable mode.
    Fork       -Fork <exe> (Fork.exe after a per-user install; not automated).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\compare-clients.ps1 -FetchSourceGit
  powershell -ExecutionPolicy Bypass -File scripts\compare-clients.ps1 -Repos bench-data\gitgit,bench-data\r200k -Runs 3
#>
[CmdletBinding()]
param(
    [string[]]$Repos = @('bench-data\gitgit', 'bench-data\r200k'),
    [int]$Runs = 3,
    [string]$GitKwaken,
    [string]$SourceGit,
    [string]$Fork,
    [switch]$FetchSourceGit,
    # Each launch is observed this long (must cover the full load of the slowest client).
    [int]$ObserveSec = 15,
    [string]$OutFile
)

$ErrorActionPreference = 'Stop'
# `powershell -File` passes "a,b" as one string.
$Repos = @($Repos | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $Root

$SourceGitVersion = '2026.21'
$SourceGitSha256 = '954a458ad3d8c1b537046e051f8427cfebacdf06977510b4b0185b44eb1133eb'

if ($FetchSourceGit) {
    $dir = Join-Path $Root 'bench-data\clients\sourcegit'
    $zip = Join-Path $Root "bench-data\clients\sourcegit_$SourceGitVersion.win-x64.zip"
    New-Item -ItemType Directory -Force (Split-Path $zip) | Out-Null
    if (-not (Test-Path $zip)) {
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -UseBasicParsing -OutFile $zip `
            "https://github.com/sourcegit-scm/sourcegit/releases/download/v$SourceGitVersion/sourcegit_$SourceGitVersion.win-x64.zip"
    }
    $h = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($h -ne $SourceGitSha256) { Remove-Item $zip; throw "SourceGit zip SHA-256 mismatch: $h" }
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    Expand-Archive $zip $dir
    # A "data" folder next to the exe switches SourceGit to portable mode.
    $exe = Get-ChildItem $dir -Recurse -Filter 'SourceGit.exe' | Select-Object -First 1
    New-Item -ItemType Directory -Force (Join-Path $exe.DirectoryName 'data') | Out-Null
    Write-Host "SourceGit $SourceGitVersion -> $($exe.FullName)"
}

if (-not $GitKwaken) { $GitKwaken = Join-Path $Root 'target\release\gitkwaken.exe' }
if (-not $SourceGit) {
    $c = Get-ChildItem (Join-Path $Root 'bench-data\clients\sourcegit') -Recurse -Filter 'SourceGit.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($c) { $SourceGit = $c.FullName }
}
if (-not $Fork) {
    $c = Join-Path $env:LOCALAPPDATA 'Fork\Fork.exe'
    if (Test-Path $c) { $Fork = $c }
}

# The client runs in a Job Object: its CPU accounting covers every process of
# the tree (including git.exe children that already exited), is cheap to query
# (no WMI), and terminating the job reliably closes the whole tree.
Add-Type @'
using System;
using System.Runtime.InteropServices;
public sealed class GkJob : IDisposable {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern IntPtr CreateJobObject(IntPtr a, string n);
    [DllImport("kernel32.dll")] static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
    [DllImport("kernel32.dll")] static extern bool TerminateJobObject(IntPtr j, uint c);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool QueryInformationJobObject(IntPtr j, int cls, out BASIC info, int len, IntPtr ret);
    [StructLayout(LayoutKind.Sequential)] struct BASIC {
        public long TotalUserTime, TotalKernelTime, ThisPeriodTotalUserTime, ThisPeriodTotalKernelTime;
        public uint TotalPageFaultCount, TotalProcesses, ActiveProcesses, TotalTerminatedProcesses;
    }
    IntPtr h;
    public GkJob(IntPtr process) {
        h = CreateJobObject(IntPtr.Zero, null);
        if (!AssignProcessToJobObject(h, process)) throw new Exception("AssignProcessToJobObject failed");
    }
    // Total CPU (user + kernel) of all processes ever in the job, in ms.
    public double CpuMs() {
        BASIC b;
        if (!QueryInformationJobObject(h, 1, out b, Marshal.SizeOf(typeof(BASIC)), IntPtr.Zero)) return -1;
        return (b.TotalUserTime + b.TotalKernelTime) / 10000.0;
    }
    public void Kill() { TerminateJobObject(h, 1); }
    public void Dispose() { if (h != IntPtr.Zero) { CloseHandle(h); h = IntPtr.Zero; } }
}
'@

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public static class GkWinShot {
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    // Captures only the given window (PW_RENDERFULLCONTENT also works for
    // DirectX content and covered windows); never the rest of the desktop.
    public static void Save(IntPtr hwnd, string path) {
        RECT r; if (!GetWindowRect(hwnd, out r)) return;
        int w = Math.Max(1, r.R - r.L), h = Math.Max(1, r.B - r.T);
        using (var bmp = new Bitmap(w, h))
        using (var g = Graphics.FromImage(bmp)) {
            IntPtr dc = g.GetHdc();
            PrintWindow(hwnd, dc, 2);
            g.ReleaseHdc(dc);
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
}
'@

function Save-Window([IntPtr]$Hwnd, [string]$Path) {
    New-Item -ItemType Directory -Force (Split-Path $Path) | Out-Null
    [GkWinShot]::Save($Hwnd, $Path)
}

# Starts $Exe with $Arguments and returns @{ window = ms; idle = ms }; saves a
# screenshot at the idle point to $Shot (proof the repository was shown).
function Measure-Launch([string]$Exe, [string[]]$Arguments, [hashtable]$Env, [string]$Shot) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path $Exe
    foreach ($k in $Env.Keys) { $psi.EnvironmentVariables[$k] = $Env[$k] }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $job = New-Object GkJob $p.Handle
    $window = $null
    $samples = New-Object System.Collections.Generic.List[double[]]
    try {
        while ($sw.Elapsed.TotalSeconds -lt $ObserveSec) {
            Start-Sleep -Milliseconds 100
            $p.Refresh()
            if ($p.HasExited) { throw "$Exe exited early ($($p.ExitCode))" }
            if (-not $window -and $p.MainWindowHandle -ne 0 -and $p.MainWindowTitle) { $window = $sw.Elapsed.TotalMilliseconds }
            $cpu = $job.CpuMs()
            if ($cpu -ge 0) { $samples.Add(@($sw.Elapsed.TotalMilliseconds, $cpu)) }
        }
        if ($Shot -and $window) { $p.Refresh(); Save-Window $p.MainWindowHandle $Shot }
    } finally {
        $job.Kill()
        $job.Dispose()
        $null = $p.WaitForExit(5000)
    }
    if (-not $window) { throw "$Exe showed no window within $ObserveSec s" }
    # Steady background CPU rate (ms CPU per ms) over the last 3 s; "loaded" is
    # the first moment 97% of the startup work above that rate was done.
    $T = $samples[$samples.Count - 1][0]; $cT = $samples[$samples.Count - 1][1]
    $ref = $samples | Where-Object { $_[0] -ge $T - 3000 } | Select-Object -First 1
    $rate = if ($T -gt $ref[0]) { ($cT - $ref[1]) / ($T - $ref[0]) } else { 0 }
    $total = $cT - $rate * $T
    $loaded = $T
    foreach ($s in $samples) {
        if ($s[0] -ge $window -and ($s[1] - $rate * $s[0]) -ge 0.97 * $total) { $loaded = $s[0]; break }
    }
    if ($env:GK_COMPARE_DEBUG) {
        Write-Host ("  [{0}] window={1:N0} loaded={2:N0} startupCpu={3:N0}ms background={4:P1} samples={5}" -f (Split-Path -Leaf $Exe), $window, $loaded, $total, $rate, $samples.Count)
    }
    return @{ window = [math]::Round($window); idle = [math]::Round($loaded); cpu = [math]::Round($total); bg = $rate }
}

function Median([double[]]$v) { $s = $v | Sort-Object; return $s[[int][math]::Floor($s.Count / 2)] }

function Time-Git([string]$Repo, [string[]]$GitArgs) {
    $ms = foreach ($i in 1..$Runs) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        & git -C $Repo @GitArgs > $null 2>&1
        $sw.Elapsed.TotalMilliseconds
    }
    return [math]::Round((Median $ms))
}

$clients = [ordered]@{}
if (Test-Path $GitKwaken) {
    $clients['GitKwaken'] = @{ exe = (Resolve-Path $GitKwaken).Path; env = @{ GITKWAKEN_NO_UPDATE_CHECK = '1'; GITKWAKEN_CONFIG_DIR = (Join-Path $env:TEMP 'gk-compare-cfg'); GITKWAKEN_START_VIEW = 'history' } }
}
if ($SourceGit) { $clients['SourceGit'] = @{ exe = $SourceGit; env = @{} } }
if ($Fork) { $clients['Fork'] = @{ exe = $Fork; env = @{} } }

$rows = @()
foreach ($repo in $Repos) {
    $abs = (Resolve-Path $repo).Path
    # Safety: never run git (or a client) on a folder that is not its own repo,
    # or git would walk up into the project repository.
    if (-not (Test-Path (Join-Path $abs '.git'))) { throw "$abs is not the root of a git repository" }
    $commits = (& git -C $abs rev-list --all --count)
    $files = (& git -C $abs ls-files | Measure-Object -Line).Lines
    $name = Split-Path $abs -Leaf
    foreach ($c in $clients.Keys) {
        $res = @()
        foreach ($i in 1..$Runs) {
            try { $res += Measure-Launch $clients[$c].exe @($abs) $clients[$c].env (Join-Path $Root "bench-data\clients\shots\$c-$name-$i.png") }
            catch { Write-Warning "$c on ${name}: $_" }
        }
        if ($res.Count) {
            $rows += [pscustomobject]@{ repo = $name; commits = $commits; files = $files; client = $c
                window_ms = Median ($res | ForEach-Object { $_.window }); loaded_ms = Median ($res | ForEach-Object { $_.idle })
                startup_cpu_ms = Median ($res | ForEach-Object { $_.cpu }); idle_cpu_pct = [math]::Round(100 * (Median ($res | ForEach-Object { $_.bg })), 1) }
            Write-Host ("{0,-10} {1,-10} window {2,6} ms  loaded {3,6} ms  startup CPU {4,6} ms  idle CPU {5}%" -f $name, $c, $rows[-1].window_ms, $rows[-1].loaded_ms, $rows[-1].startup_cpu_ms, $rows[-1].idle_cpu_pct)
        }
    }
    $st = Time-Git $abs @('status', '--porcelain=v2', '-uall')
    $lg = Time-Git $abs @('log', '--graph', '--all', '--format=%h %s')
    $lg1 = Time-Git $abs @('log', '--graph', '--all', '--format=%h %s', '-n', '200')
    $rows += [pscustomobject]@{ repo = $name; commits = $commits; files = $files; client = 'git status'; window_ms = $null; loaded_ms = $st }
    $rows += [pscustomobject]@{ repo = $name; commits = $commits; files = $files; client = 'git log --graph --all'; window_ms = $null; loaded_ms = $lg }
    $rows += [pscustomobject]@{ repo = $name; commits = $commits; files = $files; client = 'git log --graph --all -n 200'; window_ms = $null; loaded_ms = $lg1 }
    Write-Host ("{0,-10} git status {1} ms, git log --graph --all {2} ms, first 200 {3} ms" -f $name, $st, $lg, $lg1)
}

$rows | Format-Table -AutoSize | Out-String | Write-Host
if ($OutFile) { $rows | ConvertTo-Json | Set-Content -Encoding utf8 $OutFile }
