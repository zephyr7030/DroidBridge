param([string]$CacheRoot = "build/i0-cache")
$ErrorActionPreference = "Stop"
$version = "2.5.25"
$expected = "8d324b62be33604b2c45ad1dd34ab93d722534448f55a16ca7292de32b6ac135"
$url = "https://github.com/lexxmark/winflexbison/releases/download/v$version/win_flex_bison-$version.zip"
$zip = Join-Path $CacheRoot "win_flex_bison-$version.zip"
$dir = Join-Path $CacheRoot "winflexbison-$version"
New-Item -ItemType Directory -Force $CacheRoot | Out-Null
if (!(Test-Path $zip)) {
    & curl.exe -L --fail --retry 3 -o $zip $url
    if ($LASTEXITCODE -ne 0) { throw "WinFlexBison download failed: exit $LASTEXITCODE" }
}
$actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "WinFlexBison SHA-256 mismatch: $actual" }
if (!(Test-Path (Join-Path $dir "win_flex.exe"))) { Expand-Archive -Force $zip $dir }
$dir = (Resolve-Path $dir).Path
$flex = (Resolve-Path (Join-Path $dir "win_flex.exe")).Path
$bison = (Resolve-Path (Join-Path $dir "win_bison.exe")).Path
$flexVersion = (& $flex --version | Select-Object -First 1)
$bisonVersion = (& $bison --version | Select-Object -First 1)
if ($flexVersion -notmatch "2\.6\.4") { throw "Unexpected Flex version: $flexVersion" }
if ($bisonVersion -notmatch "3\.8\.2") { throw "Unexpected Bison version: $bisonVersion" }
[pscustomobject]@{ Root=$dir; Flex=$flex; Bison=$bison; Sha256=$actual; FlexVersion=$flexVersion; BisonVersion=$bisonVersion }
