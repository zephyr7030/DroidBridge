param(
    [ValidateSet('I3', 'I4', 'I5', 'I6', 'I7', 'I8-FS', 'I8-CMD')][string]$Through,
    [string]$RepositoryRoot
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    Split-Path $PSScriptRoot -Parent
} else {
    [IO.Path]::GetFullPath($RepositoryRoot)
}
Set-Location $root

function Fail([string]$Message) { throw "ARCHITECTURE_CHECK_FAILED: $Message" }
function Require-File([string]$RelativePath) {
    $path = Join-Path $root $RelativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Fail "missing architecture owner: $RelativePath" }
    return $path
}
function Read-Source([string]$RelativePath) {
    return Get-Content -LiteralPath (Require-File $RelativePath) -Raw -Encoding UTF8
}
function Reject-Pattern([string]$Text, [string]$Pattern, [string]$Message) {
    if ($Text -match $Pattern) { Fail $Message }
}
function Require-Pattern([string]$Text, [string]$Pattern, [string]$Message) {
    if ($Text -notmatch $Pattern) { Fail $Message }
}

# Kotlin dependency direction. Missing stage-owned packages are valid before their owning node starts.
$sourceRoots = @('app/src/main/java', 'app/src/main/kotlin') | Where-Object { Test-Path -LiteralPath (Join-Path $root $_) }
$rules = @(
    @{ Path='\ui\'; Forbidden=@('com.droidbridge.android.runtimehost', 'com.droidbridge.android.execution', 'rikka.shizuku') },
    @{ Path='\ui\state\'; Forbidden=@('com.droidbridge.android.runtimehost', 'com.droidbridge.android.execution', 'rikka.shizuku') },
    @{ Path='\product\'; Forbidden=@('com.droidbridge.android.runtimehost', 'com.droidbridge.android.execution', 'rikka.shizuku') },
    @{ Path='\runtimehost\'; Forbidden=@('androidx.compose', 'com.droidbridge.android.ui', 'com.droidbridge.android.product.update') },
    @{ Path='\execution\'; Forbidden=@('androidx.compose', 'com.droidbridge.android.ui', 'com.droidbridge.android.product.update') }
)
foreach ($base in $sourceRoots) {
    foreach ($file in Get-ChildItem -LiteralPath (Join-Path $root $base) -Recurse -File -Filter '*.kt') {
        $normalized = $file.FullName.Replace('/', '\')
        $text = Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8
        foreach ($rule in $rules) {
            if ($normalized -like "*$($rule.Path)*") {
                foreach ($bad in $rule.Forbidden) {
                    if ($text.Contains($bad)) { Fail "illegal Kotlin dependency: $($file.FullName) -> $bad" }
                }
            }
        }
    }
}

# Rust crates remain inside the approved dependency DAG.
$allowedCrates = @('app_native', 'contract', 'daemon', 'domain', 'persistence', 'runtime', 'supervisor')
$allowedEdges = @{
    contract=@()
    domain=@('contract')
    runtime=@('contract', 'domain')
    persistence=@('contract', 'domain', 'runtime')
    app_native=@('contract', 'domain', 'runtime', 'persistence')
    daemon=@('contract', 'domain', 'runtime', 'persistence')
    supervisor=@('contract', 'domain', 'persistence')
}
$cratesRoot = Join-Path $root 'rust/crates'
if (Test-Path -LiteralPath $cratesRoot -PathType Container) {
    $actual = @(Get-ChildItem -LiteralPath $cratesRoot -Directory | ForEach-Object Name)
    foreach ($crate in $actual) {
        if ($allowedCrates -notcontains $crate) { Fail "unexpected Rust product crate: $crate" }
        $toml = Join-Path $cratesRoot "$crate/Cargo.toml"
        if (-not (Test-Path -LiteralPath $toml -PathType Leaf)) { Fail "Rust crate lacks Cargo.toml: $crate" }
        $text = Get-Content -LiteralPath $toml -Raw -Encoding UTF8
        foreach ($other in $allowedCrates) {
            if ($other -eq $crate) { continue }
            if ($text -match "(?m)^\s*$([regex]::Escape($other))\s*(?:\.workspace\s*=\s*true|=\s*\{[^}\r\n]*\bpath\s*=)" -and $allowedEdges[$crate] -notcontains $other) {
                Fail "illegal Rust edge: $crate -> $other"
            }
        }
    }
}

if (-not [string]::IsNullOrWhiteSpace($Through)) {
    $order = @('I3', 'I4', 'I5', 'I6', 'I7', 'I8-FS', 'I8-CMD')
    $throughIndex = [Array]::IndexOf($order, $Through)
    function Includes([string]$Stage) { return $throughIndex -ge [Array]::IndexOf($order, $Stage) }

    if (Includes 'I3') {
        $core = Read-Source 'rust/crates/runtime/src/core.rs'
        Reject-Pattern $core '\b(?:FilesystemCall|CommandCall|NetworkCall|VisualCall|AndroidCall)\b' 'Runtime Core imports a mother-tool call type'
        Reject-Pattern $core '\bhandle_(?:filesystem|command|network|visual|android)_public\b' 'Runtime Core owns mother-tool routing'
        Reject-Pattern $core '\bPublicPayload::(?:Filesystem|Command|Network|Visual|Android)\b' 'Runtime Core matches a mother-tool payload'
        Reject-Pattern $core '\bpub\s+(?:async\s+)?fn\s+submit_public\b' 'Runtime Core owns public ingress'

        $ports = Read-Source 'rust/crates/runtime/src/ports.rs'
        Reject-Pattern $ports '\bfn\s+(?:filesystem|command|network|visual|android)_preflight\b' 'generic ExecutionPort exposes feature preflight'
        $ingress = Read-Source 'rust/crates/runtime/src/ingress.rs'
        Require-Pattern $ingress '\bpub\s+(?:async\s+)?fn\s+submit_public\b' 'thin public ingress is missing'
        $execution = Read-Source 'rust/crates/runtime/src/execution.rs'
        Require-Pattern $execution '\b(?:struct|enum)\s+\w*CompositeExecution\w*\b' 'closed composite execution facade is missing'
        $appAdapterPath = 'rust/crates/app_native/src/lib.rs'
        if (Test-Path -LiteralPath (Join-Path $root $appAdapterPath) -PathType Leaf) {
            $appAdapter = [regex]::Match((Read-Source $appAdapterPath), '(?ms)\bfn\s+submit_apk_public\b.*?(?=^\}|\z)').Value
            Require-Pattern $appAdapter '\bruntime::submit_public\s*\(\s*&host\.core\s*,' 'APK JNI adapter does not call thin public ingress with its Core'
            Reject-Pattern $appAdapter '\.\s*core\s*\.\s*submit_public\s*\(' 'APK JNI adapter calls public submission as a Core method'
        }
    }

    if (Includes 'I4') {
        $rustSources = @(Get-ChildItem -LiteralPath $cratesRoot -Recurse -File -Filter '*.rs' | Where-Object { $_.FullName -match '[\\/]src[\\/]' })
        $planOwners = @($rustSources | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8) -match '\bstruct\s+GuardRecoveryPlan\b' })
        if ($planOwners.Count -ne 1 -or $planOwners[0].FullName -ne (Join-Path $root 'rust/crates/persistence/src/recovery.rs')) {
            Fail 'GuardRecoveryPlan must have exactly one persistence recovery owner'
        }
        $cleanOwners = @($rustSources | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8) -match '\bstruct\s+CleanGuardRecord\b' })
        if ($cleanOwners.Count -ne 1 -or $cleanOwners[0].FullName -ne (Join-Path $root 'rust/crates/persistence/src/recovery.rs')) {
            Fail 'CleanGuardRecord must have exactly one persistence recovery owner'
        }
    }

    if (Includes 'I5') {
        $appNativeSources = @(Get-ChildItem -LiteralPath (Join-Path $root 'rust/crates/app_native/src') -Recurse -File -Filter '*.rs')
        foreach ($file in $appNativeSources) {
            Reject-Pattern (Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8) '\bAppRecoveryPlan\b' 'App owns a host-local recovery plan'
        }
        $appGuard = Read-Source 'rust/crates/app_native/src/app_guard_recovery.rs'
        Require-Pattern $appGuard '\bGuardRecoveryPlan\b' 'App guard adapter does not consume shared recovery'
        $hostController = Read-Source 'app/src/main/java/com/droidbridge/android/runtimehost/RuntimeHostController.kt'
        Require-Pattern $hostController '\bdata\s+class\s+RuntimeSessionState\b' 'coherent Kotlin RuntimeSessionState is missing'
        Require-Pattern $hostController '\b(?:runtimeSession|sessionState)\s*=\s*AtomicReference\b' 'Kotlin session snapshot is not atomically published'
        Reject-Pattern $hostController '(?m)^\s*private\s+val\s+(?:started|host|activeFence|startFailure)\s*=\s*Atomic' 'Kotlin host session authority is split across atomics'
    }

    if (Includes 'I6') {
        $shizuku = Read-Source 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuController.kt'
        Reject-Pattern $shizuku '\bfun\s+(?:executeProcess|executeGuarded|executePackage|executeFs|packageList|packageInspect|packageForceStop|packageInventory|prepareProof|settleProof|createGuardIo|readBounded)\b' 'ShizukuController owns a tool or guard primitive'
        [void](Require-File 'app/src/main/java/com/droidbridge/android/execution/shizuku/ShizukuGuardExecutor.kt')
    }

    if (Includes 'I7') {
        $daemonSources = @(Get-ChildItem -LiteralPath (Join-Path $root 'rust/crates/daemon/src') -Recurse -File -Filter '*.rs')
        foreach ($file in $daemonSources) {
            $text = Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8
            Reject-Pattern $text '\bMagiskRecoveryPlan\b' 'Magisk owns a host-local recovery plan'
            Reject-Pattern $text '\bprior_runtime_instance_ids\b' 'Magisk owns prior-instance selection'
        }
        $magiskGuard = Read-Source 'rust/crates/daemon/src/magisk_guard_recovery.rs'
        Require-Pattern $magiskGuard '\bGuardRecoveryPlan\b' 'Magisk guard adapter does not consume shared recovery'

        $daemonProtocol = Read-Source 'app/src/main/java/com/droidbridge/android/runtimehost/DaemonProtocol.kt'
        Require-Pattern $daemonProtocol '\benum\s+class\s+DaemonMessageKind\b' 'Kotlin daemon message-kind catalog is missing'
        Require-Pattern $daemonProtocol '\benum\s+class\s+DaemonOperationToken\b' 'Kotlin daemon operation catalog is missing'
        $companion = Read-Source 'app/src/main/java/com/droidbridge/android/runtimehost/MagiskCompanionServer.kt'
        Reject-Pattern $companion '"(?:protocol_version|message_id|reply_to|runtime_epoch|host_generation|runtime_instance_id|operation|payload|fd_roles)"' 'Kotlin daemon wire field is outside DaemonProtocol codec'
        Reject-Pattern $companion '"(?:HostActivate|HostStatus|HostPrepareTransition|HostRelease|HostAbortTransition|RuntimeSubmit|RuntimeCancel|DiagnosticsSnapshot|MaintenanceStatus|MaintenanceInstallApk|MaintenanceInstallModule)"' 'Kotlin daemon operation token is outside DaemonProtocol catalog'
    }

    if (Includes 'I8-FS') {
        $vertical = Read-Source 'rust/crates/runtime/src/vertical.rs'
        Reject-Pattern $vertical '\b(?:FilesystemCall|FilesystemInspect\w*|inspect_app_path|inspect_directory|map_fs_error)\b|filesystem\.inspect' 'proof-only filesystem implementation remains in Runtime vertical'
        $filesystemPath = if (Test-Path -LiteralPath (Join-Path $root 'rust/crates/runtime/src/filesystem/mod.rs') -PathType Leaf) {
            'rust/crates/runtime/src/filesystem/mod.rs'
        } else {
            'rust/crates/runtime/src/filesystem.rs'
        }
        $filesystem = Read-Source $filesystemPath
        Require-Pattern $filesystem '\bfilesystem_preflight\b' 'filesystem-local preflight owner is missing'
    }

    if (Includes 'I8-CMD') {
        # One shared Command semantic handler owns the public result and error semantics,
        # and exactly the two host surfaces implement it (S-AUTH-CMD-001).
        $commandPath = 'rust/crates/runtime/src/command.rs'
        $command = Read-Source $commandPath
        Require-Pattern $command '\btrait\s+CommandProcessPort\b' 'shared Command process port is missing from Runtime'
        Require-Pattern $command '\bfn\s+command_executor_request\b' 'shared Command handler is missing from Runtime'
        Require-Pattern $command '\bfn\s+android_command_failure\b' 'shared Command failure classifier is missing'
        Require-Pattern $command '\bfn\s+decode_android_command_settlement\b' 'shared Command settlement decoder is missing'
        Require-Pattern $command '\bfn\s+command_settlement_bound_bytes\b' 'Command settlement bound owner is missing'

        $commandSources = @(Get-ChildItem -LiteralPath $cratesRoot -Recurse -File -Filter '*.rs' | Where-Object { $_.FullName -match '[\\/]src[\\/]' })
        $portOwners = @($commandSources | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8) -match '\btrait\s+CommandProcessPort\b' })
        if ($portOwners.Count -ne 1 -or $portOwners[0].FullName -ne (Join-Path $root $commandPath)) {
            Fail 'the shared Command process port must have exactly one Runtime owner'
        }
        $surfaceOwners = @($commandSources | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8) -match '\bimpl\s+CommandProcessPort\s+for\b' })
        if ($surfaceOwners.Count -ne 2) {
            Fail "exactly two host Command surfaces implement the shared port, found $($surfaceOwners.Count)"
        }
        $boundOwners = @($commandSources | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8) -match '\bfn\s+command_settlement_bound_bytes\b' })
        if ($boundOwners.Count -ne 1 -or $boundOwners[0].FullName -ne (Join-Path $root $commandPath)) {
            Fail 'the Command settlement bound must have exactly one Runtime owner'
        }

        # An unverified cleanup is reported through the collapse to INTERNAL_ERROR, so the
        # closed error-code catalog never gains a member for it.
        $errorCodes = Read-Source 'rust/crates/contract/src/common.rs'
        Reject-Pattern $errorCodes '\bCLEANUP_UNVERIFIED\b' 'the closed error-code catalog gained an unverified-cleanup member'

        # Each surface owns exactly the identities S-AUTH-CMD-001 gives it, and a request
        # it cannot serve is rejected rather than served by another identity's runner.
        $appCommand = Read-Source 'rust/crates/app_native/src/command.rs'
        Require-Pattern $appCommand '\bguard::run_guarded_command\b' 'the APK surface does not run the App identity in its own guard runner'
        Require-Pattern $appCommand 'ShizukuProcessStart' 'the APK surface does not use the Shizuku process primitive for the shell identity'
        Require-Pattern $appCommand '\bErrorCode::RunAsUnavailable\b' 'the APK surface does not reject an identity it does not own'
        Reject-Pattern $appCommand '\b(?:setuid|setgid|setresuid)\b' 'the APK surface impersonates an identity it does not own'

        $daemonCommand = Read-Source 'rust/crates/daemon/src/command.rs'
        Require-Pattern $daemonCommand '\bguard_path\(module_root' 'the Magisk surface does not bind its own root guard'
        Require-Pattern $daemonCommand '\bandroid_command_failure\b' 'the Magisk surface does not use the shared Command failure classifier'
        Require-Pattern $daemonCommand '\bAndroidCommandSettlement::decode\b' 'the Magisk surface does not decode the shared Command settlement'
        foreach ($primitive in @('AppProcessStart', 'AppProcessCancel', 'ShizukuProcessStart', 'ShizukuProcessCancel')) {
            Require-Pattern $daemonCommand $primitive "the Magisk surface does not forward the $primitive primitive"
        }
        Reject-Pattern $daemonCommand '\b(?:setuid|setgid|setresuid)\b' 'the Magisk surface impersonates a forwarded identity'

        # Shizuku stays a primitive provider: no third Command implementation exists in
        # the Shizuku package, and the App surface routes its primitives through the
        # registration the live host generation owns.
        $shizukuRoot = Join-Path $root 'app/src/main/java/com/droidbridge/android/execution/shizuku'
        foreach ($file in Get-ChildItem -LiteralPath $shizukuRoot -Recurse -File -Filter '*.kt') {
            Reject-Pattern (Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8) '\bCommand(?:Process\w*|Result|Settlement)\b' 'the Shizuku package owns a Command implementation'
        }
        $graph = Read-Source 'app/src/main/java/com/droidbridge/android/runtimehost/RuntimeProcessGraph.kt'
        foreach ($primitive in @('AppProcessStart', 'AppProcessCancel', 'ShizukuProcessStart', 'ShizukuProcessCancel')) {
            Require-Pattern $graph $primitive "the App surface does not route the $primitive primitive"
        }
    }
}

$suffix = if ([string]::IsNullOrWhiteSpace($Through)) { '' } else { " through $Through" }
Write-Output "Architecture checks OK$suffix"
