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

# 2. Locked build of both unsigned APKs and the stable module staging (Gradle STRICT verification, cargo --locked).
Native 'unsigned release build' {
    .\gradlew.bat :standalone:assembleRelease :root-frontend:assembleRelease :root-frontend:stageStableMagiskModule --console=plain
}
Pass 'release APKs and stable module built with locked dependencies'

$standaloneApk = 'standalone/build/outputs/apk/release/standalone-release-unsigned.apk'
$frontendApk = 'root-frontend/build/outputs/apk/release/root-frontend-release-unsigned.apk'
foreach ($apk in $standaloneApk, $frontendApk) {
    if (-not (Test-Path $apk)) { Fail "unsigned release APK exists at $apk" }
}

# 3. Declared release identity (S-STACK-001) from each built APK itself; both editions carry one version.
function Read-Identity([string]$apk, [string]$expectedPackage) {
    $badging = & (Join-Path $buildTools 'aapt2.exe') dump badging $apk
    if ($LASTEXITCODE -ne 0) { Fail "aapt2 dump badging $apk" }
    $package = $badging | Where-Object { $_ -like 'package:*' } | Select-Object -First 1
    if ($package -notmatch "name='$([regex]::Escape($expectedPackage))' versionCode='(\d+)' versionName='([0-9.]+)'.*compileSdkVersion='37'") {
        Fail "APK package identity is $expectedPackage with compileSdkVersion 37 ($package)"
    }
    $code = [long]$Matches[1]
    $name = $Matches[2]
    $parts = $name.Split('.')
    if ($parts.Count -ne 3 -or $code -ne ([long]$parts[0] * 1000000 + [long]$parts[1] * 1000 + [long]$parts[2])) {
        Fail "versionCode $code derives from versionName $name"
    }
    if (-not ($badging -contains "minSdkVersion:'33'") -or -not ($badging -contains "targetSdkVersion:'37'")) {
        Fail "$expectedPackage minSdk 33 and targetSdk 37"
    }
    return @($name, $code)
}
$version, $versionCode = Read-Identity $standaloneApk 'com.droidbridge.standalone'
$frontendVersion, $frontendCode = Read-Identity $frontendApk 'com.droidbridge.root'
if ($frontendVersion -ne $version -or $frontendCode -ne $versionCode) {
    Fail "both editions carry one version ($version/$versionCode vs $frontendVersion/$frontendCode)"
}
Pass "release identities com.droidbridge.standalone and com.droidbridge.root $version ($versionCode), min 33, target/compile 37"

# 4. The App ships ARM64 and x86_64 Runtime/guard binaries; the root edition remains ARM64.
Native 'App native payload' { java tools/ReleaseTool.java verify-apk-native $standaloneApk app }
Native 'root frontend native payload' { java tools/ReleaseTool.java verify-apk-native $frontendApk root }
$staging = 'root-frontend/build/generated/magiskModule/stable'
foreach ($binary in 'bin/droidbridged', 'bin/droidbridge-supervisor', 'bin/droidbridge-exec-guard', 'module.prop', 'frontend.package') {
    if (-not (Test-Path (Join-Path $staging $binary))) { Fail "module staging contains $binary" }
}
if (Test-Path (Join-Path $staging 'frontend.apk')) { Fail 'stable staging carries no frontend before it is signed' }
Pass 'App ARM64/x86_64 and root ARM64 native payloads verify; module executables are staged'

# 5. APK signing (signed mode) or unsigned candidates. One key signs both editions' APKs.
$dist = Join-Path $root "build/release/$version-$Mode"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
$apkDist = Join-Path $dist 'apk'
$moduleDist = Join-Path $dist 'magisk'
New-Item -ItemType Directory -Force $apkDist, $moduleDist | Out-Null
$apk = Join-Path $apkDist "droidbridge-$version-arm64-v8a.apk"
$frontend = Join-Path $dist 'frontend.apk'
if ($Mode -eq 'unsigned') {
    Copy-Item $standaloneApk $apk
    Copy-Item $frontendApk $frontend
    $manifestKey = 'tools/fixtures/release/manifest-test-private-key.pem'
} else {
    $keystore = Join-Path $KeyDirectory 'droidbridge-release.p12'
    $passwordFile = Join-Path $KeyDirectory 'keystore-password.txt'
    $configured = (Get-Content release-config.properties | Where-Object { $_ -like 'apk_signer_sha256=*' }).Split('=')[1]
    foreach ($pair in @(@($standaloneApk, $apk), @($frontendApk, $frontend))) {
        $unsigned, $signed = $pair
        Native "apksigner sign $unsigned" {
            & (Join-Path $buildTools 'apksigner.bat') sign --ks $keystore --ks-key-alias droidbridge-release `
                --ks-pass "file:$passwordFile" --v1-signing-enabled false --v2-signing-enabled true `
                --v3-signing-enabled true --v4-signing-enabled false --out $signed $unsigned
        }
        $verify = & (Join-Path $buildTools 'apksigner.bat') verify --verbose --print-certs $signed
        if ($LASTEXITCODE -ne 0) { Fail "apksigner verify $signed" }
        # apksigner reports the schemes it verified for the APK's platform range, not the blocks present:
        # at minSdkVersion 33 it verifies v3 alone, so the v2 block it does write reads false here.
        foreach ($expected in 'Verified using v1 scheme (JAR signing): false',
            'Verified using v3 scheme (APK Signature Scheme v3): true', 'Verified using v4 scheme (APK Signature Scheme v4): false') {
            if (-not ($verify -contains $expected)) { Fail "apksigner reports '$expected' for $signed" }
        }
        $certificate = $verify | Where-Object { $_ -match 'Signer #1 certificate SHA-256 digest: ([0-9a-f]{64})' } | Select-Object -First 1
        if (-not $certificate -or $Matches[1] -ne $configured) { Fail "$signed signing certificate equals the configured stable fingerprint" }
    }
    $manifestKey = Join-Path $KeyDirectory 'release-manifest-private-key.pem'
    Pass 'both APKs sign with v3 and not v1 or v4, using the configured stable certificate'
}

# 6. Deterministic module ZIP carrying the frontend: two consecutive stagings produce identical bytes.
$moduleName = "droidbridge-magisk-$version.zip"
function Zip-Module([string]$out) {
    $tree = Join-Path $dist 'module-tree'
    if (Test-Path $tree) { Remove-Item -Recurse -Force $tree }
    Copy-Item -Recurse $staging $tree
    Copy-Item $frontend (Join-Path $tree 'frontend.apk')
    Native 'module zip' { java tools/ReleaseTool.java module-zip $tree $out }
    Remove-Item -Recurse -Force $tree
}
Zip-Module (Join-Path $moduleDist $moduleName)
Native 'module restaging' { .\gradlew.bat :root-frontend:stageStableMagiskModule --rerun-tasks --console=plain }
$second = Join-Path $dist 'module-repeat.zip'
Zip-Module $second
$firstHash = (Get-FileHash (Join-Path $moduleDist $moduleName) -Algorithm SHA256).Hash
$secondHash = (Get-FileHash $second -Algorithm SHA256).Hash
Remove-Item $second, $frontend
if ($firstHash -ne $secondHash) { Fail 'module ZIP is byte-identical across two consecutive builds' }
Pass "module ZIP is byte-identical across two consecutive builds ($($firstHash.ToLowerInvariant()))"

# 7. Third-party notices regenerate byte-identically and ship with both releases.
$notices = Join-Path $dist 'THIRD_PARTY_NOTICES.txt'
Native 'generate notices' { pwsh -NoProfile -File tools/generate-notices.ps1 -OutputPath $notices }
if ((Get-FileHash $notices).Hash -ne (Get-FileHash 'THIRD_PARTY_NOTICES.txt').Hash) {
    Fail 'generated notices equal the committed THIRD_PARTY_NOTICES.txt'
}
Move-Item $notices $apkDist
Copy-Item (Join-Path $apkDist 'THIRD_PARTY_NOTICES.txt') $moduleDist
Pass 'every third-party component requiring attribution appears in the generated notices'

# 8. The standalone release: manifest, signature, checksums and full verification.
$publishedAt = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
Native 'manifest' { java tools/ReleaseTool.java manifest $version $publishedAt $apkDist }
Native 'manifest signature' { java tools/ReleaseTool.java sign (Join-Path $apkDist 'release.json') $manifestKey (Join-Path $apkDist 'release.json.sig') }
Native 'APK checksums' { java tools/ReleaseTool.java sums $apkDist $version apk }
Native 'verify-release' { java tools/ReleaseTool.java verify-release $apkDist $version $Mode }

# 9. The module release: checksums, contents and the committed updateJson that offers it.
Native 'module checksums' { java tools/ReleaseTool.java sums $moduleDist $version magisk }
Native 'verify-module' { java tools/ReleaseTool.java verify-module $moduleDist $version }

Write-Output "RESULT PASS check-release $Mode $version at $revision -> apk-v$version $apkDist, magisk-v$version $moduleDist"
