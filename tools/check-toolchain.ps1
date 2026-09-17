$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
function Require([bool]$ok,[string]$message){ if(-not $ok){ throw "TOOLCHAIN_CHECK_FAILED: $message" } }

$javaText = (& cmd.exe /d /c "java -version 2>&1" | Out-String)
Require ($LASTEXITCODE -eq 0) 'java -version failed'
Require ($javaText -match 'version "17\.') 'JDK 17 required'

$wrapper = Get-Content 'gradle/wrapper/gradle-wrapper.properties' -Raw
Require ($wrapper -match 'gradle-9\.6\.0-bin\.zip') 'Gradle Wrapper 9.6.0 required'
Require ($wrapper -match 'distributionSha256Sum=bbaeb2fef8710818cf0e261201dab964c572f92b942812df0c3620d62a529a01') 'Gradle 9.6.0 distribution checksum required'
$wrapperJarHash = (Get-FileHash 'gradle/wrapper/gradle-wrapper.jar' -Algorithm SHA256).Hash.ToLowerInvariant()
Require ($wrapperJarHash -eq '497c8c2a7e5031f6aa847f88104aa80a93532ec32ee17bdb8d1d2f67a194a9c7') 'Gradle 9.6.0 wrapper JAR checksum mismatch'
$gradleText = (& .\gradlew.bat --version --no-daemon | Out-String)
Require ($LASTEXITCODE -eq 0) 'Gradle version probe failed'
Require ($gradleText -match 'Gradle 9\.6\.0') 'Gradle 9.6.0 required'

$versions = Get-Content 'gradle/libs.versions.toml' -Raw
Require ($versions -match 'agp\s*=\s*"9\.4\.0"') 'AGP 9.4.0 required'
Require ($versions -match 'kotlin\s*=\s*"2\.4\.10"') 'Kotlin 2.4.10 required'

$local = Get-Content 'local.properties' -ErrorAction Stop
$line = ($local | Select-String '^sdk.dir=' | Select-Object -First 1).Line
Require (-not [string]::IsNullOrWhiteSpace($line)) 'local.properties sdk.dir missing'
$raw = $line.Substring(8)
$sdk = $raw -replace '\\:',':' -replace '\\\\','\'
Require (Test-Path "$sdk/platforms/android-37/android.jar") 'Android platform 37 missing'
Require (Test-Path "$sdk/build-tools/36.0.0/aapt2.exe") 'Build Tools 36.0.0 missing'
$ndkProps = Get-Content "$sdk/ndk/29.0.14206865/source.properties" -Raw
Require ($ndkProps -match 'Pkg\.Revision\s*=\s*29\.0\.14206865') 'NDK 29.0.14206865 missing/mismatched'
$cmake = "$sdk/cmake/3.31.6/bin/cmake.exe"
$ninja = "$sdk/cmake/3.31.6/bin/ninja.exe"
Require (Test-Path $cmake) 'CMake 3.31.6 missing'
Require (Test-Path $ninja) 'Ninja 1.12.1 missing'
Require ((& $cmake --version | Select-Object -First 1) -eq 'cmake version 3.31.6') 'CMake version mismatch'
Require ((& $ninja --version | Select-Object -First 1) -eq '1.12.1') 'Ninja version mismatch'
Require ((& rustc +1.98.0 --version) -match '^rustc 1\.98\.0 ') 'Rust 1.98.0 required'
Require ((& cargo +1.98.0 --version) -match '^cargo 1\.98\.0 ') 'Cargo 1.98.0 required'
Require ((& cargo ndk --version) -match 'cargo-ndk 4\.1\.2') 'cargo-ndk 4.1.2 required'

$wf = @(& "$PSScriptRoot/bootstrap-winflexbison.ps1")
Require ($wf.Count -gt 0) 'WinFlexBison bootstrap returned no verified record'
$wfText = $wf | Out-String
Require ($wfText -match '8d324b62be33604b2c45ad1dd34ab93d722534448f55a16ca7292de32b6ac135') 'WinFlexBison verified digest missing'
Require ($wfText -match '2\.6\.4') 'Flex 2.6.4 verification missing'
Require ($wfText -match '3\.8\.2') 'Bison 3.8.2 verification missing'

Write-Output 'Toolchain checks OK'
Write-Output "SDK=$sdk"
$wf | Write-Output
