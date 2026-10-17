<#
.SYNOPSIS
  Writes THIRD-PARTY-LICENSES.txt: the license texts and copyright notices of
  every crate compiled into gitkwaken.exe, plus the data bundled with them.
.DESCRIPTION
  Walks `cargo metadata` from the workspace members over normal and build
  dependencies for the given target, and copies each crate's LICENSE / COPYING
  / NOTICE / font-license files from its source directory (the cargo registry).
  Crates that ship no license file get their SPDX expression and a link to the
  standard text. Binary distributions (zip, setup.exe, msi) must carry this
  file: MIT, BSD, Apache-2.0, OFL and the other licenses below require their
  notices to accompany copies of the software.
.EXAMPLE
  powershell -File scripts/third-party-licenses.ps1 -Out dist\GitKwaken-1.0.0-x64\THIRD-PARTY-LICENSES.txt
#>
param(
  [string]$Out = 'THIRD-PARTY-LICENSES.txt',
  [string]$Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $Root
try {
  $json = & cargo metadata --format-version 1 --filter-platform $Target 2>$null
  if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
} finally { Pop-Location }
$meta = $json | ConvertFrom-Json

$byId = @{}; foreach ($p in $meta.packages) { $byId[$p.id] = $p }
$nodes = @{}; foreach ($n in $meta.resolve.nodes) { $nodes[$n.id] = $n }
$ws = @{}; foreach ($id in $meta.workspace_members) { $ws[$id] = $true }
# Only what goes into the shipped binary: gk-app and its normal/build deps.
$roots = $meta.workspace_members | Where-Object { $byId[$_].name -eq 'gk-app' }
$seen = @{}; $stack = New-Object System.Collections.Stack
foreach ($r in $roots) { $stack.Push($r) }
while ($stack.Count) {
  $id = $stack.Pop()
  if ($seen.ContainsKey($id)) { continue }
  $seen[$id] = $true
  foreach ($d in $nodes[$id].deps) {
    if ($d.dep_kinds | Where-Object { $_.kind -eq $null -or $_.kind -eq 'build' }) { $stack.Push($d.pkg) }
  }
}
$pkgs = $seen.Keys | Where-Object { -not $ws.ContainsKey($_) } | ForEach-Object { $byId[$_] } |
  Sort-Object name, version

$licensePattern = '^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE|OFL|UFL|AUTHORS)([-_.].*)?$|^.*[-_]LICEN[CS]E(\..*)?$'
$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine('GitKwaken - third-party licenses')
[void]$sb.AppendLine('================================')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('gitkwaken.exe contains the Rust crates listed below (compiled in), and with them')
[void]$sb.AppendLine('the following bundled data:')
[void]$sb.AppendLine('- fonts from epaint_default_fonts (Ubuntu-Light: Ubuntu Font Licence 1.0; Hack,')
[void]$sb.AppendLine('  emoji icon font: SIL Open Font License 1.1 / MIT) - texts in that crate''s entry;')
[void]$sb.AppendLine('- syntax definitions and colour themes bundled by syntect: syntaxes from')
[void]$sb.AppendLine('  https://github.com/sublimehq/Packages ("Permission to copy, use, modify, sell and')
[void]$sb.AppendLine('  distribute this software is granted. This software is provided "as is" without')
[void]$sb.AppendLine('  express or implied warranty, and with no claim as to its suitability for any')
[void]$sb.AppendLine('  purpose."), themes base16-ocean, InspiredGitHub and Solarized (MIT).')
[void]$sb.AppendLine('Optional builds with a bundled MinGit (packaging/build-release.ps1 -BundleGit) also')
[void]$sb.AppendLine('ship Git for Windows under the GNU GPL v2 with its own license files in git\;')
[void]$sb.AppendLine('its source is at https://github.com/git-for-windows/git (tag of the bundled version).')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Crates: $(@($pkgs).Count) (target $Target).")
[void]$sb.AppendLine('')

$missing = @()
$texts = [ordered]@{}
foreach ($p in $pkgs) {
  $dir = Split-Path $p.manifest_path
  [void]$sb.AppendLine(('-' * 78))
  [void]$sb.AppendLine("$($p.name) $($p.version)")
  [void]$sb.AppendLine("License: $($p.license)")
  if ($p.repository) { [void]$sb.AppendLine("Source:  $($p.repository)") }
  if ($p.authors -and @($p.authors).Count) { [void]$sb.AppendLine("Authors: $(@($p.authors) -join ', ')") }
  [void]$sb.AppendLine(('-' * 78))
  $files = @()
  if ($p.license_file) { $files += Join-Path $dir $p.license_file }
  $files += Get-ChildItem -LiteralPath $dir -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match $licensePattern } | ForEach-Object { $_.FullName }
  # Font crates keep their licenses next to the fonts.
  foreach ($sub in 'fonts', 'assets', 'data') {
    $sd = Join-Path $dir $sub
    if (Test-Path $sd) {
      $files += Get-ChildItem -LiteralPath $sd -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match $licensePattern -or $_.Name -match '(?i)license' -or $_.Extension -eq '.txt' } |
        ForEach-Object { $_.FullName }
    }
  }
  $files = $files | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Sort-Object -Unique
  if (-not $files) {
    $missing += "$($p.name) $($p.version)"
    $ids = ([regex]::Matches(($p.license -replace '[()]', ' '), '[A-Za-z0-9.\-+]+') |
      ForEach-Object { $_.Value } | Where-Object { $_ -notin 'OR', 'AND', 'WITH' } | Sort-Object -Unique)
    [void]$sb.AppendLine('(no license file in the published crate; standard texts:)')
    foreach ($i in $ids) { [void]$sb.AppendLine("  https://spdx.org/licenses/$i.html") }
  }
  # Identical texts (the many verbatim Apache-2.0 copies) are printed once at the
  # end; texts with their own copyright line stay unique per crate.
  $refs = foreach ($f in $files) {
    $text = ((Get-Content -LiteralPath $f -Raw -Encoding UTF8) -replace "`r`n", "`n").Trim()
    if (-not $texts.Contains($text)) { $texts[$text] = $texts.Count + 1 }
    "[$($texts[$text])] $(Split-Path $f -Leaf)"
  }
  if ($refs) { [void]$sb.AppendLine("License texts: $($refs -join ', ')") }
  [void]$sb.AppendLine('')
}

[void]$sb.AppendLine(('=' * 78))
[void]$sb.AppendLine('License texts referenced above')
[void]$sb.AppendLine(('=' * 78))
foreach ($e in $texts.GetEnumerator()) {
  [void]$sb.AppendLine('')
  [void]$sb.AppendLine("[$($e.Value)]")
  [void]$sb.AppendLine($e.Key)
}

if (-not [IO.Path]::IsPathRooted($Out)) { $Out = Join-Path (Get-Location) $Out }
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText($Out, ($sb.ToString() -replace "`r`n", "`n" -replace "`n", "`r`n"), $utf8)
Write-Host ("==> {0}: {1} crates, {2:N0} KB" -f $Out, @($pkgs).Count, ((Get-Item $Out).Length / 1KB))
if ($missing) { Write-Host "    no license file in: $($missing -join ', ')" }
