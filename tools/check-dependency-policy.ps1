$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
function Require([bool]$ok,[string]$message){ if(-not $ok){ throw "DEPENDENCY_CHECK_FAILED: $message" } }
function RequireText([string]$text,[string]$needle,[string]$label){ Require ($text.Contains($needle)) "$label missing/changed" }

$versions = Get-Content 'gradle/libs.versions.toml' -Raw
foreach($pair in @(
    @('agp','9.4.0'), @('kotlin','2.4.10'), @('composeBom','2026.08.00'),
    @('activityCompose','1.13.0'), @('lifecycle','2.11.0'), @('core','1.19.0'),
    @('datastore','1.2.1'), @('coroutines','1.11.0'), @('serialization','1.11.0'),
    @('shizuku','13.1.5')
)) {
    $pattern = '(?m)^\s*{0}\s*=\s*"{1}"' -f [regex]::Escape($pair[0]), [regex]::Escape($pair[1])
    Require ($versions -match $pattern) "$($pair[0])=$($pair[1]) required"
}
foreach($v in @('1.3.0','1.1.7','1.5.1')) { Require ($versions.Contains($v)) "required stable Android dependency version $v absent" }
foreach($forbidden in @('androidx.navigation:navigation-','androidx.appcompat','androidx.fragment','room','workmanager','hilt','koin','retrofit','okhttp','rxjava')) {
    if($versions.ToLowerInvariant().Contains($forbidden.ToLowerInvariant())) { throw "DEPENDENCY_CHECK_FAILED: forbidden dependency family in catalog: $forbidden" }
}

$cargo = Get-Content 'rust/Cargo.toml' -Raw
$expectedCargo = @(
'serde = { version = "=1.0.229", default-features = false, features = ["derive", "std"] }',
'serde_json = "=1.0.151"','schemars = "=1.2.2"','thiserror = "=2.0.20"',
'tokio = { version = "=1.53.1", default-features = false, features = ["rt-multi-thread", "macros", "sync", "time", "process", "io-util", "net", "fs"] }',
'uuid = { version = "=1.26.0", default-features = false, features = ["v4", "serde", "std"] }',
'sha2 = "=0.11.0"','base64 = "=0.23.1"',
'chrono = { version = "=0.4.45", default-features = false, features = ["std", "clock", "serde"] }',
'chrono-tz = { version = "=0.10.4", features = ["serde"] }','rrule = "=0.14.0"',
'reqwest = { version = "=0.13.4", default-features = false, features = ["rustls-no-provider", "json", "stream"] }',
'rustls = { version = "=0.23.35", default-features = false, features = ["ring", "std", "tls12"] }',
'webpki-roots = "=1.0.9"','zip = { version = "=8.6.0", default-features = false, features = ["deflate"] }',
'tar = "=0.4.46"','flate2 = "=1.1.10"','jni = "=0.22.4"','libc = "=0.2.189"',
'hyper = { version = "=1.11.1", default-features = false, features = ["http1", "server"] }',
'hyper-util = { version = "=0.1.20", default-features = false, features = ["tokio", "server", "http1"] }',
'http-body-util = "=0.1.5"','bytes = "=1.12.1"','etherparse = "=0.21.0"',
'rustix = { version = "=1.1.4", features = ["fs", "process", "time"] }'
)
foreach($line in $expectedCargo){ RequireText $cargo $line $line }
Require (-not ($cargo -match '(?m)^\s*[^#\r\n]+\s*=\s*["''][^"'']*[\*^~][^"'']*["'']')) 'dynamic/non-exact Cargo version detected'

# Gradle lock/verification becomes mandatory when I0R reaches handoff.
foreach($lock in @('buildscript-gradle.lockfile','settings-gradle.lockfile','app/gradle.lockfile','gradle/verification-metadata.xml')) {
    Require (Test-Path $lock) "required dependency lock/verification file missing: $lock"
}
$selectedPre = New-Object System.Collections.Generic.List[string]
foreach($lock in @('buildscript-gradle.lockfile','settings-gradle.lockfile','app/gradle.lockfile')) {
    foreach($line in Get-Content $lock) {
        if($line -match '^([^=]+)=') {
            $coord=$Matches[1]
            if($coord -match '(?i)(alpha|beta|(?:^|[.-])rc[0-9.-]*$|[.-]M[0-9]+)'){ $selectedPre.Add($coord) }
        }
    }
}
$allowedPre=@('com.android.tools.build.jetifier:jetifier-core:1.0.0-beta10','com.android.tools.build.jetifier:jetifier-processor:1.0.0-beta10')
$actual=@($selectedPre | Sort-Object -Unique)
foreach($c in $actual){ Require ($allowedPre -contains $c) "unapproved selected pre-release dependency: $c" }
foreach($c in $allowedPre){ Require ($actual -contains $c) "required AGP 9.4.0 Jetifier exception not selected: $c" }

# Cargo.lock is stage-materialized together with the first Rust crate.
if(Test-Path 'rust/crates'){
    Require (Test-Path 'rust/Cargo.lock') 'Cargo.lock required once a Rust product crate exists'
    & cargo +1.98.0 metadata --manifest-path rust/Cargo.toml --format-version 1 --locked --no-deps | Out-Null
    if($LASTEXITCODE -ne 0){ throw 'DEPENDENCY_CHECK_FAILED: Cargo --locked metadata failed' }
} else {
    Require (-not (Test-Path 'rust/Cargo.lock')) 'placeholder Cargo.lock forbidden before first Rust crate'
}

Write-Output 'Dependency/version/feature policy checks OK'
