<#
.SYNOPSIS
  Builds a GitKwaken release: portable zip, update.json and (if Inno Setup is installed) installer.

.DESCRIPTION
  Outputs (in <repo>\dist):
    GitKwaken-<ver>-x64\              staging folder (gitkwaken.exe, LICENSE-*, README.md)
    GitKwaken-<ver>-x64.zip           portable build; also what the self-updater downloads
    GitKwaken-<ver>-x64-setup.exe     installer (only when ISCC.exe is found)
    update.json                       self-update channel manifest (see crates/gk-update)
    SHA256SUMS.txt

.PARAMETER Version
  Release version. Default: package version of gk-app from `cargo metadata`.
.PARAMETER DownloadBaseUrl
  Base URL the zip will be published under (written into update.json).
.PARAMETER Notes
  Release notes for update.json.
.PARAMETER SkipBuild
  Package an existing target\release\gitkwaken.exe without rebuilding.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\build-release.ps1
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$DownloadBaseUrl,
    [string]$Notes,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Dist = Join-Path $Root 'dist'
Push-Location $Root
try {
    # ---------------------------------------------------------------- metadata
    $meta = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    if (-not $Version) {
        $Version = ($meta.packages | Where-Object { $_.name -eq 'gk-app' }).version
    }
    $Version = $Version.TrimStart('v')
    $TargetDir = $meta.target_directory   # honours CARGO_TARGET_DIR
    if (-not $DownloadBaseUrl) {
        $DownloadBaseUrl = "https://github.com/OWNER/gitkwaken/releases/download/v$Version"
    }
    if (-not $Notes) {
        $Notes = "GitKwaken $Version. See https://github.com/OWNER/gitkwaken/releases/tag/v$Version"
    }
    $Name = "GitKwaken-$Version-x64"
    Write-Host "==> GitKwaken $Version (target dir: $TargetDir)"

    # ---------------------------------------------------------------- build
    if (-not $SkipBuild) {
        Write-Host '==> cargo build --release -p gk-app'
        cargo build --release --locked -p gk-app
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    $Exe = Join-Path $TargetDir 'release\gitkwaken.exe'
    if (-not (Test-Path $Exe)) { throw "missing $Exe" }

    # ---------------------------------------------------------------- stage
    $Stage = Join-Path $Dist $Name
    if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
    New-Item -ItemType Directory -Force $Stage | Out-Null
    Copy-Item $Exe $Stage
    foreach ($f in 'LICENSE-MIT', 'LICENSE-APACHE', 'README.md') {
        $p = Join-Path $Root $f
        if (Test-Path $p) { Copy-Item $p $Stage } else { Write-Warning "$f not found, skipped" }
    }

    # ---------------------------------------------------------------- zip
    $Zip = Join-Path $Dist "$Name.zip"
    if (Test-Path $Zip) { Remove-Item -Force $Zip }
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    # Deflate, top-level folder GitKwaken-<ver>-x64/. Entries are added by hand because
    # ZipFile.CreateFromDirectory on Windows PowerShell 5.1 writes '\' separators.
    $zipStream = [System.IO.File]::Open($Zip, [System.IO.FileMode]::CreateNew)
    $archive = New-Object System.IO.Compression.ZipArchive($zipStream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in Get-ChildItem -Path $Stage -Recurse -File) {
            $rel = $file.FullName.Substring($Stage.Length).TrimStart('\', '/') -replace '\\', '/'
            [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $archive, $file.FullName, "$Name/$rel", [System.IO.Compression.CompressionLevel]::Optimal)
        }
    } finally {
        $archive.Dispose()
        $zipStream.Dispose()
    }
    $ZipHash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
    $ZipSize = (Get-Item $Zip).Length
    Write-Host ("==> {0} ({1:N0} bytes, {2:N2} MB)" -f $Zip, $ZipSize, ($ZipSize / 1MB))

    # ---------------------------------------------------------------- update.json
    $manifest = [ordered]@{
        version   = $Version
        notes     = $Notes
        url       = "$DownloadBaseUrl/$Name.zip"
        sha256    = $ZipHash
        published = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')
    }
    $json = $manifest | ConvertTo-Json
    # UTF-8 *without* BOM: serde_json rejects a BOM.
    [System.IO.File]::WriteAllText((Join-Path $Dist 'update.json'), $json + "`n", (New-Object System.Text.UTF8Encoding $false))
    Write-Host "==> dist\update.json"

    # ---------------------------------------------------------------- installer
    $Iscc = $null
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { $Iscc = $cmd.Source }
    if (-not $Iscc) {
        $candidates = @(
            (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
            (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
            (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
        )
        $Iscc = $candidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    }
    $Setup = $null
    if ($Iscc) {
        Write-Host "==> Inno Setup: $Iscc"
        & $Iscc /Qp "/DAppVersion=$Version" "/DSourceDir=$Stage" "/DOutputDir=$Dist" (Join-Path $PSScriptRoot 'gitkwaken.iss')
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
        $Setup = Join-Path $Dist "$Name-setup.exe"
        Write-Host ("==> {0} ({1:N2} MB)" -f $Setup, ((Get-Item $Setup).Length / 1MB))
    } else {
        Write-Warning 'Inno Setup (ISCC.exe) not found - installer skipped. Install with: winget install JRSoftware.InnoSetup'
    }

    # ---------------------------------------------------------------- checksums
    $sums = foreach ($f in @($Zip, $Setup) | Where-Object { $_ }) {
        '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 $f).Hash.ToLowerInvariant(), (Split-Path -Leaf $f)
    }
    [System.IO.File]::WriteAllText((Join-Path $Dist 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding $false))

    Write-Host '==> done'
}
finally {
    Pop-Location
}
