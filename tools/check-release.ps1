param(
    [Parameter(Mandatory = $true)][ValidateSet('unsigned', 'signed')][string]$Mode,
    # Signed mode only: the operator's offline key folder (droidbridge-release.p12, keystore-password.txt,
    # release-manifest-private-key.pem). Keys are passed to the signers by path and never printed.
    [string]$KeyDirectory
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "$env:LOCALAPPDATA\Android\Sdk" }
$buildTools = Join-Path $sdk 'build-tools\36.0.0'

function Pass([string]$assertion) { Write-Output "PASS $assertion" }
function Fail([string]$assertion) { throw "FAIL $assertion" }
function Native([string]$what, [scriptblock]$command) {
    & $command
    if ($LASTEXITCODE -ne 0) { Fail "$what (exit $LASTEXITCODE)" }
}

if ($Mode -eq 'signed') {
    if (-not $KeyDirectory) { Fail 'signed mode requires -KeyDirectory' }
    foreach ($name in 'droidbridge-release.p12', 'keystore-password.txt', 'release-manifest-private-key.pem') {
        if (-not (Test-Path (Join-Path $KeyDirectory $name))) { Fail "key directory contains $name" }
    }
    if ((Resolve-Path $KeyDirectory).Path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        Fail 'release keys live outside the repository'
    }
} elseif ($KeyDirectory) {
    Fail 'unsigned mode never receives release keys'
}

# 1. Clean working tree at the verified revision.
Native 'git status' { $script:dirty = git status --porcelain --untracked-files=no }
if ($dirty) { Fail "working tree is clean (tracked changes: $($dirty -join '; '))" }
$revision = (git rev-parse HEAD).Trim()
Pass "working tree is clean at $revision"
Native 'release configuration' { java tools/ReleaseTool.java check-config }

# 2. Locked build of the unsigned APK and the stable module staging (Gradle STRICT verification, cargo --locked).
Native 'unsigned release build' { .\gradlew.bat :app:assembleRelease :app:stageStableMagiskModule --console=plain }
Pass 'release APK and stable module built with locked dependencies'

$unsignedApk = 'app/build/outputs/apk/release/app-release-unsigned.apk'
if (-not (Test-Path $unsignedApk)) { Fail "unsigned release APK exists at $unsignedApk" }

# 3. Declared release identity (S-STACK-001) from the built APK itself.
$badging = & (Join-Path $buildTools 'aapt2.exe') dump badging $unsignedApk
if ($LASTEXITCODE -ne 0) { Fail 'aapt2 dump badging' }
$package = $badging | Where-Object { $_ -like 'package:*' } | Select-Object -First 1
if ($package -notmatch "name='com\.droidbridge\.android' versionCode='(\d+)' versionName='([0-9.]+)'.*compileSdkVersion='37'") {
    Fail "APK package identity is com.droidbridge.android with compileSdkVersion 37 ($package)"
}
$versionCode = [long]$Matches[1]
$version = $Matches[2]
$parts = $version.Split('.')
if ($parts.Count -ne 3 -or $versionCode -ne ([long]$parts[0] * 1000000 + [long]$parts[1] * 1000 + [long]$parts[2])) {
    Fail "versionCode $versionCode derives from versionName $version"
}
if (-not ($badging -contains "minSdkVersion:'33'") -or -not ($badging -contains "targetSdkVersion:'37'")) {
    Fail 'APK minSdk 33 and targetSdk 37'
}
Pass "release identity com.droidbridge.android $version ($versionCode), min 33, target/compile 37"

# 4. Exactly the arm64-v8a ABI set in the APK and the module native executables.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$apkZip = [IO.Compression.ZipFile]::OpenRead((Resolve-Path $unsignedApk))
try {
    $abis = @($apkZip.Entries | Where-Object { $_.FullName -like 'lib/*' } | ForEach-Object { $_.FullName.Split('/')[1] } | Sort-Object -Unique)
} finally { $apkZip.Dispose() }
if (($abis -join ',') -ne 'arm64-v8a') { Fail "APK native ABI set is exactly arm64-v8a (found $($abis -join ','))" }
$staging = 'app/build/generated/magiskModule/stable'
foreach ($binary in 'bin/droidbridged', 'bin/droidbridge-supervisor', 'bin/droidbridge-exec-guard', 'module.prop') {
    if (-not (Test-Path (Join-Path $staging $binary))) { Fail "module staging contains $binary" }
}
$moduleProp = (Get-Content -Raw (Join-Path $staging 'module.prop')).Replace("`r", '')
$expectedProp = "id=droidbridge`nname=DroidBridge`nversion=$version`nversionCode=$versionCode`nauthor=DroidBridge`ndescription=DroidBridge privileged Android backend`n"
if ($moduleProp -ne $expectedProp) { Fail 'module.prop matches S-DIST-005 for this release version' }
Pass 'APK ABI set is arm64-v8a and module executables/module.prop are present'

# 5. Deterministic module ZIP: two consecutive stagings of the same revision produce identical bytes.
$dist = Join-Path $root "build/release/$version-$Mode"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null
$moduleName = "droidbridge-magisk-$version.zip"
Native 'module zip (first)' { java tools/ReleaseTool.java module-zip $staging (Join-Path $dist $moduleName) }
Native 'module restaging' { .\gradlew.bat :app:stageStableMagiskModule --rerun-tasks --console=plain }
$second = Join-Path $root 'build/release/module-repeat.zip'
Native 'module zip (second)' { java tools/ReleaseTool.java module-zip $staging $second }
$firstHash = (Get-FileHash (Join-Path $dist $moduleName) -Algorithm SHA256).Hash
$secondHash = (Get-FileHash $second -Algorithm SHA256).Hash
Remove-Item $second
if ($firstHash -ne $secondHash) { Fail 'module ZIP is byte-identical across two consecutive builds' }
Pass "module ZIP is byte-identical across two consecutive builds ($($firstHash.ToLowerInvariant()))"

# 6. Third-party notices regenerate byte-identically.
$notices = Join-Path $dist 'THIRD_PARTY_NOTICES.txt'
Native 'generate notices' { pwsh -NoProfile -File tools/generate-notices.ps1 -OutputPath $notices }
if ((Get-FileHash $notices).Hash -ne (Get-FileHash 'THIRD_PARTY_NOTICES.txt').Hash) {
    Fail 'generated notices equal the committed THIRD_PARTY_NOTICES.txt'
}
Pass 'every third-party component requiring attribution appears in the generated notices'

# 7. APK signing (signed mode) or unsigned candidate, then manifest, signature and checksums.
$apkName = "droidbridge-$version-arm64-v8a.apk"
$apk = Join-Path $dist $apkName
if ($Mode -eq 'unsigned') {
    Copy-Item $unsignedApk $apk
    $manifestKey = 'tools/fixtures/release/manifest-test-private-key.pem'
} else {
    $keystore = Join-Path $KeyDirectory 'droidbridge-release.p12'
    $passwordFile = Join-Path $KeyDirectory 'keystore-password.txt'
    Native 'apksigner sign' {
        & (Join-Path $buildTools 'apksigner.bat') sign --ks $keystore --ks-key-alias droidbridge-release `
            --ks-pass "file:$passwordFile" --v1-signing-enabled false --v2-signing-enabled true `
            --v3-signing-enabled true --v4-signing-enabled false --out $apk $unsignedApk
    }
    $manifestKey = Join-Path $KeyDirectory 'release-manifest-private-key.pem'
    $verify = & (Join-Path $buildTools 'apksigner.bat') verify --verbose --print-certs $apk
    if ($LASTEXITCODE -ne 0) { Fail 'apksigner verify' }
    # apksigner reports the schemes it verified for the APK's platform range, not the blocks present:
    # at minSdkVersion 33 it verifies v3 alone, so the v2 block it does write reads false here.
    foreach ($expected in 'Verified using v1 scheme (JAR signing): false',
        'Verified using v3 scheme (APK Signature Scheme v3): true', 'Verified using v4 scheme (APK Signature Scheme v4): false') {
        if (-not ($verify -contains $expected)) { Fail "apksigner reports '$expected'" }
    }
    $configured = (Get-Content release-config.properties | Where-Object { $_ -like 'apk_signer_sha256=*' }).Split('=')[1]
    $certificate = $verify | Where-Object { $_ -match 'Signer #1 certificate SHA-256 digest: ([0-9a-f]{64})' } | Select-Object -First 1
    if (-not $certificate -or $Matches[1] -ne $configured) { Fail 'APK signing certificate equals the configured stable fingerprint' }
    Pass 'APK signs with v3 and not v1 or v4, using the configured stable certificate'
}
$publishedAt = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
Native 'manifest' { java tools/ReleaseTool.java manifest $version $publishedAt $dist }
Native 'manifest signature' { java tools/ReleaseTool.java sign (Join-Path $dist 'release.json') $manifestKey (Join-Path $dist 'release.json.sig') }
Native 'checksums' { java tools/ReleaseTool.java sums $dist $version }
Native 'verify-release' { java tools/ReleaseTool.java verify-release $dist $version $Mode }

Write-Output "RESULT PASS check-release $Mode $version at $revision -> $dist"
