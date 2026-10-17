# Starts GitKwaken (optionally with a repository), waits, captures the main
# window to a PNG and closes the app. Used to review the UI visually.
#
#   powershell -File scripts/screenshot.ps1 -Exe target\release\gitkwaken.exe -Repo D:\repo -Out shot.png [-Wait 4] [-Keys "^2"]

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Repo = "",
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$Wait = 4,
    [string]$Keys = "",
    [int]$Width = 1400,
    [int]$Height = 900,
    [switch]$KeepOpen
)

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[W]::SetProcessDPIAware() | Out-Null

# Isolated settings so screenshots don't touch the user's configuration.
$cfg = Join-Path $env:TEMP "gitkwaken-shot-config"
New-Item -ItemType Directory -Force $cfg | Out-Null
$env:GITKWAKEN_CONFIG_DIR = $cfg

$args = @()
if ($Repo -ne "") { $args += $Repo }
$p = Start-Process -FilePath $Exe -ArgumentList $args -PassThru
$deadline = (Get-Date).AddSeconds(20)
while ($p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200; $p.Refresh() }
if ($p.MainWindowHandle -eq 0) { Write-Error "no window"; exit 1 }
$h = $p.MainWindowHandle
[W]::MoveWindow($h, 40, 40, $Width, $Height, $true) | Out-Null
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Seconds $Wait
if ($Keys -ne "") {
    foreach ($k in $Keys.Split("|")) {
        [System.Windows.Forms.SendKeys]::SendWait($k)
        Start-Sleep -Milliseconds 700
    }
    Start-Sleep -Seconds 1
}
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
# PrintWindow with PW_RENDERFULLCONTENT captures only this window's own
# content (also when it is covered), never other applications on screen.
$hdc = $g.GetHdc()
$ok = [W]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
if (-not $ok) { Write-Error "PrintWindow failed"; Stop-Process -Id $p.Id -Force; exit 1 }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
if (-not $KeepOpen) { Stop-Process -Id $p.Id -Force }
Write-Output "saved $Out ($w x $hh)"
