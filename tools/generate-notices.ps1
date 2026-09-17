param([string]$OutputPath = 'THIRD_PARTY_NOTICES.txt')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$rows = @(Import-Csv tools\third-party-direct.tsv -Delimiter "`t")
if ($rows.Count -eq 0) { throw 'third-party direct inventory is empty' }

$seen = @{}
foreach ($row in $rows) {
    foreach ($field in @('kind','name','version','license','source')) {
        if ([string]::IsNullOrWhiteSpace($row.$field)) { throw "third-party row missing ${field}: $($row.name)" }
    }
    $key = "$($row.kind)|$($row.name)|$($row.version)"
    if ($seen.ContainsKey($key)) { throw "duplicate third-party row: $key" }
    $seen[$key] = $true
}

if (Test-Path 'rust/crates') {
    if (-not (Test-Path 'rust/Cargo.lock')) { throw 'Cargo.lock missing after Rust crates materialized' }
    $cargoJson = (& cargo +1.98.0 metadata --manifest-path rust/Cargo.toml --format-version 1 --locked | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $cargo = $cargoJson | ConvertFrom-Json
    . "$PSScriptRoot/notice-dependencies.ps1"
    Assert-NoticeDependencies $cargo $rows
}

$lines = @(
    'DroidBridge third-party notices (declared direct dependencies and native/build inputs).',
    'Generated deterministically from tools/third-party-direct.tsv.',
    ''
)
$entries = [string[]]@($rows | ForEach-Object { "[$($_.kind)] $($_.name) $($_.version) | $($_.license) | $($_.source)" })
[Array]::Sort($entries, [StringComparer]::Ordinal)
$lines += $entries
$text = ($lines -join "`n") + "`n"
$target = if ([IO.Path]::IsPathRooted($OutputPath)) { $OutputPath } else { Join-Path $root $OutputPath }
$parent = Split-Path $target -Parent
if ($parent) { New-Item -ItemType Directory -Force $parent | Out-Null }
[IO.File]::WriteAllText($target,$text,(New-Object Text.UTF8Encoding($false)))
Write-Output "Third-party notices generated: $target"
