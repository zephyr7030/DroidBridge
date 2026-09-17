param([string]$CacheRoot = "build/i0-cache")
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$version = "1.10.6"
$expectedSource = "872dd11337fe1ab02ad9d4fee047c9da244d695c6ddf34e2ebb733efd4ed8aa9"
$url = "https://www.tcpdump.org/release/libpcap-$version.tar.gz"
$archive = Join-Path $CacheRoot "libpcap-$version.tar.gz"
$source = Join-Path $CacheRoot "libpcap-$version"
$build = Join-Path $CacheRoot "libpcap-$version-android-arm64"
New-Item -ItemType Directory -Force $CacheRoot | Out-Null
if (!(Test-Path $archive)) {
    & curl.exe -L --fail --retry 3 --ssl-no-revoke --tlsv1.2 -o $archive $url
    if ($LASTEXITCODE -ne 0) { throw "Pinned libpcap source unavailable from canonical HTTPS URL: curl exit $LASTEXITCODE" }
}
$actualSource = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualSource -ne $expectedSource) { throw "libpcap SHA-256 mismatch: $actualSource" }
if (Test-Path $source) { Remove-Item -Recurse -Force $source }
& tar.exe -xzf $archive -C $CacheRoot
if ($LASTEXITCODE -ne 0) { throw "libpcap extraction failed" }
$patch = "$PSScriptRoot\patches\libpcap-1.10.6-host-null-device.patch"
$patchHash = (Get-FileHash $patch -Algorithm SHA256).Hash.ToLowerInvariant()
Push-Location $source
try {
    & git apply --check $patch
    if ($LASTEXITCODE -ne 0) { throw "libpcap patch pristine-context check failed" }
    & git apply $patch
    if ($LASTEXITCODE -ne 0) { throw "libpcap patch apply failed" }
} finally { Pop-Location }
$wf = & "$PSScriptRoot\bootstrap-winflexbison.ps1" -CacheRoot $CacheRoot
$raw = ((Get-Content local.properties | Select-String '^sdk.dir=').Line.Substring(8))
$sdk = $raw -replace '\\:',':' -replace '\\\\','\'
$cmake = "$sdk\cmake\3.31.6\bin\cmake.exe"
$ninja = "$sdk\cmake\3.31.6\bin\ninja.exe"
$toolchain = "$sdk\ndk\29.0.14206865\build\cmake\android.toolchain.cmake"
if (Test-Path $build) { Remove-Item -Recurse -Force $build }
& $cmake -S $source -B $build -G Ninja "-DCMAKE_MAKE_PROGRAM=$ninja" "-DCMAKE_TOOLCHAIN_FILE=$toolchain" '-DANDROID_ABI=arm64-v8a' '-DANDROID_PLATFORM=android-30' '-DBUILD_SHARED_LIBS=OFF' "-DLEX_EXECUTABLE=$($wf.Flex)" "-DYACC_EXECUTABLE=$($wf.Bison)"
if ($LASTEXITCODE -ne 0) { throw "libpcap Android configure failed" }
& $cmake --build $build --target pcap_static
if ($LASTEXITCODE -ne 0) { throw "libpcap pcap_static build failed" }
$library = Get-ChildItem $build -Recurse -File -Filter 'libpcap.a' | Select-Object -First 1
if (!$library) { throw "libpcap.a not produced" }
Write-Output "libpcap source sha256=$actualSource"
Write-Output "libpcap patch sha256=$patchHash"
Write-Output "libpcap static=$($library.FullName)"
