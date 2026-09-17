$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path $PSScriptRoot -Parent
$checker = Join-Path $PSScriptRoot 'check-architecture.ps1'
$tmpParent = [IO.Path]::GetFullPath((Join-Path $root 'build/tmp'))
$fixture = [IO.Path]::GetFullPath((Join-Path $tmpParent ("architecture-gate-" + [guid]::NewGuid().ToString('N'))))
$script:assertions = 0

function Assert-True([bool]$Condition, [string]$Message) {
    $script:assertions++
    if (-not $Condition) { throw "ARCHITECTURE_GATE_TEST_FAILED: $Message" }
}

function Write-Fixture([string]$RelativePath, [string]$Content) {
    $path = Join-Path $fixture $RelativePath
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    [IO.File]::WriteAllText($path, $Content, [Text.UTF8Encoding]::new($false))
}

function Invoke-Gate([string]$Through) {
    try { & $checker -RepositoryRoot $fixture -Through $Through | Out-Null }
    finally { Set-Location $root }
}

function Assert-Rejected([scriptblock]$Mutation, [scriptblock]$Restore, [string]$Message) {
    & $Mutation
    $rejected = $false
    try { Invoke-Gate 'I8-FS' } catch { $rejected = $true }
    try { Assert-True $rejected $Message } finally { & $Restore }
}

[IO.Directory]::CreateDirectory($fixture) | Out-Null
try {
    foreach ($crate in @('runtime', 'persistence', 'app_native', 'daemon')) {
        Write-Fixture "rust/crates/$crate/Cargo.toml" "[package]`nname = `"$crate`"`nversion = `"0.0.0`"`n"
    }
    Write-Fixture 'rust/crates/runtime/src/core.rs' 'pub struct RuntimeCore;'
    Write-Fixture 'rust/crates/runtime/src/ports.rs' 'pub trait ExecutionPort { fn claim_and_start(&self); fn cancel(&self); }'
    Write-Fixture 'rust/crates/runtime/src/ingress.rs' 'pub fn submit_public() {}'
    Write-Fixture 'rust/crates/runtime/src/execution.rs' 'pub struct CompositeExecutionSurface;'
    Write-Fixture 'rust/crates/runtime/src/vertical.rs' 'pub fn runtime_vertical_probe() {}'
    Write-Fixture 'rust/crates/runtime/src/filesystem.rs' 'pub fn filesystem_preflight() {}'
    Write-Fixture 'rust/crates/persistence/src/recovery.rs' 'pub struct GuardRecoveryPlan; pub struct CleanGuardRecord;'
    Write-Fixture 'rust/crates/app_native/src/app_guard_recovery.rs' 'use persistence::recovery::GuardRecoveryPlan;'
    $appAdapter = 'mod app_guard_recovery; fn submit_apk_public(host: &NativeHost, encoded: &[u8]) { runtime::submit_public(&host.core, encoded, ended_at, now_ms, dispatch_installed); }'
    Write-Fixture 'rust/crates/app_native/src/lib.rs' $appAdapter
    Write-Fixture 'rust/crates/daemon/src/magisk_guard_recovery.rs' 'use persistence::recovery::GuardRecoveryPlan;'
    Write-Fixture 'rust/crates/daemon/src/process.rs' 'mod magisk_host;'
    Write-Fixture 'rust/crates/daemon/src/lib.rs' 'pub enum DaemonOperation { HostActivate }'
    Write-Fixture 'app/src/main/java/com/droidbridge/android/runtimehost/RuntimeHostController.kt' 'data class RuntimeSessionState(val started: Boolean, val host: String, val activeFence: Any?, val startFailure: String); private val runtimeSession = AtomicReference(RuntimeSessionState(false, "none", null, "RUNTIME_UNAVAILABLE"))'
    Write-Fixture 'app/src/main/java/com/droidbridge/android/runtimehost/DaemonProtocol.kt' 'enum class DaemonMessageKind { Request, Response, Cancel }; enum class DaemonOperationToken { HostActivate }; internal object DaemonProtocol'
    Write-Fixture 'app/src/main/java/com/droidbridge/android/runtimehost/MagiskCompanionServer.kt' 'internal class MagiskCompanionServer'
    Write-Fixture 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuController.kt' 'internal class ShizukuController { fun connect() = Unit }'
    Write-Fixture 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuGuardExecutor.kt' 'internal class ShizukuGuardExecutor'

    Invoke-Gate 'I8-FS'
    Assert-True $true 'conforming fixture was rejected'

    Assert-Rejected {
        Write-Fixture 'rust/crates/app_native/src/lib.rs' ($appAdapter.Replace('runtime::submit_public(&host.core, ', 'host.core.submit_public('))
    } {
        Write-Fixture 'rust/crates/app_native/src/lib.rs' $appAdapter
    } 'APK JNI Core-method public submission was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/runtime/src/core.rs' 'pub fn handle_filesystem_public() {}'
    } {
        Write-Fixture 'rust/crates/runtime/src/core.rs' 'pub struct RuntimeCore;'
    } 'Core mother-tool routing was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/runtime/src/ports.rs' 'pub trait ExecutionPort { fn filesystem_preflight(&self); }'
    } {
        Write-Fixture 'rust/crates/runtime/src/ports.rs' 'pub trait ExecutionPort { fn claim_and_start(&self); fn cancel(&self); }'
    } 'generic-port feature preflight was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/app_native/src/other.rs' 'pub struct GuardRecoveryPlan;'
    } {
        Remove-Item -LiteralPath (Join-Path $fixture 'rust/crates/app_native/src/other.rs') -Force
    } 'duplicate GuardRecoveryPlan owner was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/app_native/src/lib.rs' ($appAdapter + ' struct AppRecoveryPlan;')
    } {
        Write-Fixture 'rust/crates/app_native/src/lib.rs' $appAdapter
    } 'host-local App recovery plan was accepted'

    Assert-Rejected {
        Write-Fixture 'app/src/main/java/com/droidbridge/android/runtimehost/RuntimeHostController.kt' 'private val started = AtomicBoolean(false); private val host = AtomicReference("none"); private val activeFence = AtomicReference<Any?>(null)'
    } {
        Write-Fixture 'app/src/main/java/com/droidbridge/android/runtimehost/RuntimeHostController.kt' 'data class RuntimeSessionState(val started: Boolean, val host: String, val activeFence: Any?, val startFailure: String); private val runtimeSession = AtomicReference(RuntimeSessionState(false, "none", null, "RUNTIME_UNAVAILABLE"))'
    } 'split Kotlin session projection was accepted'

    Assert-Rejected {
        Write-Fixture 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuController.kt' 'internal class ShizukuController { private suspend fun executeProcess() = Unit }'
    } {
        Write-Fixture 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuController.kt' 'internal class ShizukuController { fun connect() = Unit }'
    } 'Shizuku controller-owned primitive was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/daemon/src/process.rs' 'struct MagiskRecoveryPlan;'
    } {
        Write-Fixture 'rust/crates/daemon/src/process.rs' 'mod magisk_host;'
    } 'host-local Magisk recovery plan was accepted'

    Assert-Rejected {
        Write-Fixture 'rust/crates/runtime/src/vertical.rs' 'fn inspect_app_path() {}'
    } {
        Write-Fixture 'rust/crates/runtime/src/vertical.rs' 'pub fn runtime_vertical_probe() {}'
    } 'proof-only filesystem vertical was accepted'
} finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    if ($resolved.StartsWith($tmpParent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and (Test-Path -LiteralPath $resolved)) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}

Write-Output "ARCHITECTURE_GATE_ASSERTIONS=$script:assertions"
