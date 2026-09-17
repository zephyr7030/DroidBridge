package com.droidbridge.android.execution.shizuku

import android.os.Binder
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import java.math.BigInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.selects.select

internal interface ShizukuSessionLease {
    val generation: Long
    val remote: IShizukuUserService
    val token: Binder
    val lost: CompletableDeferred<Unit>

    fun requireCurrent(
        request: AndroidExecutionRequest? = null,
        allowCleanupControl: Boolean = false,
    )
}

internal suspend fun <T> ShizukuSessionLease.awaitCompletion(
    completion: CompletableDeferred<T>,
): T = select {
    completion.onAwait { it }
    lost.onAwait {
        val error = ShizukuExecutionException("STALE_AUTHORITY")
        completion.completeExceptionally(error)
        throw error
    }
}

internal class ShizukuExecutionException(code: String) : AndroidExecutionException(code)

internal data class ShizukuObservation(
    val managerInstalled: Boolean,
    val binderAlive: Boolean,
    val authorized: Boolean,
    val userServiceUid: Int?,
    val connecting: Boolean,
    val userServiceFailed: Boolean = false,
    val cleanupQuarantined: Boolean = false,
)

internal enum class ShizukuProviderState {
    NotInstalled,
    NotRunning,
    NotAuthorized,
    Connecting,
    Connected,
    IncompatibleIdentity,
}

internal data class ShizukuCapabilityProjection(
    val providerState: ShizukuProviderState,
    val registrationState: RegisteredCapabilityState,
    val reason: String?,
    val exposesExecutor: Boolean,
)

internal object ShizukuCapabilityProjector {
    fun project(observation: ShizukuObservation): ShizukuCapabilityProjection {
        val providerState = when {
            !observation.managerInstalled -> ShizukuProviderState.NotInstalled
            !observation.binderAlive -> ShizukuProviderState.NotRunning
            !observation.authorized -> ShizukuProviderState.NotAuthorized
            observation.userServiceFailed -> ShizukuProviderState.NotRunning
            observation.connecting || observation.userServiceUid == null -> ShizukuProviderState.Connecting
            observation.userServiceUid == SHELL_UID -> ShizukuProviderState.Connected
            else -> ShizukuProviderState.IncompatibleIdentity
        }
        val registrationState = when (providerState) {
            ShizukuProviderState.Connected -> RegisteredCapabilityState.Available
            ShizukuProviderState.Connecting -> RegisteredCapabilityState.Unknown
            else -> RegisteredCapabilityState.Unavailable
        }
        val reason = when (providerState) {
            ShizukuProviderState.NotInstalled -> "MANAGER_NOT_INSTALLED"
            ShizukuProviderState.NotRunning -> "BINDER_UNAVAILABLE"
            ShizukuProviderState.NotAuthorized -> "GRANT_MISSING"
            ShizukuProviderState.Connecting -> "CONNECTING"
            ShizukuProviderState.IncompatibleIdentity -> "INCOMPATIBLE_IDENTITY"
            ShizukuProviderState.Connected -> null
        }
        return ShizukuCapabilityProjection(
            providerState = providerState,
            registrationState = registrationState,
            reason = reason,
            exposesExecutor = providerState == ShizukuProviderState.Connected &&
                !observation.cleanupQuarantined,
        )
    }

    private const val SHELL_UID = 2_000
}

internal class ShizukuCapabilityPublisher(
    private val registry: AndroidExecutionRegistry,
    private val nextGeneration: () -> Long,
) {
    fun publishProvider(
        observation: ShizukuObservation,
        executor: AndroidExecutionBridge?,
    ) {
        val projection = ShizukuCapabilityProjector.project(observation)
        registry.register(
            CapabilityRegistration(
                key = "shizuku.shell",
                state = projection.registrationState,
                reason = projection.reason,
                sourceGeneration = nextGeneration(),
                executor = executor.takeIf { projection.exposesExecutor },
            ),
        )
    }

    fun publishGuard(state: RegisteredCapabilityState, reason: String?) {
        registry.register(
            CapabilityRegistration(
                key = "execution.shell_guard",
                state = state,
                reason = reason,
                sourceGeneration = nextGeneration(),
            ),
        )
    }
}

internal class ExecutionOwnershipRegistry<Client : Any, Handle : Any>(
    private val limit: Int,
) {
    private data class Owner<Client>(val client: Client, val executionId: String)

    private val clients = mutableSetOf<Client>()
    private val executions = linkedMapOf<Owner<Client>, Handle>()

    init {
        require(limit > 0)
    }

    @Synchronized
    fun attach(client: Client) {
        clients += client
    }

    @Synchronized
    fun admit(client: Client, executionId: String, handle: Handle): Boolean {
        if (client !in clients || executionId.isEmpty() || executions.size >= limit) return false
        val owner = Owner(client, executionId)
        if (owner in executions) return false
        executions[owner] = handle
        return true
    }

    @Synchronized
    fun ownedHandle(client: Client, executionId: String): Handle? = executions[Owner(client, executionId)]

    @Synchronized
    fun remove(client: Client, executionId: String): Handle? = executions.remove(Owner(client, executionId))

    @Synchronized
    fun detach(client: Client): Set<Handle> {
        clients -= client
        val owned = executions.filterKeys { it.client == client }
        owned.keys.forEach(executions::remove)
        return owned.values.toSet()
    }

    @get:Synchronized
    val activeCount: Int
        get() = executions.size
}

internal enum class ShizukuProbeSettlement {
    Available,
    Unavailable,
    CleanupUnverified,
}

internal object ShizukuLaunchPolicy {
    fun isAdmitted(
        effectiveUid: Int,
        callerUid: Int,
        packageUid: Int,
        nativeDirectory: String,
        guardPath: String,
        activeCount: Int,
    ): Boolean =
        effectiveUid == SHELL_UID &&
            callerUid == packageUid &&
            activeCount in 0 until MAX_GUARDS &&
            nativeDirectory.startsWith('/') &&
            guardPath == nativeDirectory.trimEnd('/') + "/$GUARD_NAME"

    fun settleProbe(cleanupVerified: Boolean, shellExitCode: Int?): ShizukuProbeSettlement = when {
        !cleanupVerified -> ShizukuProbeSettlement.CleanupUnverified
        shellExitCode == 0 -> ShizukuProbeSettlement.Available
        else -> ShizukuProbeSettlement.Unavailable
    }

    const val MAX_GUARDS = 64
    const val GUARD_NAME = "libdroidbridge_exec_guard.so"
    private const val SHELL_UID = 2_000
}

internal object ShizukuPrimitivePolicy {
    fun isCleanupControl(primitive: AndroidPrimitive): Boolean =
        primitive == AndroidPrimitive.ShizukuBind ||
            primitive == AndroidPrimitive.ShizukuProcessCancel

    fun requiresGuard(primitive: AndroidPrimitive): Boolean =
        primitive == AndroidPrimitive.ShizukuProcessStart ||
            primitive == AndroidPrimitive.ShizukuPackagePrimitive

    fun isAdmitted(
        primitive: AndroidPrimitive,
        guardState: RegisteredCapabilityState,
        cleanupQuarantined: Boolean,
    ): Boolean = when {
        isCleanupControl(primitive) -> true
        cleanupQuarantined -> false
        requiresGuard(primitive) -> guardState == RegisteredCapabilityState.Available
        else -> true
    }
}

internal data class ShizukuPackageFact(
    val packageName: String,
    val versionCode: BigInteger,
    val system: Boolean,
)

internal object ShizukuPackageParser {
    private val record = Regex("package:([^ ]+) versionCode:([0-9]+)")

    fun parse(lines: List<String>, system: Boolean): List<ShizukuPackageFact> {
        val facts = linkedMapOf<String, ShizukuPackageFact>()
        lines.filter(String::isNotBlank).forEach { line ->
            val match = record.matchEntire(line) ?: throw IllegalArgumentException("malformed package record")
            val name = match.groupValues[1]
            require(name.isNotEmpty() && name.toByteArray(Charsets.UTF_8).size <= 255 && '\u0000' !in name)
            val version = match.groupValues[2].toBigIntegerOrNull()
                ?.takeIf { it <= MAX_U64 }
                ?: throw IllegalArgumentException("invalid package version")
            val fact = ShizukuPackageFact(name, version, system)
            val previous = facts.putIfAbsent(name, fact)
            require(previous == null || previous == fact) { "conflicting duplicate package record" }
        }
        return facts.values.sortedBy(ShizukuPackageFact::packageName)
    }

    private val MAX_U64 = BigInteger("18446744073709551615")
}
