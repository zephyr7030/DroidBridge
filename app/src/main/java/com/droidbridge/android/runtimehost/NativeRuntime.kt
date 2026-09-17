package com.droidbridge.android.runtimehost

internal object NativeRuntime {
    external fun nativeStart(canonicalBase: String, environmentJson: String): String
    external fun nativeRecoverDeadMagiskHost(
        canonicalBase: String,
        environmentJson: String,
    ): String
    external fun nativeObserveOwner(canonicalBase: String): String
    external fun nativeObserveHostTransition(canonicalBase: String): String
    external fun nativePrepareHostTransition(targetHost: String): String
    external fun nativePrepareRemoteHostTransition(
        canonicalBase: String,
        targetHost: String,
    ): String
    external fun nativeAbortRemoteHostTransition(
        canonicalBase: String,
        intentJson: String,
    ): Boolean
    external fun nativeAbortHostTransition(intentJson: String): Boolean
    external fun nativeReleaseHostTransition(intentJson: String): Boolean
    external fun nativeCommitHostTransition(canonicalBase: String, intentJson: String): String
    external fun nativeFinishHostTransition(
        canonicalBase: String,
        intentJson: String,
        targetInstanceId: String,
    ): Boolean
    external fun nativeFinishCommittedHostTransition(
        canonicalBase: String,
        targetInstanceId: String,
    ): Boolean
    external fun nativeValidateLiveMagiskHost(
        canonicalBase: String,
        targetInstanceId: String,
    ): Boolean
    external fun nativeSubmit(envelope: ByteArray): ByteArray
    external fun nativeQueryArtifacts(query: ByteArray, descriptor: IntArray): ByteArray?
    external fun nativeMcpStart(port: Int, token: String, productVersion: String): Boolean
    external fun nativeMcpSetToken(token: String): Boolean
    external fun nativeMcpStop(): Boolean
    external fun nativeMcpState(): String?
    external fun nativeTunnelStart(
        port: Int,
        tunnelId: String,
        apiKey: String,
        productVersion: String,
    ): Boolean

    external fun nativeTunnelValidate(tunnelId: String, apiKey: String, productVersion: String): String?
    external fun nativeTunnelStop(): Boolean
    external fun nativeTunnelState(): String?
    external fun nativeTunnelLastCall(): Long
    external fun nativeTunnelLastError(): String?
    external fun nativeMaintenanceState(canonicalBase: String): String?
    external fun nativeResetRuntimeHostToApk(canonicalBase: String): String?
    external fun nativeResetRuntimeData(canonicalBase: String): String?
    external fun nativeStrandedExecutions(canonicalBase: String): Int
    external fun nativeClearStrandedExecutions(canonicalBase: String): String?
    external fun nativeCloseAdmissionForMaintenance(): String?
    external fun nativeReopenAdmission(canonicalBase: String): Boolean
    external fun nativeRegisterCapability(
        key: String,
        state: String,
        reason: String,
        sourceGeneration: Long,
        hasExecutor: Boolean,
    ): Boolean
    external fun nativeNetworkDefaultChanged(
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
        subscriptionGeneration: Long,
        sourceGeneration: Long,
        networkId: String,
        transport: String,
    ): Boolean
    external fun nativeAutomationWake(): Boolean
    external fun nativeProbeAppGuard(guardPath: String): Boolean
    external fun nativeProbeCompanionAppGuard(guardPath: String): Boolean
    external fun nativePrepareShizukuGuardProof(executionId: String): Int
    external fun nativeSettleShizukuGuardProof(executionId: String): String
    external fun nativeAbortShizukuGuardProof(executionId: String): Boolean
    external fun nativeRecordHostFault(code: String, phase: String): Boolean
    external fun nativeRunAppCommand(executionId: String, requestJson: String): String?
    external fun nativeCancelAppCommand(executionId: String): Boolean
    external fun nativeAdoptCompanionGuardScope(
        canonicalBase: String,
        runtimeEpoch: String,
        runtimeInstanceId: String,
        guardPath: String,
    ): Boolean
    external fun nativeRunI5DeviceBenchmark(benchmarkBase: String): String
    external fun nativeStop()
}
