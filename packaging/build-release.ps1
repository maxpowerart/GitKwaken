<#
.SYNOPSIS
  Builds a GitKwaken release: portable zip, update.json and (if Inno Setup is installed) installer.

.DESCRIPTION
  Outputs (in <repo>\dist):
    GitKwaken-<ver>-x64\              staging folder (gitkwaken.exe, LICENSE-*, README.md)
    GitKwaken-<ver>-x64.zip           portable build; also what the self-updater downloads
    GitKwaken-<ver>-x64-setup.exe     installer (only when ISCC.exe is found)
    GitKwaken-<ver>-x64.msi           per-user MSI (only when the WiX CLI `wix` is found;
                                      dotnet tool install --global wix --version 6.0.2)
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
.PARAMETER BundleGit
  Also bundle MinGit: downloads the busybox-free 64-bit MinGit zip from the
  git-for-windows GitHub release (version >= 2.45), verifies its SHA-256 against
  the checksum table in that release's notes, and unpacks it to <stage>\git\.
  It ships in the zip and the installer installs it to {app}\git. gk-cli's
  find_git prefers <exe_dir>\git\cmd\git.exe when present. Downloads are
  cached in <dist>\cache.
.PARAMETER MinGitVersion
  Git for Windows version for -BundleGit, e.g. 2.47.1. Default: latest release.
.PARAMETER DistDir
  Output folder. Default: <repo>\dist.
.PARAMETER SkipMsi
  Do not build the MSI even when WiX is installed.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\build-release.ps1
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\build-release.ps1 -BundleGit -MinGitVersion 2.47.1
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$DownloadBaseUrl,
    [string]$Notes,
    # GitHub "owner/name"; defaults to the origin remote. Drives download URLs
    # and the update channel compiled into the executable.
    [string]$Repo,
    [switch]$SkipBuild,
    [switch]$BundleGit,
    [string]$MinGitVersion,
    [string]$DistDir,
    [switch]$SkipMsi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Plain text -> minimal RTF (the WiX license dialog only accepts RTF).
function ConvertTo-Rtf([string]$Text) {
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Segoe UI;}}\viewkind4\uc1\pard\f0\fs18 ')
    foreach ($ch in ($Text -replace "`r`n", "`n").ToCharArray()) {
        $c = [int]$ch
        if ($ch -eq '\' -or $ch -eq '{' -or $ch -eq '}') { [void]$sb.Append('\').Append($ch) }
        elseif ($ch -eq "`n") { [void]$sb.Append("\par`r`n") }
        elseif ($c -gt 127) { [void]$sb.Append('\u').Append([int16]$c).Append('?') }
        else { [void]$sb.Append($ch) }
    }
    [void]$sb.Append('}')
    $sb.ToString()
}

# WiX CLI (dotnet global tool `wix`), or $null.
function Find-Wix {
    $cmd = Get-Command 'wix' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $p = Join-Path $env:USERPROFILE '.dotnet\tools\wix.exe'
    if (Test-Path $p) { return $p }
    $null
}

# Downloads MinGit into $Dest (git\cmd\git.exe, ...) after checking its SHA-256
# against the "MinGit-<v>-64-bit.zip | <sha256>" row of the release notes.
function Install-MinGit([string]$Dest, [string]$WantVersion, [string]$CacheDir) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $headers = @{ 'User-Agent' = 'gitkwaken-build'; 'Accept' = 'application/vnd.github+json' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $api = 'https://api.github.com/repos/git-for-windows/git/releases'
    if ($WantVersion) {
        $rel = Invoke-RestMethod -Headers $headers -Uri "$api/tags/v$($WantVersion.TrimStart('v')).windows.1"
    } else {
        $rel = Invoke-RestMethod -Headers $headers -Uri "$api/latest"
    }
    # Busybox-free build: "MinGit-2.47.1-64-bit.zip" (not "...-busybox-64-bit.zip").
    $asset = $rel.assets | Where-Object { $_.name -match '^MinGit-\d+\.\d+\.\d+(\.\d+)?-64-bit\.zip$' } | Select-Object -First 1
    if (-not $asset) { throw "no busybox-free 64-bit MinGit zip in release $($rel.tag_name)" }
    $null = $asset.name -match '^MinGit-(\d+)\.(\d+)\.(\d+)'
    $ver = [version]('{0}.{1}.{2}' -f $Matches[1], $Matches[2], $Matches[3])
    if ($ver -lt [version]'2.45.0') { throw "MinGit $ver is older than the required 2.45" }
    $m = [regex]::Match($rel.body, [regex]::Escape($asset.name) + '\s*\|\s*([0-9a-fA-F]{64})')
    if (-not $m.Success) { throw "SHA-256 of $($asset.name) not found in the $($rel.tag_name) release notes" }
    $want = $m.Groups[1].Value.ToLowerInvariant()
    New-Item -ItemType Directory -Force $CacheDir | Out-Null
    $zip = Join-Path $CacheDir $asset.name
    if (-not (Test-Path $zip) -or (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant() -ne $want) {
        Write-Host "==> downloading $($asset.browser_download_url)"
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = 'gitkwaken-build' } -Uri $asset.browser_download_url -OutFile $zip
    }
    $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($got -ne $want) {
        Remove-Item -Force $zip
        throw "SHA-256 mismatch for $($asset.name): got $got, release notes list $want"
    }
    Write-Host "==> MinGit $ver ($($asset.name)): SHA-256 $got matches the $($rel.tag_name) release notes"
    if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
    Expand-Archive -Path $zip -DestinationPath $Dest
    $gitExe = Join-Path $Dest 'cmd\git.exe'
    if (-not (Test-Path $gitExe)) { throw "unexpected MinGit layout: $gitExe missing" }
    $v = & $gitExe --version
    if ($LASTEXITCODE -ne 0) { throw "bundled git does not run: $gitExe" }
    Write-Host "==> bundled $v in $Dest"
}

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Dist = if ($DistDir) { [System.IO.Path]::GetFullPath($DistDir) } else { Join-Path $Root 'dist' }
New-Item -ItemType Directory -Force $Dist | Out-Null
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
    if (-not $Repo) {
        $origin = $null
        if (@(git remote) -contains 'origin') { $origin = git remote get-url origin }
        if ($LASTEXITCODE -eq 0 -and $origin -match 'github.com[:/]([^/]+/[^/]+?)(.git)?$') { $Repo = $Matches[1] }
    }
    if ($Repo) {
        $RepoUrl = "https://github.com/$Repo"
        if (-not $DownloadBaseUrl) { $DownloadBaseUrl = "$RepoUrl/releases/download/v$Version" }
        if (-not $Notes) { $Notes = "GitKwaken $Version. See $RepoUrl/releases/tag/v$Version" }
        $env:GITKWAKEN_UPDATE_CHANNEL = "$RepoUrl/releases/latest/download/update.json"
        Write-Host "==> repository $Repo; update channel $env:GITKWAKEN_UPDATE_CHANNEL"
    } else {
        $RepoUrl = ''
        # No GitHub repository: the release folder itself is the update channel,
        # so builds published here are picked up by installed copies.
        $env:GITKWAKEN_UPDATE_CHANNEL = 'file:///' + ((Join-Path $Dist 'update.json') -replace '\\', '/')
        Write-Host "==> no GitHub repository; local update channel $env:GITKWAKEN_UPDATE_CHANNEL"
        if (-not $DownloadBaseUrl) { $DownloadBaseUrl = '.' }
        if (-not $Notes) { $Notes = "GitKwaken $Version" }
    }
    $Name = "GitKwaken-$Version-x64"
    # Installer version resources and the MSI ProductVersion are numeric: 1.0.1-beta.1 -> 1.0.1.
    $NumericVersion = ($Version -split '[-+]')[0]
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
    if ($BundleGit) {
        Install-MinGit -Dest (Join-Path $Stage 'git') -WantVersion $MinGitVersion -CacheDir (Join-Path $Dist 'cache')
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
        $urlDef = if ($RepoUrl) { "/DAppUrl=$RepoUrl" } else { "/DAppUrl=https://github.com" }
        & $Iscc /Qp "/DAppVersion=$Version" "/DNumericVersion=$NumericVersion" "/DSourceDir=$Stage" "/DOutputDir=$Dist" $urlDef (Join-Path $PSScriptRoot 'gitkwaken.iss')
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
        $Setup = Join-Path $Dist "$Name-setup.exe"
        Write-Host ("==> {0} ({1:N2} MB)" -f $Setup, ((Get-Item $Setup).Length / 1MB))
    } else {
        Write-Warning 'Inno Setup (ISCC.exe) not found - installer skipped. Install with: winget install JRSoftware.InnoSetup'
    }

    # ---------------------------------------------------------------- msi
    # Per-user WiX MSI (packaging/wix/gitkwaken.wxs). WiX CLI: dotnet tool install --global wix
    # --version 6.0.2; the UI/Util extensions of the same version are added to the global
    # extension cache on first use.
    $Msi = $null
    $Wix = if ($SkipMsi) { $null } else { Find-Wix }
    if ($Wix) {
        $wixVer = ((& $Wix --version) -split '\+')[0].Trim()
        if ($LASTEXITCODE -ne 0 -or -not $wixVer) { throw "cannot run $Wix --version" }
        Write-Host "==> WiX $wixVer`: $Wix"
        $exts = @(& $Wix extension list -g)
        foreach ($e in 'WixToolset.UI.wixext', 'WixToolset.Util.wixext') {
            if (-not ($exts | Where-Object { $_ -match ('^' + [regex]::Escape("$e $wixVer") + '\b') })) {
                & $Wix extension add -g "$e/$wixVer"
                if ($LASTEXITCODE -ne 0) { throw "wix extension add $e/$wixVer failed" }
            }
        }
        # MSI ProductVersion is numeric ($NumericVersion); MajorUpgrade allows same-version
        # upgrades, so successive pre-releases of one version still replace each other.
        $WixObj = Join-Path $Dist 'wix'
        New-Item -ItemType Directory -Force $WixObj | Out-Null
        $LicenseRtf = Join-Path $WixObj 'License.rtf'
        $licText = "GitKwaken is dual-licensed under the MIT license or the Apache License, Version 2.0, " +
            "at your option. Both texts are installed next to gitkwaken.exe (LICENSE-MIT, LICENSE-APACHE).`n`n" +
            [System.IO.File]::ReadAllText((Join-Path $Root 'LICENSE-MIT')) + "`n`n" +
            [System.IO.File]::ReadAllText((Join-Path $Root 'LICENSE-APACHE'))
        [System.IO.File]::WriteAllText($LicenseRtf, (ConvertTo-Rtf $licText), [System.Text.Encoding]::ASCII)
        $Msi = Join-Path $Dist "$Name.msi"
        if (Test-Path $Msi) { Remove-Item -Force $Msi }
        $wixArgs = @(
            'build', (Join-Path $PSScriptRoot 'wix\gitkwaken.wxs'), '-arch', 'x64', '-nologo',
            '-ext', "WixToolset.UI.wixext/$wixVer", '-ext', "WixToolset.Util.wixext/$wixVer",
            '-d', "Version=$NumericVersion", '-d', "SourceDir=$Stage",
            '-d', "IconFile=$(Join-Path $Root 'assets\icon.ico')", '-d', "LicenseRtf=$LicenseRtf",
            '-d', "AppUrl=$(if ($RepoUrl) { $RepoUrl } else { 'https://github.com' })",
            '-intermediatefolder', (Join-Path $WixObj 'obj'), '-pdbtype', 'none', '-o', $Msi
        )
        # ICE validation. Expected warnings: ICE61 (same-version upgrades allowed) and ICE91
        # (per-user files); errors fail the build.
        $validateArgs = @('msi', 'validate', '-nologo')
        if (Test-Path (Join-Path $Stage 'git\cmd\git.exe')) {
            $wixArgs += @('-d', 'BundleGit=1')
            # The harvested MinGit tree (<Files>) uses file KeyPaths (ICE38) and has no
            # RemoveFile row per sub-folder (ICE64); both only matter for roaming profiles, and
            # uninstall removes the whole tree (verified with msiexec /x).
            $validateArgs += @('-sice', 'ICE38', '-sice', 'ICE64')
        }
        & $Wix @wixArgs
        if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
        & $Wix @validateArgs $Msi
        if ($LASTEXITCODE -ne 0) { throw "wix msi validate failed ($LASTEXITCODE)" }
        Write-Host ("==> {0} ({1:N2} MB, ProductVersion {2})" -f $Msi, ((Get-Item $Msi).Length / 1MB), $NumericVersion)
    } elseif ($SkipMsi) {
        Write-Host '==> MSI skipped (-SkipMsi)'
    } else {
        Write-Warning 'WiX CLI (wix) not found - MSI skipped. Install with: dotnet tool install --global wix --version 6.0.2'
    }

    # ---------------------------------------------------------------- checksums
    $sums = foreach ($f in @($Zip, $Setup, $Msi) | Where-Object { $_ }) {
        '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 $f).Hash.ToLowerInvariant(), (Split-Path -Leaf $f)
    }
    [System.IO.File]::WriteAllText((Join-Path $Dist 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding $false))

    # ---------------------------------------------------------------- winget
    if ($Repo -and $Setup) {
        $WingetOut = Join-Path $Dist 'winget'
        New-Item -ItemType Directory -Force $WingetOut | Out-Null
        $vars = @{
            '{{VERSION}}'      = $Version
            '{{REPO}}'         = $Repo
            '{{RELEASE_DATE}}' = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')
            '{{ZIP_SHA256}}'   = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToUpperInvariant()
            '{{SETUP_SHA256}}' = (Get-FileHash -Algorithm SHA256 $Setup).Hash.ToUpperInvariant()
        }
        foreach ($t in Get-ChildItem (Join-Path $PSScriptRoot 'winget') -Filter '*.yaml') {
            $text = [System.IO.File]::ReadAllText($t.FullName)
            foreach ($k in $vars.Keys) { $text = $text.Replace($k, $vars[$k]) }
            [System.IO.File]::WriteAllText((Join-Path $WingetOut $t.Name), $text, (New-Object System.Text.UTF8Encoding $false))
        }
        Write-Host "==> dist\winget (manifests for $Repo)"
    }

    Write-Host '==> done'
}
finally {
    Pop-Location
}
