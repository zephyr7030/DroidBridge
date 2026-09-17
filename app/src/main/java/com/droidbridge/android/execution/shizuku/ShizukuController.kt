package com.droidbridge.android.execution.shizuku

import android.content.ComponentName
import android.content.pm.PackageManager
import android.os.Binder
import android.os.SystemClock
import com.droidbridge.android.BuildConfig
import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import java.io.File
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import org.json.JSONObject
import rikka.shizuku.Shizuku
import rikka.shizuku.ShizukuProvider

internal class ShizukuController(
    private val application: android.app.Application,
    private val registry: AndroidExecutionRegistry,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val scope: CoroutineScope,
) {
    private inner class Session(
        override val generation: Long,
        override val remote: IShizukuUserService,
        override val token: Binder,
        override val lost: CompletableDeferred<Unit> = CompletableDeferred(),
        var guardState: RegisteredCapabilityState = RegisteredCapabilityState.Unknown,
        var guardReason: String? = "CONNECTING",
        var probeInFlight: Boolean = false,
    ) : ShizukuSessionLease {
        override fun requireCurrent(
            request: AndroidExecutionRequest?,
            allowCleanupControl: Boolean,
        ) = requireCurrentSession(this, request, allowCleanupControl)
    }

    private var connectionGeneration = 0L
    private var probeGeneration = 0L
    private var session: Session? = null
    private var incompatibleUid: Int? = null
    private var activeConnection: android.content.ServiceConnection? = null
    private var activeArgs: Shizuku.UserServiceArgs? = null
    private var cleanupPending = false
    private var cleanupQuarantined = false
    private var started = false
    private val capabilityPublisher = ShizukuCapabilityPublisher(registry) {
        SOURCE_GENERATION.incrementAndGet()
    }
    private val guardPath = File(
        application.applicationInfo.nativeLibraryDir,
        ShizukuLaunchPolicy.GUARD_NAME,
    ).absolutePath
    private val guardExecutor = ShizukuGuardExecutor(
        guardPath,
        application.packageName,
        ::publishCleanupUnverified,
        ::onProofsDrained,
    )
    private val processExecutor = ShizukuProcessExecutor(
        guardPath,
        application.packageName,
        guardExecutor,
    )
    private val packageExecutor = ShizukuPackageExecutor(application.packageName, guardExecutor)
    private val fsExecutor = ShizukuFsClientExecutor()

    private val binderReceived = Shizuku.OnBinderReceivedListener {
        clearIncompatibleIdentity()
        refresh()
    }
    private val binderDead = Shizuku.OnBinderDeadListener { invalidateAndRefresh() }
    private val permissionResult = Shizuku.OnRequestPermissionResultListener { requestCode, _ ->
        if (requestCode == PERMISSION_REQUEST_CODE) refresh()
    }

    @Volatile
    private var keepAliveWanted = false

    /** Asks the shell-side user service to wake the Runtime if it dies while a connection is enabled. */
    fun setKeepAliveWanted(wanted: Boolean) {
        keepAliveWanted = wanted
        val current = synchronized(this) { session } ?: return
        // The first request runs the shell allowlist commands; keep them off the caller's lock.
        scope.launch { pushKeepAlive(current.remote, current.token) }
    }

    private fun pushKeepAlive(remote: IShizukuUserService, token: Binder) {
        // A user service from an older build of the same version code has no such transaction.
        runCatching { remote.setKeepAlive(token, keepAliveWanted) }
    }

    @Synchronized
    fun start() {
        if (started) return
        started = true
        Shizuku.addBinderReceivedListenerSticky(binderReceived)
        Shizuku.addBinderDeadListener(binderDead)
        Shizuku.addRequestPermissionResultListener(permissionResult)
        ensureBinderRelay(application)
        refresh()
    }

    fun recheck() {
        clearIncompatibleIdentity()
        refresh(forceProbe = true)
    }

    /**
     * Reprobes the shell guard after the guard scope its proofs belong to was replaced. A
     * quarantined session stays quarantined because a probe is never started for it.
     */
    fun onGuardScopeReplaced() {
        refresh(forceProbe = true)
    }

    fun requestAuthorization(): Boolean {
        if (!managerInstalled() || !binderAlive()) {
            refresh()
            return false
        }
        if (authorized()) {
            refresh()
            return true
        }
        return runCatching {
            Shizuku.requestPermission(PERMISSION_REQUEST_CODE)
            true
        }.getOrDefault(false)
    }

    @Synchronized
    fun stop() {
        if (!started) return
        started = false
        disconnect()
        Shizuku.removeBinderReceivedListener(binderReceived)
        Shizuku.removeBinderDeadListener(binderDead)
        Shizuku.removeRequestPermissionResultListener(permissionResult)
    }

    @Synchronized
    private fun refresh(forceProbe: Boolean = false) {
        if (!started) return
        val observation = observe(connecting = false)
        val current = session
        if (!observation.managerInstalled || !observation.binderAlive || !observation.authorized) {
            disconnect()
            publish(observation, null, RegisteredCapabilityState.Unavailable, observationReason(observation))
            return
        }
        if (cleanupPending) {
            publish(
                observe(connecting = true),
                null,
                RegisteredCapabilityState.Unknown,
                "CONNECTING",
            )
            return
        }
        if (current != null) {
            if (cleanupQuarantined) {
                publish(
                    observation.copy(userServiceUid = SHELL_UID),
                    null,
                    RegisteredCapabilityState.Unavailable,
                    "CLEANUP_UNVERIFIED",
                )
                return
            }
            if (forceProbe && !current.probeInFlight) {
                current.guardState = RegisteredCapabilityState.Unknown
                current.guardReason = "CONNECTING"
                capabilityPublisher.publishGuard(current.guardState, current.guardReason)
            }
            startProbe(current, forceProbe)
            return
        }
        incompatibleUid?.let { uid ->
            publish(
                observation.copy(userServiceUid = uid),
                null,
                RegisteredCapabilityState.Unavailable,
                "INCOMPATIBLE_IDENTITY",
            )
            return
        }
        if (activeConnection != null) {
            publish(
                observe(connecting = true),
                null,
                RegisteredCapabilityState.Unknown,
                "CONNECTING",
            )
            return
        }
        bind()
    }

    @Synchronized
    private fun bind() {
        disconnect()
        val generation = ++connectionGeneration
        val args = Shizuku.UserServiceArgs(
            ComponentName(application, DroidBridgeShizukuUserService::class.java),
        )
            .daemon(true)
            .processNameSuffix("droidbridge_shizuku")
            .tag(USER_SERVICE_TAG)
            .version(USER_SERVICE_VERSION)
        val connection = object : android.content.ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: android.os.IBinder) {
                val connected = this
                scope.launch { acceptConnection(generation, connected, args, binder) }
            }

            override fun onServiceDisconnected(name: ComponentName) = loseConnection(generation)
            override fun onBindingDied(name: ComponentName) = loseConnection(generation)
            override fun onNullBinding(name: ComponentName) = loseConnection(generation)
        }
        activeConnection = connection
        activeArgs = args
        publish(
            observe(connecting = true),
            null,
            RegisteredCapabilityState.Unknown,
            "CONNECTING",
        )
        runCatching { Shizuku.bindUserService(args, connection) }
            .onFailure {
                if (generation == connectionGeneration) {
                    activeConnection = null
                    activeArgs = null
                    publish(
                        observe(connecting = false).copy(userServiceFailed = true),
                        null,
                        RegisteredCapabilityState.Unavailable,
                        "BINDER_UNAVAILABLE",
                    )
                }
            }
    }

    private suspend fun acceptConnection(
        generation: Long,
        connection: android.content.ServiceConnection,
        args: Shizuku.UserServiceArgs,
        binder: android.os.IBinder,
    ) {
        if (!isCurrentConnection(generation, connection)) return
        val remote = IShizukuUserService.Stub.asInterface(binder)
        val uid = runCatching { remote.uid }.getOrElse {
            loseConnection(generation)
            return
        }
        if (uid != SHELL_UID) {
            val remove = synchronized(this) {
                if (!isCurrentConnection(generation, connection)) {
                    false
                } else {
                    connectionGeneration++
                    activeConnection = null
                    activeArgs = null
                    incompatibleUid = uid
                    publish(
                        observe(connecting = false).copy(userServiceUid = uid),
                        null,
                        RegisteredCapabilityState.Unavailable,
                        "INCOMPATIBLE_IDENTITY",
                    )
                    true
                }
            }
            if (remove) runCatching { Shizuku.unbindUserService(args, connection, true) }
            return
        }
        val token = Binder()
        val clientId = UUID.randomUUID().toString()
        val attached = runCatching { remote.attachClient(token, clientId) }.isSuccess
        if (!attached) {
            val remove = synchronized(this) {
                if (!isCurrentConnection(generation, connection)) {
                    false
                } else {
                    connectionGeneration++
                    activeConnection = null
                    activeArgs = null
                    publish(
                        observe(connecting = false).copy(userServiceFailed = true),
                        null,
                        RegisteredCapabilityState.Unavailable,
                        "BINDER_UNAVAILABLE",
                    )
                    true
                }
            }
            if (remove) runCatching { Shizuku.unbindUserService(args, connection, true) }
            return
        }
        val accepted = synchronized(this) {
            if (!isCurrentConnection(generation, connection)) {
                false
            } else {
                val next = Session(generation, remote, token)
                session = next
                publish(
                    observe(connecting = false).copy(userServiceUid = SHELL_UID),
                    bridge(next).takeUnless { cleanupQuarantined },
                    if (cleanupQuarantined) {
                        RegisteredCapabilityState.Unavailable
                    } else {
                        RegisteredCapabilityState.Unknown
                    },
                    if (cleanupQuarantined) "CLEANUP_UNVERIFIED" else "CONNECTING",
                )
                true
            }
        }
        if (!accepted) {
            runCatching { remote.detachClient(token) }
            return
        }
        pushKeepAlive(remote, token)
        session?.takeIf { it.generation == generation && !cleanupQuarantined }?.let {
            startProbe(it, force = false)
        }
    }

    @Synchronized
    private fun startProbe(candidate: Session, force: Boolean) {
        if (session !== candidate || !started || cleanupQuarantined || candidate.probeInFlight) return
        if (!force && candidate.guardState != RegisteredCapabilityState.Unknown) return
        candidate.probeInFlight = true
        val probe = ++probeGeneration
        scope.launch { probe(candidate, probe) }
    }

    private suspend fun probe(candidate: Session, probe: Long) {
        val result = runCatching { guardExecutor.probe(candidate) }
            .getOrDefault(ShizukuProbeSettlement.Unavailable)
        when (result) {
            ShizukuProbeSettlement.Available ->
                publishProbe(candidate, probe, RegisteredCapabilityState.Available, null)
            ShizukuProbeSettlement.Unavailable ->
                publishProbe(candidate, probe, RegisteredCapabilityState.Unavailable, "GUARD_PROBE_FAILED")
            ShizukuProbeSettlement.CleanupUnverified ->
                publishProbe(candidate, probe, RegisteredCapabilityState.Unavailable, "CLEANUP_UNVERIFIED")
        }
    }

    private fun bridge(candidate: Session) = AndroidExecutionBridge { request ->
        execute(candidate, request)
    }

    /**
     * The bridge the App surface routes its shell process primitives through. It resolves
     * the live session on every call, so a session that has already been replaced is
     * rejected by that session's own authority check rather than served by a stale lease.
     */
    fun primitiveBridge(): AndroidExecutionBridge = AndroidExecutionBridge { request ->
        val candidate = synchronized(this) { session }
            ?: throw ShizukuExecutionException("STALE_AUTHORITY")
        execute(candidate, request)
    }

    private suspend fun execute(
        candidate: Session,
        request: AndroidExecutionRequest,
    ): AndroidExecutionResult {
        try {
            candidate.requireCurrent(
                request,
                allowCleanupControl = ShizukuPrimitivePolicy.isCleanupControl(request.primitive),
            )
            return when (request.primitive) {
                AndroidPrimitive.ShizukuBind -> {
                    require(request.descriptors.isEmpty())
                    AndroidExecutionResult(
                        JSONObject().put("uid", SHELL_UID).toString().toByteArray(),
                    )
                }
                AndroidPrimitive.ShizukuProcessStart -> processExecutor.start(candidate, request)
                AndroidPrimitive.ShizukuProcessCancel -> processExecutor.cancel(candidate, request)
                AndroidPrimitive.ShizukuPackagePrimitive -> packageExecutor.execute(candidate, request)
                AndroidPrimitive.ShizukuFsPrimitive -> fsExecutor.execute(candidate, request)
                else -> throw ShizukuExecutionException("UNSUPPORTED")
            }
        } finally {
            request.descriptors.forEach { descriptor ->
                runCatching { descriptor.descriptor.close() }
            }
        }
    }

    @Synchronized
    private fun publishProbe(
        candidate: Session,
        probe: Long,
        guardState: RegisteredCapabilityState,
        reason: String?,
    ) {
        if (session !== candidate || probeGeneration != probe || !started) return
        candidate.probeInFlight = false
        candidate.guardState = guardState
        candidate.guardReason = reason
        if (reason == "CLEANUP_UNVERIFIED") cleanupQuarantined = true
        if (cleanupQuarantined) {
            publish(
                observe(connecting = false).copy(userServiceUid = SHELL_UID),
                null,
                guardState,
                reason,
            )
        } else {
            capabilityPublisher.publishGuard(guardState, reason)
        }
    }

    @Synchronized
    private fun publishCleanupUnverified(candidate: ShizukuSessionLease) {
        cleanupQuarantined = true
        val current = candidate as? Session ?: return
        if (!started || session !== current) return
        current.guardState = RegisteredCapabilityState.Unavailable
        current.guardReason = "CLEANUP_UNVERIFIED"
        current.probeInFlight = false
        publish(
            observe(connecting = false).copy(userServiceUid = SHELL_UID),
            bridge(current),
            RegisteredCapabilityState.Unavailable,
            "CLEANUP_UNVERIFIED",
        )
    }

    private fun onProofsDrained() {
        val refreshAfterCleanup = synchronized(this) {
            if (cleanupPending && !guardExecutor.hasActiveProofs()) {
                cleanupPending = false
                started
            } else {
                false
            }
        }
        if (refreshAfterCleanup) scope.launch { refresh() }
    }

    @Synchronized
    private fun requireCurrentSession(
        candidate: Session,
        request: AndroidExecutionRequest?,
        allowCleanupControl: Boolean,
    ) {
        if (!started || session !== candidate || cleanupPending ||
            (!allowCleanupControl && cleanupQuarantined) ||
            !binderAlive() || !authorized()
        ) {
            throw ShizukuExecutionException("STALE_AUTHORITY")
        }
        if (request == null) return
        if (!validatesFence(
                request.runtimeEpoch,
                request.hostGeneration,
                request.runtimeInstanceId,
            )
        ) {
            throw ShizukuExecutionException("STALE_AUTHORITY")
        }
        if (!allowCleanupControl && !ShizukuPrimitivePolicy.isAdmitted(
                request.primitive,
                candidate.guardState,
                cleanupQuarantined,
            )
        ) {
            throw ShizukuExecutionException(
                if (cleanupQuarantined) "CLEANUP_UNVERIFIED" else "CAPABILITY_UNAVAILABLE",
            )
        }
    }

    @Synchronized
    private fun isCurrentConnection(
        generation: Long,
        connection: android.content.ServiceConnection,
    ): Boolean = started && generation == connectionGeneration && activeConnection === connection

    private fun loseConnection(generation: Long) {
        synchronized(this) {
            if (generation != connectionGeneration) return
            disconnect()
            if (started) {
                publish(
                    observe(connecting = false).copy(userServiceFailed = true),
                    null,
                    RegisteredCapabilityState.Unavailable,
                    "BINDER_UNAVAILABLE",
                )
            }
        }
    }

    private fun invalidateAndRefresh() {
        synchronized(this) { disconnect() }
        refresh()
    }

    @Synchronized
    private fun clearIncompatibleIdentity() {
        incompatibleUid = null
    }

    @Synchronized
    private fun disconnect() {
        connectionGeneration++
        probeGeneration++
        session?.let { current ->
            runCatching { current.remote.detachClient(current.token) }
            current.lost.complete(Unit)
        }
        if (guardExecutor.hasActiveProofs()) cleanupPending = true
        val connection = activeConnection
        val args = activeArgs
        if (connection != null && args != null) {
            runCatching { Shizuku.unbindUserService(args, connection, false) }
        }
        session = null
        incompatibleUid = null
        activeConnection = null
        activeArgs = null
    }

    private fun observe(connecting: Boolean): ShizukuObservation = ShizukuObservation(
        managerInstalled = managerInstalled(),
        binderAlive = binderAlive(),
        authorized = authorized(),
        userServiceUid = session?.let { SHELL_UID } ?: incompatibleUid,
        connecting = connecting,
        cleanupQuarantined = cleanupQuarantined,
    )

    private fun managerInstalled(): Boolean = runCatching {
        application.packageManager.getApplicationInfo(
            MANAGER_PACKAGE,
            PackageManager.ApplicationInfoFlags.of(0),
        )
    }.isSuccess

    private fun binderAlive(): Boolean = runCatching { Shizuku.pingBinder() }.getOrDefault(false)

    private fun authorized(): Boolean = binderAlive() &&
        runCatching { Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED }
            .getOrDefault(false)

    private fun observationReason(observation: ShizukuObservation): String =
        ShizukuCapabilityProjector.project(observation).reason ?: "BINDER_UNAVAILABLE"

    private fun publish(
        observation: ShizukuObservation,
        executor: AndroidExecutionBridge?,
        guardState: RegisteredCapabilityState,
        guardReason: String?,
    ) {
        capabilityPublisher.publishProvider(observation, executor)
        capabilityPublisher.publishGuard(guardState, guardReason)
    }

    companion object {
        private const val MANAGER_PACKAGE = "moe.shizuku.privileged.api"
        private const val PERMISSION_REQUEST_CODE = 7_041
        private const val USER_SERVICE_TAG = "droidbridge-shizuku-v1"
        private const val USER_SERVICE_VERSION = BuildConfig.VERSION_CODE
        private const val SHELL_UID = 2_000
        private val BINDER_RELAY_INSTALLED = AtomicBoolean(false)
        private val SOURCE_GENERATION = java.util.concurrent.atomic.AtomicLong(
            SystemClock.elapsedRealtimeNanos(),
        )

        private fun ensureBinderRelay(application: android.app.Application) {
            if (!BINDER_RELAY_INSTALLED.compareAndSet(false, true)) return
            runCatching { ShizukuProvider.requestBinderForNonProviderProcess(application) }
                .onFailure { BINDER_RELAY_INSTALLED.set(false) }
        }
    }
}
