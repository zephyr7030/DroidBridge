package com.droidbridge.android.runtimehost

import android.Manifest
import android.app.AlarmManager
import android.app.Application
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelFileDescriptor
import com.droidbridge.android.execution.android.NativeAndroidExecutionDispatcher
import com.droidbridge.android.execution.android.NetworkDefaultObservation
import com.droidbridge.android.BuildConfig
import com.droidbridge.android.execution.android.RoleDescriptor
import com.droidbridge.android.product.release.ReleaseConfig
import org.json.JSONObject
import java.io.File
import java.time.ZoneId
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlinx.serialization.json.put

private const val DEAD_HOST_RECOVERY_DELAY_MILLIS = 31_000L
private const val DIAGNOSTICS_DEADLINE_MILLIS = 2_000L
private const val MAINTENANCE_RESET = """{"schema_version":1,"reset":true}"""
private const val PRIVILEGED_INSTALL_TIMEOUT_MILLIS = 330_000L

internal enum class MagiskHostStatusAction {
    Wait,
    EstablishCurrentHost,
    BeginDemotion,
    RecoverDemotion,
}

internal fun decideMagiskHostStatus(
    ready: Boolean,
    cleanupReady: Boolean,
    requiresApkHost: Boolean,
    transitionPresent: Boolean,
    pendingSourceTransition: Boolean,
): MagiskHostStatusAction {
    if (!cleanupReady) return MagiskHostStatusAction.Wait
    if (transitionPresent) {
        if (pendingSourceTransition) return MagiskHostStatusAction.RecoverDemotion
        return if (ready) {
            MagiskHostStatusAction.EstablishCurrentHost
        } else {
            MagiskHostStatusAction.Wait
        }
    }
    if (!ready || requiresApkHost) return MagiskHostStatusAction.BeginDemotion
    return MagiskHostStatusAction.EstablishCurrentHost
}

internal fun controlAcknowledged(payload: JsonObject, field: String): Boolean =
    payload["error"] == null && (payload[field] as? JsonPrimitive)?.booleanOrNull == true

internal data class RuntimeFence(
    val runtimeEpoch: String,
    val hostGeneration: Long,
    val runtimeInstanceId: String,
)

internal data class RuntimeSessionState(
    val started: Boolean = false,
    val host: DaemonHostToken? = null,
    val activeFence: RuntimeFence? = null,
    val startFailure: String = "RUNTIME_UNAVAILABLE",
) {
    init {
        require(started == (activeFence != null))
        require(!started || (host != null && startFailure.isEmpty()))
        require(started || startFailure.isNotEmpty())
    }

    fun validates(
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): Boolean = started && activeFence?.let { fence ->
        fence.runtimeEpoch == runtimeEpoch &&
            fence.hostGeneration == hostGeneration &&
            fence.runtimeInstanceId == runtimeInstanceId
    } == true
}

internal fun shouldAttemptRuntimeStart(session: RuntimeSessionState): Boolean =
    !session.started && session.startFailure != DaemonErrorToken.HostTransitionPending.wire

internal class RuntimeHostController(
    private val application: Application,
) {
    private val runtimeSession = AtomicReference(RuntimeSessionState())
    private val serverStarted = AtomicBoolean(false)
    private val deadHostRecoveryScheduled = AtomicBoolean(false)
    private val demotionRunning = AtomicBoolean(false)
    private val promotionState = HostPromotionState()
    private val promotionConnection = AtomicReference<DaemonConnection?>(null)
    private val hintSink = AtomicReference<((String) -> Unit)?>(null)
    private val frameworkReadySink = AtomicReference<((Long) -> Unit)?>(null)
    private val companionDisconnectedSink = AtomicReference<(() -> Boolean)?>(null)
    private val networkAttachmentSource = AtomicReference<(() -> String?)?>(null)
    private val networkAttachmentRevision = AtomicLong(0)
    private val networkAttachmentScheduled = AtomicBoolean(false)
    private val guardScopeSink = AtomicReference<(() -> Unit)?>(null)
    private val apkProjectionReleasedSink = AtomicReference<(() -> Unit)?>(null)
    private val platformGeneration = AtomicLong(0)
    private val companionCapabilityRevision = AtomicLong(0)
    private val companionCapabilityRefreshScheduled = AtomicBoolean(false)
    private val companionCapabilityFacts = CompanionCapabilityFacts()
    private val transitionExecutor = AtomicReference<ExecutorService?>(null)
    private val lastEnvironment = AtomicReference<String?>(null)
    private val deviceContext = application.createDeviceProtectedStorageContext()
    private val canonicalBase = File(deviceContext.filesDir, "droidbridge")
    private val json = Json { ignoreUnknownKeys = false }

    /** One bounded worker for S-UI-017 status reads; a timed-out read never blocks the next caller. */
    private val diagnosticsReads = Executors.newSingleThreadExecutor { task ->
        Thread(task, "droidbridge-diagnostics").apply { isDaemon = true }
    }
    private val companion = MagiskCompanionServer(
        application.packageName,
        object : DaemonCompanionListener {
            override fun currentOwner(): DaemonOwnerFence = observeOwner()

            override fun onHostStatus(connection: DaemonConnection, payload: JsonObject) {
                handleHostStatus(connection, payload)
            }

            override fun onDaemonRequest(
                request: DaemonWireEnvelope,
                descriptors: List<ParcelFileDescriptor>,
            ): DaemonReplyPayload = when (request.operation) {
                DaemonOperationToken.CapabilitySnapshot -> {
                    require(descriptors.isEmpty())
                    capabilitySnapshot(request.payload)
                }
                DaemonOperationToken.CompanionExecute ->
                    companionExecutionResponse(request, descriptors)
                DaemonOperationToken.CompanionCancel -> {
                    if (descriptors.isEmpty()) {
                        companionCancellationResponse(request)
                    } else {
                        DaemonReplyPayload(
                            companionFailurePayload(DaemonErrorToken.InvalidArgument.wire),
                        )
                    }
                }
                else -> error("invalid daemon operation")
            }

            override fun onDaemonDisconnected(connection: DaemonConnection) {
                promotionConnection.compareAndSet(connection, null)
                promotionState.clearBackend()
                observeModule(ModuleObservation.Absent)
                // Nothing here can still know what a departed daemon runs, and it wakes this
                // process again with the current count as soon as it reconnects.
                NativeAndroidExecutionDispatcher.forgetDaemonTaskActivity()
                if (
                    runtimeSession.get().host == DaemonHostToken.MagiskBackend &&
                    companionDisconnectedSink.get()?.invoke() == false
                ) {
                    NativeRuntime.nativeRecordHostFault(
                        "CLEANUP_UNVERIFIED",
                        "network_default_callback",
                    )
                }
                val withdrawn = runtimeSession.updateAndGet { current ->
                    if (current.started && current.host == DaemonHostToken.MagiskBackend) {
                        inactiveSession(
                            DaemonHostToken.MagiskBackend,
                            DaemonErrorToken.CapabilityUnavailable.wire,
                        )
                    } else {
                        current
                    }
                }
                if (!withdrawn.started && withdrawn.host == DaemonHostToken.MagiskBackend) {
                    hintSink.get()?.invoke("context.status")
                    scheduleDeadHostRecovery()
                }
            }
        },
    )

    @Synchronized
    fun start(): Boolean {
        val observedSession = runtimeSession.get()
        if (observedSession.started) return true
        if (!shouldAttemptRuntimeStart(observedSession)) return false
        val packageInfo = application.packageManager.getPackageInfo(application.packageName, 0)
        val environment = JSONObject()
            .put("sdk_int", Build.VERSION.SDK_INT)
            .put("abi", Build.SUPPORTED_ABIS.firstOrNull().orEmpty())
            .put("timezone", ZoneId.systemDefault().id)
            .put("manufacturer", Build.MANUFACTURER)
            .put("model", Build.MODEL)
            .put("device", Build.DEVICE)
            .put("build_fingerprint", Build.FINGERPRINT)
            .put("version_name", packageInfo.versionName.orEmpty())
            .put("version_code", packageInfo.longVersionCode)
            .put("runtime_epoch", "00000000-0000-4000-8000-000000000000")
            .put("host_generation", 1)
        lastEnvironment.set(environment.toString())
        if (File(canonicalBase, "runtime-reset-intent.json").exists()) {
            // A recorded S-UPD-006 reset only moves forward, and before any activation (S-UI-017).
            val recovered = runCatching {
                NativeRuntime.nativeResetRuntimeData(canonicalBase.absolutePath)?.let(::JSONObject)
            }.getOrNull()
            if (recovered?.optBoolean("reset", false) != true) {
                runtimeSession.compareAndSet(
                    observedSession,
                    inactiveSession(
                        observedSession.host,
                        recovered?.optString("code").orEmpty().ifEmpty { DaemonErrorToken.IoError.wire },
                    ),
                )
                return false
            }
        }
        var result = runCatching {
            JSONObject(NativeRuntime.nativeStart(canonicalBase.absolutePath, environment.toString()))
        }.getOrNull() ?: return false
        if (!result.optBoolean("ready", false)) {
            val owner = runCatching { observeOwner() }.getOrNull()
            val transition = runCatching {
                JSONObject(
                    NativeRuntime.nativeObserveHostTransition(canonicalBase.absolutePath),
                )
            }.getOrNull()
            if (
                owner != null &&
                transition != null &&
                shouldRecoverUncommittedApkTransition(
                    result.optString("code"),
                    owner.host,
                    transition.optString("state"),
                ) &&
                NativeRuntime.nativeAbortRemoteHostTransition(
                    canonicalBase.absolutePath,
                    transition.getJSONObject("intent").toString(),
                )
            ) {
                result = runCatching {
                    JSONObject(
                        NativeRuntime.nativeStart(
                            canonicalBase.absolutePath,
                            environment.toString(),
                        ),
                    )
                }.getOrNull() ?: return false
            } else if (
                owner != null &&
                transition != null &&
                shouldResumeCommittedApkTransition(
                    result.optString("code"),
                    owner.host,
                    transition.optString("state"),
                )
            ) {
                result = runCatching {
                    JSONObject(
                        NativeRuntime.nativeRecoverDeadMagiskHost(
                            canonicalBase.absolutePath,
                            environment.toString(),
                        ),
                    )
                }.getOrNull() ?: return false
            }
        }
        if (!result.optBoolean("ready", false)) {
            val code = result.optString("code", "RUNTIME_UNAVAILABLE")
            val owner = runCatching { observeOwner() }.getOrNull()
            runtimeSession.compareAndSet(
                observedSession,
                inactiveSession(owner?.host ?: observedSession.host, code),
            )
            if (owner?.host == DaemonHostToken.MagiskBackend && !runtimeSession.get().started) {
                scheduleDeadHostRecovery()
            }
            startCompanion()
            return false
        }
        val generation = result.getLong("host_generation")
        val activated = activeSession(
            DaemonHostToken.ApkRuntime,
            result.getString("runtime_epoch"),
            generation,
            result.getString("runtime_instance_id"),
        )
        if (!runtimeSession.compareAndSet(observedSession, activated)) {
            NativeRuntime.nativeRecordHostFault(
                DaemonErrorToken.StaleAuthority.wire,
                "host_start_projection",
            )
            startCompanion()
            return runtimeSession.get().started
        }
        startCompanion()
        replayCompanionFacts()
        registerPlatformFacts()
        publishFrameworkPrimitives(generation)
        recoverMaintenance()
        val guard = File(application.applicationInfo.nativeLibraryDir, "libdroidbridge_exec_guard.so")
        if (!NativeRuntime.nativeProbeAppGuard(guard.absolutePath)) {
            NativeRuntime.nativeRecordHostFault("CLEANUP_UNVERIFIED", "app_guard_probe")
        }
        guardScopeSink.get()?.invoke()
        return true
    }

    /**
     * Replays App facts recorded while another host was authoritative into the App Runtime
     * that has just become host, so those sources are not stranded as not ready until they
     * next change.
     */
    private fun replayCompanionFacts() {
        companionCapabilityFacts.apkHostReplay().forEach { fact ->
            NativeRuntime.nativeRegisterCapability(
                fact.key,
                fact.state,
                fact.reason.orEmpty(),
                fact.sourceGeneration,
                fact.hasExecutor,
            )
        }
    }

    fun registerPlatformFacts() {
        val generation = platformGeneration.incrementAndGet()
        val localNetwork = if (Build.VERSION.SDK_INT <= 36) {
            "available"
        } else if (application.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED) {
            "available"
        } else {
            "unavailable"
        }
        register("android.local_network", localNetwork, "PLATFORM_PERMISSION", generation, publish = false)
        val notifications = if (
            application.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        ) "available" else "unavailable"
        register("android.notifications", notifications, "PLATFORM_PERMISSION", generation, publish = false)
        val alarmManager = application.getSystemService(AlarmManager::class.java)
        val exactAlarm = if (alarmManager.canScheduleExactAlarms()) "available" else "unavailable"
        register("automation.exact_alarm", exactAlarm, "PLATFORM_SPECIAL_ACCESS", generation, publish = false)
    }

    fun register(
        key: String,
        state: String,
        reason: String,
        generation: Long,
        hasExecutor: Boolean = false,
        publish: Boolean = true,
    ): Boolean {
        val recorded = companionCapabilityFacts.register(
            key,
            state,
            if (state == "available") "" else reason,
            generation,
            hasExecutor,
        )
        if (!recorded) return false
        companionCapabilityRevision.incrementAndGet()
        val session = runtimeSession.get()
        if (!session.started || session.host != DaemonHostToken.ApkRuntime) {
            scheduleCompanionCapabilityRefresh()
            if (publish) hintSink.get()?.invoke("context.status")
            return true
        }
        val accepted = NativeRuntime.nativeRegisterCapability(
            key,
            state,
            if (state == "available") "" else reason,
            generation,
            hasExecutor,
        )
        if (accepted && publish) hintSink.get()?.invoke("context.status")
        if (accepted) {
            promotionState.observeIdleHint()
            promotionConnection.get()?.let(::schedulePromotion)
        }
        return accepted
    }

    fun submit(envelope: ByteArray): ByteArray {
        if (!start()) throw RuntimeStartException(runtimeSession.get().startFailure)
        val session = runtimeSession.get()
        val fence = session.activeFence ?: throw RuntimeStartException(session.startFailure)
        frameworkReadySink.get()?.invoke(fence.hostGeneration)
        registerPlatformFacts()
        if (session.host == DaemonHostToken.ApkRuntime) return NativeRuntime.nativeSubmit(envelope)
        val connection = companion.connection()
            ?: throw RuntimeStartException(DaemonErrorToken.CapabilityUnavailable.wire)
        val owner = observeOwner()
        val response = forward(connection, envelope, owner)
        response.use { return it.envelope.payload.toString().encodeToByteArray() }
    }

    /**
     * One RuntimeForward, re-sent once when the transport fails it. The Runtime records a request
     * before it serves it, so an identical re-send asks for that same request's outcome instead of
     * running it a second time, and it is offered only while the host that admitted the first
     * attempt is still the owner, so it can never reach another host.
     */
    private fun forward(
        connection: DaemonConnection,
        envelope: ByteArray,
        owner: DaemonOwnerFence,
    ): DaemonReceivedMessage {
        try {
            return forwardOnce(connection, envelope, owner)
        } catch (first: Exception) {
            if (first is CancellationException) throw first
            val retry = if (runCatching { observeOwner() == owner }.getOrDefault(false)) {
                companion.connection()
            } else {
                null
            }
            return if (retry == null) throw first else forwardOnce(retry, envelope, owner)
        }
    }

    private fun forwardOnce(
        connection: DaemonConnection,
        envelope: ByteArray,
        owner: DaemonOwnerFence,
    ): DaemonReceivedMessage = connection.request(
        DaemonOperationToken.RuntimeForward,
        json.parseToJsonElement(envelope.decodeToString()),
        owner,
        300_000,
    )

    /**
     * Answers one S-MCP-006 internal artifact query through the same host path as [submit]: the
     * APK Runtime answers locally while it is authoritative, otherwise the live Magisk Core answers
     * one RuntimeForward envelope carrying at most one `mcp_artifact` descriptor.
     */
    fun queryArtifacts(query: ByteArray): McpArtifactQueryReply {
        if (!start()) throw RuntimeStartException(runtimeSession.get().startFailure)
        val session = runtimeSession.get()
        session.activeFence ?: throw RuntimeStartException(session.startFailure)
        if (session.host == DaemonHostToken.ApkRuntime) {
            val slot = intArrayOf(-1)
            val payload = NativeRuntime.nativeQueryArtifacts(query, slot)
            val descriptor = slot[0].takeIf { it >= 0 }?.let(ParcelFileDescriptor::adoptFd)
            if (payload == null) {
                descriptor?.close()
                throw RuntimeStartException(DaemonErrorToken.InternalError.wire)
            }
            return McpArtifactQueryReply(payload, descriptor)
        }
        val connection = companion.connection()
            ?: throw RuntimeStartException(DaemonErrorToken.CapabilityUnavailable.wire)
        val owner = observeOwner()
        connection.request(
            DaemonOperationToken.RuntimeForward,
            json.parseToJsonElement(query.decodeToString()),
            owner,
            ARTIFACT_QUERY_TIMEOUT_MILLIS,
        ).use { response ->
            val roles = response.envelope.fdRoles
            check(roles.isEmpty() || roles == listOf(MCP_ARTIFACT_ROLE))
            // The received message owns its descriptor, so the reply holds its own duplicate.
            val descriptor = response.descriptors.singleOrNull()?.dup()
            return McpArtifactQueryReply(
                response.envelope.payload.toString().encodeToByteArray(),
                descriptor,
            )
        }
    }

    /** S-UI-017 `getMaintenanceState`: canonical-file and guard-proof facts only; no Core starts. */
    fun maintenanceState(): String {
        val result = runCatching {
            NativeRuntime.nativeMaintenanceState(canonicalBase.absolutePath)?.let(::JSONObject)
        }.getOrNull() ?: return maintenanceFailure(DaemonErrorToken.InternalError.wire)
        if (result.has("code")) return maintenanceFailure(result.getString("code"))
        return result.toString()
    }

    /** The module fact from the authenticated companion; no connection means no stable module is running. */
    private val moduleObservation = AtomicReference(ModuleObservation.Absent)

    private val maintenanceHost = object : MaintenanceHost {
        override fun ensureApkHost(): String? {
            val current = runtimeSession.get()
            if (current.started && current.host == DaemonHostToken.MagiskBackend) {
                val connection = companion.connection() ?: return DaemonErrorToken.CapabilityUnavailable.wire
                demoteToApk(connection, recoverOnly = false, sourceInstance = current.activeFence?.runtimeInstanceId)
            } else if (!current.started) {
                start()
            }
            val session = runtimeSession.get()
            return if (session.started && session.host == DaemonHostToken.ApkRuntime) {
                null
            } else {
                session.startFailure.ifEmpty { DaemonErrorToken.HostTransitionPending.wire }
            }
        }

        override fun closeAdmission(): String? {
            val result = runCatching { NativeRuntime.nativeCloseAdmissionForMaintenance()?.let(::JSONObject) }.getOrNull()
            return if (result?.optBoolean("closed", false) == true) {
                null
            } else {
                result?.optString("code").orEmpty().ifEmpty { DaemonErrorToken.InternalError.wire }
            }
        }

        override fun reopenAdmission(): Boolean = NativeRuntime.nativeReopenAdmission(canonicalBase.absolutePath)

        override fun moduleObservation(): ModuleObservation = moduleObservation.get()

        override fun cleanupVerified(): Boolean =
            runCatching { JSONObject(maintenanceState()).optString("cleanup") == "verified" }.getOrDefault(false)

        override fun privilegedInstallAvailable(): Boolean {
            val session = runtimeSession.get()
            return session.started && session.host == DaemonHostToken.ApkRuntime &&
                moduleObservation.get() == ModuleObservation.Compatible &&
                companion.connection()?.isHealthy() == true
        }

        override fun privilegedInstall(
            kind: PrivilegedArtifact,
            record: UpdateMaintenanceRecord,
            artifact: File,
        ): PrivilegedOutcome {
            // Nothing was dispatched without a connection, so no attempt can be running.
            val connection = companion.connection() ?: return PrivilegedOutcome(null, cleanupVerified = true)
            val executionId = requireNotNull(record.maintenanceExecutionId)
            val (operation, role, payload) = when (kind) {
                PrivilegedArtifact.Apk -> Triple(
                    DaemonOperationToken.MaintenanceInstallApk,
                    "verified_apk",
                    buildJsonObject {
                        put("update_id", record.updateId)
                        put("execution_id", executionId)
                        put("package", application.packageName)
                        put("version_code", record.targetVersionCode)
                        put("sha256", requireNotNull(record.targetApkSha256))
                        put("signer_sha256", record.targetApkSignerSha256)
                        put("size", requireNotNull(record.targetApkSize))
                    },
                )
                PrivilegedArtifact.Module -> Triple(
                    DaemonOperationToken.MaintenanceInstallModule,
                    "verified_module_zip",
                    buildJsonObject {
                        put("update_id", record.updateId)
                        put("execution_id", executionId)
                        put("module_id", if (application.packageName.endsWith(".debug")) "droidbridge_debug" else "droidbridge")
                        put("version_code", record.targetVersionCode)
                        put("sha256", requireNotNull(record.targetModuleSha256))
                        put("size", requireNotNull(record.targetModuleSize))
                    },
                )
            }
            val reply = ParcelFileDescriptor.open(artifact, ParcelFileDescriptor.MODE_READ_ONLY).use { descriptor ->
                runCatching {
                    connection.request(operation, payload, observeOwner(), PRIVILEGED_INSTALL_TIMEOUT_MILLIS, listOf(role), listOf(descriptor))
                        .use { it.envelope.payload.jsonObject }
                }.getOrNull()
            }
            val exitCode = reply?.takeIf { it["error"] == null && it["cleanup"]?.jsonPrimitive?.contentOrNull == "clean" }
                ?.get("process_exit_code")?.jsonPrimitive?.contentOrNull?.toIntOrNull()
            if (exitCode != null) return PrivilegedOutcome(exitCode, cleanupVerified = true)
            // A refusal or lost reply is settled only by the daemon's own attempt facts.
            return PrivilegedOutcome(null, cleanupVerified = privilegedCleanup(record.updateId) in setOf("clean", "none"))
        }

        override fun privilegedCleanup(updateId: String): String? {
            val connection = companion.connection() ?: return null
            return runCatching {
                connection.request(
                    DaemonOperationToken.MaintenanceStatus,
                    buildJsonObject { put("update_id", updateId) },
                    observeOwner(),
                    5_000,
                ).use { it.envelope.payload.jsonObject }
            }.getOrNull()?.takeIf { it["update_id"]?.jsonPrimitive?.contentOrNull == updateId }
                ?.get("cleanup")?.jsonPrimitive?.contentOrNull
        }
    }

    private val maintenance: UpdateMaintenanceController by lazy {
        UpdateMaintenanceController(
            packageName = application.packageName,
            config = ReleaseConfig.from(
                BuildConfig.GITHUB_OWNER,
                BuildConfig.GITHUB_REPO,
                BuildConfig.RELEASE_MANIFEST_URL,
                BuildConfig.APK_SIGNER_SHA256,
                BuildConfig.RELEASE_KEY_ID,
                BuildConfig.RELEASE_PUBLIC_KEY_BASE64,
            ),
            store = UpdateMaintenanceStore.android(canonicalBase),
            host = maintenanceHost,
            packages = AndroidPackageFacts(application),
            installer = AndroidApkSessionInstaller(application),
            cacheRoot = File(application.cacheDir, "updates"),
        )
    }

    /** Reconciles against current package/session observations before reporting, e.g. after a dismissed installer. */
    fun updateMaintenanceState(): String = onTransitionExecutor {
        runCatching { maintenance.recover() }
            .onFailure { NativeRuntime.nativeRecordHostFault(DaemonErrorToken.IoError.wire, "update_maintenance_recover") }
        maintenance.state()
    }

    fun beginProductUpdate(manifest: ByteArray, signature: ByteArray): String =
        onTransitionExecutor { maintenance.beginProductUpdate(manifest, signature) }

    fun beginModuleRepair(manifest: ByteArray, signature: ByteArray): String =
        onTransitionExecutor { maintenance.beginModuleRepair(manifest, signature) }

    fun installUpdateApk(updateId: String): String = onTransitionExecutor { maintenance.installApk(updateId) }

    fun installUpdateModule(updateId: String): String = onTransitionExecutor { maintenance.installModule(updateId) }

    fun cancelUpdate(updateId: String): String = onTransitionExecutor { maintenance.cancel(updateId) }

    fun continueWithoutModule(updateId: String): String =
        onTransitionExecutor { maintenance.continueWithoutModule(updateId) }

    private fun observeModule(observed: ModuleObservation) {
        if (moduleObservation.getAndSet(observed) != observed) recoverMaintenance()
    }

    /** Observation-based maintenance reconciliation; a failure is recorded, never retried silently. */
    private fun recoverMaintenance() {
        executor().execute {
            runCatching { maintenance.recover() }
                .onFailure { NativeRuntime.nativeRecordHostFault(DaemonErrorToken.IoError.wire, "update_maintenance_recover") }
        }
    }

    /**
     * S-UI-017 `getDiagnosticsSnapshot`: the Runtime session plus a full status read bounded to
     * [DIAGNOSTICS_DEADLINE_MILLIS]. A session that is not started is reported, never started.
     */
    fun diagnosticsSnapshot(): String {
        val session = runtimeSession.get()
        val status = if (session.started) {
            val envelope = buildJsonObject {
                put("protocol_version", 1)
                put("request_id", java.util.UUID.randomUUID().toString())
                put("payload", buildJsonObject {
                    put("tool", "context")
                    put("action", "status")
                    put("input", buildJsonObject { put("detail", "full") })
                })
            }.toString().encodeToByteArray()
            val read = diagnosticsReads.submit<ByteArray> { submit(envelope) }
            runCatching { read.get(DIAGNOSTICS_DEADLINE_MILLIS, java.util.concurrent.TimeUnit.MILLISECONDS) }
                .onFailure { read.cancel(true) }
                .getOrNull()
                ?.let { response -> runCatching { json.parseToJsonElement(response.decodeToString()).jsonObject }.getOrNull() }
                ?.takeIf { response -> response["outcome"]?.jsonPrimitive?.contentOrNull == "success" }
                ?.get("result")
        } else {
            null
        }
        return buildJsonObject {
            put("schema_version", 1)
            put("session", buildJsonObject {
                put("started", session.started)
                session.host?.let { put("host", it.wire) }
                if (!session.started) put("start_failure", session.startFailure)
            })
            status?.let { put("status", it) }
        }.toString()
    }

    /**
     * S-UI-017 `resetRuntimeData`, serialized with host transitions. A live Magisk source is first
     * retired through the ordinary demotion; the fresh APK instance is active before success returns.
     */
    /** Executions a lost instance left running, which stop the Magisk daemon from recovering the store. */
    fun strandedExecutions(): Int = NativeRuntime.nativeStrandedExecutions(canonicalBase.absolutePath)

    /**
     * Settles those executions as interrupted so the host can recover again. The native side refuses
     * while any live Runtime owns the store, so a working host is never touched.
     */
    fun clearStrandedExecutions(): String = onTransitionExecutor {
        val reply = NativeRuntime.nativeClearStrandedExecutions(canonicalBase.absolutePath)
            ?: return@onTransitionExecutor maintenanceFailure(DaemonErrorToken.InternalError.wire)
        // The supervisor restarts the daemon on its own; the next start finds nothing to refuse.
        reply
    }

    fun resetRuntimeData(): String = onTransitionExecutor {
        val current = runtimeSession.get()
        if (current.started && current.host == DaemonHostToken.MagiskBackend) {
            val connection = companion.connection()
                ?: return@onTransitionExecutor maintenanceFailure(DaemonErrorToken.CapabilityUnavailable.wire)
            demoteToApk(connection, recoverOnly = false, sourceInstance = current.activeFence?.runtimeInstanceId)
            val demoted = runtimeSession.get()
            if (!demoted.started || demoted.host != DaemonHostToken.ApkRuntime) {
                return@onTransitionExecutor maintenanceFailure(DaemonErrorToken.HostTransitionPending.wire)
            }
        }
        val source = runtimeSession.get()
        val withdrawn = inactiveSession(DaemonHostToken.ApkRuntime, DaemonErrorToken.HostTransitionPending.wire)
        if (source.started && !runtimeSession.compareAndSet(source, withdrawn)) {
            return@onTransitionExecutor maintenanceFailure(DaemonErrorToken.HostTransitionPending.wire)
        }
        val result = runCatching {
            NativeRuntime.nativeResetRuntimeData(canonicalBase.absolutePath)?.let(::JSONObject)
        }.getOrNull()
        val reset = result?.optBoolean("reset", false) == true
        val code = result?.optString("code").orEmpty().ifEmpty { DaemonErrorToken.InternalError.wire }
        if (source.started) {
            if (reset || result?.optBoolean("released", false) == true) {
                releaseApkProjection()
                if (!reset) runtimeSession.compareAndSet(withdrawn, inactiveSession(DaemonHostToken.ApkRuntime, code))
            } else {
                // The refused reset reopened the unchanged instance.
                runtimeSession.compareAndSet(withdrawn, source)
            }
        }
        if (!reset) return@onTransitionExecutor maintenanceFailure(code)
        activateAfterMaintenance()
    }

    /** S-UI-017 `resetRuntimeHostToApk`: only a non-started session with a corrupt owner qualifies. */
    fun resetRuntimeHostToApk(): String = onTransitionExecutor {
        if (runtimeSession.get().started) {
            return@onTransitionExecutor maintenanceFailure(DaemonErrorToken.StaleAuthority.wire)
        }
        val result = runCatching {
            NativeRuntime.nativeResetRuntimeHostToApk(canonicalBase.absolutePath)?.let(::JSONObject)
        }.getOrNull()
        if (result?.optBoolean("reset", false) != true) {
            return@onTransitionExecutor maintenanceFailure(
                result?.optString("code").orEmpty().ifEmpty { DaemonErrorToken.InternalError.wire },
            )
        }
        activateAfterMaintenance()
    }

    private fun activateAfterMaintenance(): String {
        runtimeSession.updateAndGet { current ->
            if (current.started) current else inactiveSession(DaemonHostToken.ApkRuntime, "RUNTIME_UNAVAILABLE")
        }
        return if (start()) MAINTENANCE_RESET else maintenanceFailure(runtimeSession.get().startFailure)
    }

    private fun releaseApkProjection() {
        runCatching { apkProjectionReleasedSink.get()?.invoke() }
            .onFailure {
                // A retained alarm only reaches a non-APK session and delivers nothing there.
                NativeRuntime.nativeRecordHostFault(DaemonErrorToken.IoError.wire, "automation_alarm_release")
            }
    }

    private fun onTransitionExecutor(action: () -> String): String =
        runCatching { executor().submit<String> { action() }.get() }
            .getOrElse { maintenanceFailure(DaemonErrorToken.InternalError.wire) }

    private fun maintenanceFailure(code: String): String =
        JSONObject().put("schema_version", 1).put("error", code).toString()

    fun validatesFence(
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): Boolean = runtimeSession.get().validates(runtimeEpoch, hostGeneration, runtimeInstanceId)

    /**
     * Delivers one exact-alarm or reconcile broadcast to the APK Runtime's Automation scheduler,
     * which rescans canonical due truth and re-arms its single alarm (S-LIFE-003/004). A Magisk
     * Runtime owns its own timer projection, so the broadcast has nothing to deliver there.
     */
    fun wakeAutomation(): Boolean {
        registerPlatformFacts()
        if (!start()) return false
        if (runtimeSession.get().host != DaemonHostToken.ApkRuntime) return true
        return NativeRuntime.nativeAutomationWake()
    }

    fun publishNetworkDefault(observed: NetworkDefaultObservation): Boolean {
        val session = runtimeSession.get()
        if (
            !session.validates(
                observed.runtimeEpoch,
                observed.hostGeneration,
                observed.runtimeInstanceId,
            )
        ) return true
        if (session.host == DaemonHostToken.ApkRuntime) {
            return NativeRuntime.nativeNetworkDefaultChanged(
                observed.runtimeEpoch,
                observed.hostGeneration,
                observed.runtimeInstanceId,
                observed.subscriptionGeneration,
                observed.sourceGeneration,
                observed.networkId.orEmpty(),
                observed.transport.orEmpty(),
            )
        }
        if (session.host != DaemonHostToken.MagiskBackend) return false
        val connection = companion.connection() ?: return false
        val owner = DaemonOwnerFence(
            observed.runtimeEpoch,
            DaemonHostToken.MagiskBackend,
            observed.hostGeneration,
            observed.runtimeInstanceId,
        )
        return runCatching {
            connection.request(
                DaemonOperationToken.NetworkDefaultChanged,
                buildJsonObject {
                    put("subscription_generation", observed.subscriptionGeneration)
                    put("source_generation", observed.sourceGeneration)
                    observed.networkId?.let { put("network_id", it) }
                    observed.transport?.let { put("transport", it) }
                },
                owner,
                10_000,
            ).use { response ->
                val payload = response.envelope.payload.jsonObject
                payload.keys == setOf("accepted") &&
                    payload["accepted"]?.jsonPrimitive?.booleanOrNull == true
            }
        }.getOrDefault(false)
    }

    /**
     * Asserts the network this process runs under to the Magisk host, so the host's own process
     * follows it. The event plane cannot carry this fact: S-NET-006 subscribes
     * `network.default_changed` only while an enabled Automation requires it, while the host must
     * follow the device's network whenever it is the host. The caller is never blocked — the
     * status that establishes the host arrives on the companion's read loop, which must stay
     * reachable for the reply this send waits on.
     */
    fun scheduleNetworkAttachment() {
        if (runtimeSession.get().host != DaemonHostToken.MagiskBackend) {
            return
        }
        val revision = networkAttachmentRevision.incrementAndGet()
        if (!networkAttachmentScheduled.compareAndSet(false, true)) return
        executor().execute {
            var observed = revision
            try {
                do {
                    observed = networkAttachmentRevision.get()
                    publishNetworkAttachment()
                } while (networkAttachmentRevision.get() != observed)
            } finally {
                networkAttachmentScheduled.set(false)
                if (networkAttachmentRevision.get() != observed) scheduleNetworkAttachment()
            }
        }
    }

    /**
     * Sends one attachment fact: the network this process is bound to, or its absence. A fact
     * that cannot be delivered is recorded as a host fault rather than dropped, because the host
     * would otherwise keep the previous binding with no other signal that it is stale.
     */
    private fun publishNetworkAttachment() {
        val observed = runCatching { observeOwner() }.getOrNull()
        if (observed == null) {
            NativeRuntime.nativeRecordHostFault(DaemonErrorToken.IoError.wire, "network_attachment")
            return
        }
        // A store that no longer names this App's live instance is a host on its way out: it
        // releases its own binding, and the next establishment asserts a fresh one.
        if (observed.host != DaemonHostToken.MagiskBackend || observed.runtimeInstanceId == null) {
            return
        }
        val handle = networkAttachmentSource.get()?.invoke()
        val connection = companion.connection()
        if (connection == null) {
            return
        }
        val delivered = runCatching {
            connection.request(
                DaemonOperationToken.NetworkAttachment,
                buildJsonObject {
                    handle?.let { put("network_id", it) }
                },
                observed,
                10_000,
            ).use { response ->
                val payload = response.envelope.payload.jsonObject
                payload.keys == setOf("accepted") &&
                    payload["accepted"]?.jsonPrimitive?.booleanOrNull == true
            }
        }.getOrDefault(false)
        if (!delivered) {
            NativeRuntime.nativeRecordHostFault(DaemonErrorToken.IoError.wire, "network_attachment")
        }
    }

    fun setHintSink(sink: ((String) -> Unit)?) {
        hintSink.set(sink)
    }

    fun setFrameworkReadySink(sink: ((Long) -> Unit)?) {
        frameworkReadySink.set(sink)
    }

    fun setCompanionDisconnectedSink(sink: (() -> Boolean)?) {
        companionDisconnectedSink.set(sink)
    }

    /**
     * Supplies the network handle this process is bound to. It is read when the fact is sent, so
     * a change that lands while a send is in flight is carried by the next one.
     */
    fun setNetworkAttachmentSource(source: (() -> String?)?) {
        networkAttachmentSource.set(source)
    }

    /**
     * Receives each replacement of the execution-guard scope, so identity guards whose
     * proofs belong to that scope are proven against the scope that is now live.
     */
    fun setGuardScopeSink(sink: (() -> Unit)?) {
        guardScopeSink.set(sink)
    }

    /**
     * Receives the release of the APK Runtime instance, so its host-owned wake projection (the
     * single exact alarm) is cancelled before the new exclusive owner builds its own (S-LIFE-003).
     */
    fun setApkProjectionReleasedSink(sink: (() -> Unit)?) {
        apkProjectionReleasedSink.set(sink)
    }

    private fun startCompanion() {
        if (!serverStarted.compareAndSet(false, true)) return
        runCatching { companion.start() }
            .onFailure {
                serverStarted.set(false)
                NativeRuntime.nativeRecordHostFault("IPC_UNAVAILABLE", "magisk_companion")
            }
    }

    private fun scheduleCompanionCapabilityRefresh() {
        if (runtimeSession.get().host != DaemonHostToken.MagiskBackend) return
        if (!companionCapabilityRefreshScheduled.compareAndSet(false, true)) return
        executor().execute {
            var observedRevision = companionCapabilityRevision.get()
            try {
                do {
                    observedRevision = companionCapabilityRevision.get()
                    val connection = companion.connection() ?: return@execute
                    val owner = observeOwner()
                    connection.request(
                        DaemonOperationToken.HostStatus,
                        buildJsonObject {},
                        owner,
                        5_000,
                    ).use { response ->
                        check(response.envelope.payload is JsonObject)
                    }
                } while (companionCapabilityRevision.get() != observedRevision)
            } catch (_: Exception) {
                // A probe that did not arrive is not a capability change: the revision check below
                // owns asking again, and the disconnect callback owns a connection that was lost.
            } finally {
                companionCapabilityRefreshScheduled.set(false)
                if (companionCapabilityRevision.get() != observedRevision) {
                    scheduleCompanionCapabilityRefresh()
                }
            }
        }
    }

    private fun observeOwner(): DaemonOwnerFence {
        val value = JSONObject(NativeRuntime.nativeObserveOwner(canonicalBase.absolutePath))
        check(!value.has("code"))
        val ownerHost = DaemonProtocol.decodeHost(value.getString("host"))
        val generation = value.getLong("host_generation")
        val session = runtimeSession.get()
        val active = session.activeFence?.takeIf { fence ->
            fence.runtimeEpoch == value.getString("runtime_epoch") &&
                fence.hostGeneration == generation &&
                session.host == ownerHost
        }
        return DaemonOwnerFence(
            runtimeEpoch = value.getString("runtime_epoch"),
            host = ownerHost,
            hostGeneration = generation,
            runtimeInstanceId = active?.runtimeInstanceId,
        )
    }

    private fun capabilitySnapshot(payload: JsonElement): DaemonReplyPayload {
        require(payload is JsonObject && payload.isEmpty())
        val response = buildJsonObject {
            put("registrations", buildJsonArray {
                companionCapabilityFacts.snapshot().forEach { fact ->
                    add(buildJsonObject {
                        put("key", fact.key)
                        put("state", fact.state)
                        fact.reason?.let { put("reason", it) }
                        put("source_generation", fact.sourceGeneration)
                        put("has_executor", fact.hasExecutor)
                    })
                }
            })
        }
        return DaemonReplyPayload(response)
    }

    /**
     * Runs one companion execution. The already-admitted primitive and the live host
     * generation select the executor; an unregistered pair is a typed unavailability.
     */
    private fun companionExecutionResponse(
        request: DaemonWireEnvelope,
        descriptors: List<ParcelFileDescriptor>,
    ): DaemonReplyPayload {
        val execution = decodeCompanionExecution(request.payload)
            ?: return DaemonReplyPayload(
                companionFailurePayload(DaemonErrorToken.InvalidArgument.wire),
            )
        val runtimeInstanceId = requireNotNull(request.runtimeInstanceId)
        val result = runCatching {
            NativeAndroidExecutionDispatcher.executePrimitive(
                primitive = execution.primitive,
                generation = request.hostGeneration,
                payload = execution.payload,
                executionId = execution.executionId,
                runtimeEpoch = request.runtimeEpoch,
                hostGeneration = request.hostGeneration,
                runtimeInstanceId = runtimeInstanceId,
                descriptors = request.fdRoles
                    .zip(descriptors)
                    .map { (role, descriptor) -> RoleDescriptor(role, descriptor) },
            )
        }.getOrElse {
            // An execution that fails this way owes its own typed answer: the companion connection
            // carries the requests of every other caller as well.
            return DaemonReplyPayload(companionFailurePayload(DaemonErrorToken.InternalError.wire))
        }
        val errorCode = result.errorCode
        if (errorCode != null) return DaemonReplyPayload(companionFailurePayload(errorCode))
        val payload = companionResultPayload(result.payload)
            ?: return refusedResult(result.descriptors, DaemonErrorToken.InternalError)
        val roles = companionResultRoles(result.descriptors.map(RoleDescriptor::role))
            ?: return refusedResult(result.descriptors, DaemonErrorToken.ProtocolIncompatible)
        return DaemonReplyPayload(payload, roles, result.descriptors.map(RoleDescriptor::descriptor))
    }

    /**
     * Delivers one cancellation to the identity that owns the execution. The
     * already-admitted primitive selects that identity's own cancel, so the APK surface
     * cancels only the runs it started and no identity ever cancels another's process.
     */
    private fun companionCancellationResponse(
        request: DaemonWireEnvelope,
    ): DaemonReplyPayload {
        val cancellation = decodeCompanionCancellation(request.payload)
            ?: return DaemonReplyPayload(
                companionFailurePayload(DaemonErrorToken.InvalidArgument.wire),
            )
        val runtimeInstanceId = requireNotNull(request.runtimeInstanceId)
        val result = runCatching {
            NativeAndroidExecutionDispatcher.executePrimitive(
                primitive = cancellation.primitive,
                generation = request.hostGeneration,
                payload = json.encodeToString(
                    JsonObject.serializer(),
                    buildJsonObject { put("execution_id", cancellation.executionId) },
                ).encodeToByteArray(),
                executionId = cancellation.executionId,
                runtimeEpoch = request.runtimeEpoch,
                hostGeneration = request.hostGeneration,
                runtimeInstanceId = runtimeInstanceId,
            )
        }.getOrElse {
            // A cancellation that fails this way owes its own typed answer, not the connection.
            return DaemonReplyPayload(companionFailurePayload(DaemonErrorToken.InternalError.wire))
        }
        val errorCode = result.errorCode
        if (errorCode != null) return DaemonReplyPayload(companionFailurePayload(errorCode))
        val payload = companionResultPayload(result.payload)
            ?: return refusedResult(result.descriptors, DaemonErrorToken.InternalError)
        val roles = companionResultRoles(result.descriptors.map(RoleDescriptor::role))
            ?: return refusedResult(result.descriptors, DaemonErrorToken.ProtocolIncompatible)
        return DaemonReplyPayload(payload, roles, result.descriptors.map(RoleDescriptor::descriptor))
    }

    private fun refusedResult(
        roles: List<RoleDescriptor>,
        code: DaemonErrorToken,
    ): DaemonReplyPayload {
        roles.forEach { role -> runCatching { role.descriptor.close() } }
        return DaemonReplyPayload(companionFailurePayload(code.wire))
    }

    private fun handleHostStatus(connection: DaemonConnection, payload: JsonObject) {
        when (payload.getValue("role").jsonPrimitive.content) {
            "backend_only" -> {
                val ready = payload.getValue("ready").jsonPrimitive.boolean
                observeModule(if (ready) ModuleObservation.Compatible else ModuleObservation.Mismatched)
                if (!ready) return
                promotionConnection.set(connection)
                promotionState.observeBackendReady()
                schedulePromotion(connection)
            }
            "runtime_host" -> {
                observeModule(ModuleObservation.Compatible)
                promotionConnection.set(null)
                promotionState.clearBackend()
                val owner = observeOwner()
                if (owner.host != DaemonHostToken.MagiskBackend) return
                val ready = payload.getValue("ready").jsonPrimitive.boolean
                val cleanupReady = payload.getValue("transition_cleanup_ready").jsonPrimitive.boolean
                val instance = payload["runtime_instance_id"]?.jsonPrimitive?.contentOrNull
                if (!cleanupReady) return
                val transition = JSONObject(
                    NativeRuntime.nativeObserveHostTransition(canonicalBase.absolutePath),
                )
                if (transition.has("code")) {
                    recordSessionFailure(transition.getString("code"))
                    NativeRuntime.nativeRecordHostFault(
                        transition.getString("code"),
                        "host_transition_observe",
                    )
                    return
                }
                val transitionState = transition.getString("state")
                val action = decideMagiskHostStatus(
                    ready = ready,
                    cleanupReady = cleanupReady,
                    requiresApkHost = requiresApkHost(),
                    transitionPresent = transitionState != "none",
                    pendingSourceTransition = transitionState == "source_pending",
                )
                when (action) {
                    MagiskHostStatusAction.Wait -> return
                    MagiskHostStatusAction.BeginDemotion -> {
                        if (instance == null) {
                            recordSessionFailure(DaemonErrorToken.StaleAuthority.wire)
                            NativeRuntime.nativeRecordHostFault(
                                DaemonErrorToken.StaleAuthority.wire,
                                "host_transition_source_missing",
                            )
                            return
                        }
                        scheduleDemotion(connection, recoverOnly = false, sourceInstance = instance)
                        return
                    }
                    MagiskHostStatusAction.RecoverDemotion -> {
                        scheduleDemotion(connection, recoverOnly = true, sourceInstance = instance)
                        return
                    }
                    MagiskHostStatusAction.EstablishCurrentHost -> Unit
                }
                if (instance == null) {
                    recordSessionFailure(DaemonErrorToken.StaleAuthority.wire)
                    NativeRuntime.nativeRecordHostFault(
                        DaemonErrorToken.StaleAuthority.wire,
                        "host_transition_target_missing",
                    )
                    return
                }
                if (!establishMagiskHost(owner, instance, transitionState != "none")) {
                    recordSessionFailure(DaemonErrorToken.StaleAuthority.wire)
                    NativeRuntime.nativeRecordHostFault(
                        DaemonErrorToken.StaleAuthority.wire,
                        "host_transition_finish",
                    )
                    return
                }
                if (requiresApkHost()) {
                    scheduleDemotion(connection, recoverOnly = false, sourceInstance = instance)
                }
            }
        }
    }

    private fun requiresApkHost(): Boolean =
        File(canonicalBase, "update-maintenance.json").exists() ||
            File(canonicalBase, "module-exclusion.json").exists()

    private fun schedulePromotion(connection: DaemonConnection) {
        val session = runtimeSession.get()
        if (session.host != DaemonHostToken.ApkRuntime || !session.started) return
        if (promotionConnection.get() !== connection || !connection.isHealthy()) return
        if (!promotionState.tryBegin()) return
        executor().execute {
            try {
                promoteToMagisk(connection)
            } finally {
                if (promotionState.finishAttempt()) schedulePromotion(connection)
            }
        }
    }

    private fun scheduleDeadHostRecovery() {
        val scheduledSession = runtimeSession.get()
        if (scheduledSession.host != DaemonHostToken.MagiskBackend || scheduledSession.started) return
        val environment = lastEnvironment.get() ?: return
        if (!deadHostRecoveryScheduled.compareAndSet(false, true)) return
        executor().execute {
            try {
                Thread.sleep(DEAD_HOST_RECOVERY_DELAY_MILLIS)
                val observedSession = runtimeSession.get()
                if (observedSession.host != DaemonHostToken.MagiskBackend || observedSession.started ||
                    companion.connection() != null
                ) return@execute
                val result = runCatching {
                    JSONObject(
                        NativeRuntime.nativeRecoverDeadMagiskHost(
                            canonicalBase.absolutePath,
                            environment,
                        ),
                    )
                }.getOrNull() ?: return@execute
                if (!result.optBoolean("ready", false)) {
                    runtimeSession.compareAndSet(
                        observedSession,
                        inactiveSession(
                            DaemonHostToken.MagiskBackend,
                            result.optString("code", "RUNTIME_UNAVAILABLE"),
                        ),
                    )
                    return@execute
                }
                val activated = activeSession(
                    DaemonHostToken.ApkRuntime,
                    result.getString("runtime_epoch"),
                    result.getLong("host_generation"),
                    result.getString("runtime_instance_id"),
                )
                if (!runtimeSession.compareAndSet(observedSession, activated)) return@execute
                replayCompanionFacts()
                registerPlatformFacts()
                val guard = File(
                    application.applicationInfo.nativeLibraryDir,
                    "libdroidbridge_exec_guard.so",
                )
                if (!NativeRuntime.nativeProbeAppGuard(guard.absolutePath)) {
                    NativeRuntime.nativeRecordHostFault("CLEANUP_UNVERIFIED", "app_guard_probe")
                }
                guardScopeSink.get()?.invoke()
                hintSink.get()?.invoke("context.status")
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
            } finally {
                deadHostRecoveryScheduled.set(false)
            }
        }
    }

    private fun scheduleDemotion(
        connection: DaemonConnection,
        recoverOnly: Boolean,
        sourceInstance: String?,
    ) {
        if (runtimeSession.get().host != DaemonHostToken.MagiskBackend || !connection.isHealthy()) return
        if (!demotionRunning.compareAndSet(false, true)) return
        executor().execute {
            try {
                demoteToApk(connection, recoverOnly, sourceInstance)
            } finally {
                demotionRunning.set(false)
            }
        }
    }

    private fun demoteToApk(
        connection: DaemonConnection,
        recoverOnly: Boolean,
        sourceInstance: String?,
    ) {
        val environment = lastEnvironment.get() ?: return
        var prepared = JSONObject(
            NativeRuntime.nativePrepareRemoteHostTransition(
                canonicalBase.absolutePath,
                DaemonHostToken.ApkRuntime.wire,
            ),
        )
        if (!prepared.optBoolean("prepared", false)) {
            recordTransitionFailure(prepared, "host_transition_prepare_remote")
            return
        }
        if (prepared.optBoolean("recovery", false)) {
            val priorIntent = prepared.getJSONObject("intent")
            val priorIntentJson = priorIntent.toString()
            val owner = observeOwner()
            val recoveredCommit = commitRemoteTransition(priorIntentJson)
            if (!recoveredCommit.has("code")) {
                activateApkTransition(environment, priorIntentJson)
                return
            }
            if (recoveredCommit.getString("code") != DaemonErrorToken.HostTransitionPending.wire) {
                recordTransitionFailure(recoveredCommit, "host_transition_recover_commit")
                return
            }
            if (!abortRemoteTransition(connection, owner, priorIntent, priorIntentJson)) {
                val retriedCommit = commitRemoteTransition(priorIntentJson)
                if (!retriedCommit.has("code")) {
                    activateApkTransition(environment, priorIntentJson)
                } else if (
                    retriedCommit.getString("code") !=
                    DaemonErrorToken.HostTransitionPending.wire
                ) {
                    recordTransitionFailure(retriedCommit, "host_transition_recover_retry")
                }
                return
            }
            if (recoverOnly) {
                if (sourceInstance == null || !establishMagiskHost(owner, sourceInstance)) {
                    recordSessionFailure(DaemonErrorToken.StaleAuthority.wire)
                    NativeRuntime.nativeRecordHostFault(
                        DaemonErrorToken.StaleAuthority.wire,
                        "host_transition_recover_abort",
                    )
                }
                return
            }
            prepared = JSONObject(
                NativeRuntime.nativePrepareRemoteHostTransition(
                    canonicalBase.absolutePath,
                    DaemonHostToken.ApkRuntime.wire,
                ),
            )
            if (!prepared.optBoolean("prepared", false) || prepared.optBoolean("recovery", false)) {
                recordTransitionFailure(prepared, "host_transition_prepare_retry")
                return
            }
        }
        val intent = prepared.getJSONObject("intent")
        val intentJson = intent.toString()
        val sourceOwner = observeOwner()
        val prepareResponse = runCatching {
            connection.request(
                DaemonOperationToken.HostPrepareTransition,
                buildJsonObject {
                    put("transition_id", intent.getString("transition_id"))
                    put("runtime_epoch", intent.getString("runtime_epoch"))
                    put("from_host", intent.getString("from_host"))
                    put("from_generation", intent.getLong("from_generation"))
                    put("from_instance_id", intent.getString("from_instance_id"))
                    put("target_host", DaemonHostToken.ApkRuntime.wire)
                },
                sourceOwner,
                5_000,
            ).use { it.envelope.payload.jsonObject }
        }.getOrNull() ?: return
        if (!controlAcknowledged(prepareResponse, "prepared")) {
            NativeRuntime.nativeAbortRemoteHostTransition(canonicalBase.absolutePath, intentJson)
            return
        }
        val sourceSession = runtimeSession.get()
        if (
            sourceInstance == null ||
            sourceSession.host != DaemonHostToken.MagiskBackend ||
            !sourceSession.validates(
                sourceOwner.runtimeEpoch,
                sourceOwner.hostGeneration,
                sourceInstance,
            )
        ) {
            abortRemoteTransition(connection, sourceOwner, intent, intentJson)
            return
        }
        val withdrawnSession = inactiveSession(
            DaemonHostToken.MagiskBackend,
            DaemonErrorToken.HostTransitionPending.wire,
        )
        if (!runtimeSession.compareAndSet(sourceSession, withdrawnSession)) {
            abortRemoteTransition(connection, sourceOwner, intent, intentJson)
            return
        }
        val releaseResponse = runCatching {
            connection.request(
                DaemonOperationToken.HostRelease,
                buildJsonObject { put("transition_id", intent.getString("transition_id")) },
                sourceOwner,
                5_000,
            ).use { it.envelope.payload.jsonObject }
        }.getOrNull()
        if (releaseResponse != null && !controlAcknowledged(releaseResponse, "released")) {
            if (abortRemoteTransition(connection, sourceOwner, intent, intentJson)) {
                restoreMagiskAfterAbort(sourceOwner, sourceInstance, "host_transition_release_abort")
            }
            return
        }
        var committed = commitRemoteTransition(intentJson)
        if (committed.has("code")) {
            if (committed.getString("code") == DaemonErrorToken.HostTransitionPending.wire) {
                if (abortRemoteTransition(connection, sourceOwner, intent, intentJson)) {
                    restoreMagiskAfterAbort(sourceOwner, sourceInstance, "host_transition_commit_abort")
                    return
                }
                committed = commitRemoteTransition(intentJson)
                if (!committed.has("code")) {
                    activateApkTransition(environment, intentJson)
                }
            } else {
                recordTransitionFailure(committed, "host_transition_commit")
            }
            return
        }
        activateApkTransition(environment, intentJson)
    }

    private fun abortRemoteTransition(
        connection: DaemonConnection,
        owner: DaemonOwnerFence,
        intent: JSONObject,
        intentJson: String,
    ): Boolean {
        val response = runCatching {
            connection.request(
                DaemonOperationToken.HostAbortTransition,
                buildJsonObject { put("transition_id", intent.getString("transition_id")) },
                owner,
                5_000,
            ).use { it.envelope.payload.jsonObject }
        }.getOrNull() ?: return false
        return controlAcknowledged(response, "aborted") &&
            NativeRuntime.nativeAbortRemoteHostTransition(canonicalBase.absolutePath, intentJson)
    }

    private fun commitRemoteTransition(intentJson: String): JSONObject = JSONObject(
        NativeRuntime.nativeCommitHostTransition(canonicalBase.absolutePath, intentJson),
    )

    private fun recordTransitionFailure(result: JSONObject, phase: String) {
        val code = result.optString("code", DaemonErrorToken.StaleAuthority.wire)
        recordSessionFailure(code)
        NativeRuntime.nativeRecordHostFault(code, phase)
    }

    private fun establishMagiskHost(
        owner: DaemonOwnerFence,
        instance: String,
        committedTransition: Boolean = false,
    ): Boolean {
        val observedSession = runtimeSession.get()
        val validated = if (committedTransition) {
            NativeRuntime.nativeFinishCommittedHostTransition(canonicalBase.absolutePath, instance)
        } else {
            NativeRuntime.nativeValidateLiveMagiskHost(canonicalBase.absolutePath, instance)
        }
        if (!validated) {
            return false
        }
        val activated = activeSession(
            DaemonHostToken.MagiskBackend,
            owner.runtimeEpoch,
            owner.hostGeneration,
            instance,
        )
        if (!runtimeSession.compareAndSet(observedSession, activated)) return false
        registerPlatformFacts()
        publishFrameworkPrimitives(owner.hostGeneration)
        adoptCompanionGuardScope(owner.runtimeEpoch, owner.hostGeneration, instance)
        scheduleNetworkAttachment()
        hintSink.get()?.invoke("context.status")
        return true
    }

    /**
     * Adopts the guard scope of the Magisk host this APK serves as an authenticated
     * companion, so the App and shell commands that host forwards here run under the
     * proof identity of the Runtime instance that admitted them. A scope this process
     * cannot adopt is recorded as the typed unavailability a command then reports, never
     * as a reason to run the command under a different identity.
     */
    private fun adoptCompanionGuardScope(runtimeEpoch: String, hostGeneration: Long, instance: String) {
        val guard = File(
            application.applicationInfo.nativeLibraryDir,
            "libdroidbridge_exec_guard.so",
        )
        val adopted = NativeRuntime.nativeAdoptCompanionGuardScope(
            canonicalBase.absolutePath,
            runtimeEpoch,
            instance,
            guard.absolutePath,
        )
        if (!adopted) {
            NativeRuntime.nativeRecordHostFault(
                DaemonErrorToken.CapabilityUnavailable.wire,
                "companion_guard_scope",
            )
        } else if (NativeRuntime.nativeProbeCompanionAppGuard(guard.absolutePath)) {
            register(
                "execution.app_guard",
                "available",
                "",
                hostGeneration,
                hasExecutor = true,
            )
        } else {
            register(
                "execution.app_guard",
                "unavailable",
                "CLEANUP_UNVERIFIED",
                hostGeneration,
            )
            NativeRuntime.nativeRecordHostFault("CLEANUP_UNVERIFIED", "companion_guard_probe")
        }
        if (adopted) guardScopeSink.get()?.invoke()
    }

    /**
     * Publishes the App-local framework primitives at the live host generation, so a
     * request that names one resolves against the session that is actually live instead
     * of a generation an earlier host left behind.
     */
    private fun publishFrameworkPrimitives(generation: Long) {
        frameworkReadySink.get()?.invoke(generation)
    }

    private fun restoreMagiskAfterAbort(
        owner: DaemonOwnerFence,
        sourceInstance: String?,
        phase: String,
    ): Boolean {
        if (sourceInstance != null && establishMagiskHost(owner, sourceInstance)) return true
        recordSessionFailure(DaemonErrorToken.StaleAuthority.wire)
        NativeRuntime.nativeRecordHostFault(DaemonErrorToken.StaleAuthority.wire, phase)
        return false
    }

    private fun activateApkTransition(environment: String, intentJson: String) {
        val observedSession = runtimeSession.get()
        val activated = JSONObject(
            NativeRuntime.nativeStart(canonicalBase.absolutePath, environment),
        )
        if (!activated.optBoolean("ready", false)) {
            recordSessionFailure(activated.optString("code", "RUNTIME_UNAVAILABLE"))
            return
        }
        val instance = activated.getString("runtime_instance_id")
        if (!NativeRuntime.nativeFinishHostTransition(canonicalBase.absolutePath, intentJson, instance)) return
        val targetSession = activeSession(
            DaemonHostToken.ApkRuntime,
            activated.getString("runtime_epoch"),
            activated.getLong("host_generation"),
            instance,
        )
        if (!runtimeSession.compareAndSet(observedSession, targetSession)) return
        replayCompanionFacts()
        registerPlatformFacts()
        publishFrameworkPrimitives(activated.getLong("host_generation"))
        val guard = File(
            application.applicationInfo.nativeLibraryDir,
            "libdroidbridge_exec_guard.so",
        )
        if (!NativeRuntime.nativeProbeAppGuard(guard.absolutePath)) {
            NativeRuntime.nativeRecordHostFault("CLEANUP_UNVERIFIED", "app_guard_probe")
        }
        guardScopeSink.get()?.invoke()
        hintSink.get()?.invoke("context.status")
    }

    private fun promoteToMagisk(connection: DaemonConnection) {
        val sourceSession = runtimeSession.get()
        if (!sourceSession.started || sourceSession.host != DaemonHostToken.ApkRuntime) return
        // Update maintenance and module exclusion pin the desired host to APK (S-AUTH-001).
        if (requiresApkHost()) return
        val prepared = JSONObject(
            NativeRuntime.nativePrepareHostTransition(DaemonHostToken.MagiskBackend.wire),
        )
        if (!prepared.optBoolean("prepared", false)) return
        val intent = prepared.getJSONObject("intent")
        val intentJson = intent.toString()
        val sourceOwner = observeOwner()
        val preparePayload = buildJsonObject {
            put("transition_id", intent.getString("transition_id"))
            put("runtime_epoch", intent.getString("runtime_epoch"))
            put("from_host", intent.getString("from_host"))
            put("from_generation", intent.getLong("from_generation"))
            put("from_instance_id", intent.getString("from_instance_id"))
            put("target_host", intent.getString("target_host"))
        }
        val targetPrepared = runCatching {
            connection.request(
                DaemonOperationToken.HostPrepareTransition,
                preparePayload,
                sourceOwner,
                5_000,
            )
                .use { it.envelope }
        }.getOrNull()
        if (targetPrepared == null || targetPrepared.payload.jsonObject.containsKey("error")) {
            NativeRuntime.nativeAbortHostTransition(intentJson)
            return
        }
        val withdrawnSession = inactiveSession(
            DaemonHostToken.ApkRuntime,
            DaemonErrorToken.HostTransitionPending.wire,
        )
        if (!runtimeSession.compareAndSet(sourceSession, withdrawnSession)) {
            NativeRuntime.nativeAbortHostTransition(intentJson)
            return
        }
        if (!NativeRuntime.nativeReleaseHostTransition(intentJson)) {
            NativeRuntime.nativeAbortHostTransition(intentJson)
            runtimeSession.compareAndSet(withdrawnSession, sourceSession)
            return
        }
        runCatching { apkProjectionReleasedSink.get()?.invoke() }
            .onFailure {
                // A retained alarm only reaches a non-APK session and delivers nothing there.
                NativeRuntime.nativeRecordHostFault(
                    DaemonErrorToken.IoError.wire,
                    "automation_alarm_release",
                )
            }
        val committed = JSONObject(
            NativeRuntime.nativeCommitHostTransition(canonicalBase.absolutePath, intentJson),
        )
        if (committed.has("code")) {
            recordSessionFailure(committed.getString("code"))
            return
        }
        val targetPendingSession = inactiveSession(
            DaemonHostToken.MagiskBackend,
            DaemonErrorToken.HostTransitionPending.wire,
        )
        if (!runtimeSession.compareAndSet(withdrawnSession, targetPendingSession)) return
        val targetOwner = DaemonOwnerFence(
            runtimeEpoch = committed.getString("runtime_epoch"),
            host = DaemonProtocol.decodeHost(committed.getString("host")),
            hostGeneration = committed.getLong("host_generation"),
            runtimeInstanceId = null,
        )
        val activated = runCatching {
            connection.request(
                DaemonOperationToken.HostActivate,
                buildJsonObject {
                    put("transition_id", intent.getString("transition_id"))
                    put("runtime_epoch", targetOwner.runtimeEpoch)
                    put("host_generation", targetOwner.hostGeneration)
                    put("target_host", DaemonHostToken.MagiskBackend.wire)
                },
                targetOwner,
                5_000,
            ).use { it.envelope }
        }.getOrNull() ?: return
        if (activated.payload.jsonObject.containsKey("error")) return
        val instance = activated.payload.jsonObject
            .getValue("runtime_instance_id")
            .jsonPrimitive
            .content
        if (!NativeRuntime.nativeFinishHostTransition(canonicalBase.absolutePath, intentJson, instance)) return
        val targetSession = activeSession(
            DaemonHostToken.MagiskBackend,
            targetOwner.runtimeEpoch,
            targetOwner.hostGeneration,
            instance,
        )
        if (!runtimeSession.compareAndSet(targetPendingSession, targetSession)) return
        // A promoted host starts as bare as an established one: it holds none of this companion's
        // facts, framework primitives or guard scope until they are published at its generation.
        registerPlatformFacts()
        publishFrameworkPrimitives(targetOwner.hostGeneration)
        adoptCompanionGuardScope(targetOwner.runtimeEpoch, targetOwner.hostGeneration, instance)
        // The host's binding is per-process and never inherited: a daemon just made the host has
        // none, exactly as on establishment, so this path asserts the same fact.
        scheduleNetworkAttachment()
        hintSink.get()?.invoke("context.status")
    }

    private fun activeSession(
        host: DaemonHostToken,
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): RuntimeSessionState = RuntimeSessionState(
        started = true,
        host = host,
        activeFence = RuntimeFence(runtimeEpoch, hostGeneration, runtimeInstanceId),
        startFailure = "",
    )

    private fun inactiveSession(host: DaemonHostToken?, code: String): RuntimeSessionState =
        RuntimeSessionState(
            started = false,
            host = host,
            activeFence = null,
            startFailure = code.ifEmpty { "RUNTIME_UNAVAILABLE" },
        )

    private fun recordSessionFailure(code: String) {
        runtimeSession.updateAndGet { current ->
            if (current.started) current else inactiveSession(current.host, code)
        }
    }

    private fun executor(): ExecutorService {
        transitionExecutor.get()?.let { return it }
        val created = Executors.newSingleThreadExecutor { task ->
            Thread(task, "droidbridge-host-transition").apply { isDaemon = true }
        }
        if (transitionExecutor.compareAndSet(null, created)) return created
        created.shutdownNow()
        return requireNotNull(transitionExecutor.get())
    }
}

internal fun shouldRecoverUncommittedApkTransition(
    startCode: String,
    ownerHost: DaemonHostToken,
    transitionState: String,
): Boolean =
    startCode == DaemonErrorToken.HostTransitionPending.wire &&
        ownerHost == DaemonHostToken.ApkRuntime &&
        transitionState == "source_pending"

internal fun shouldResumeCommittedApkTransition(
    startCode: String,
    ownerHost: DaemonHostToken,
    transitionState: String,
): Boolean =
    startCode == DaemonErrorToken.HostTransitionPending.wire &&
        ownerHost == DaemonHostToken.ApkRuntime &&
        transitionState == "target_committed"

internal class HostPromotionState {
    private var backendReady = false
    private var running = false
    private var idleHintPending = false

    @Synchronized
    fun observeBackendReady() {
        backendReady = true
        idleHintPending = true
    }

    @Synchronized
    fun observeIdleHint() {
        if (backendReady) idleHintPending = true
    }

    @Synchronized
    fun clearBackend() {
        backendReady = false
        idleHintPending = false
    }

    @Synchronized
    fun tryBegin(): Boolean {
        if (!backendReady || running || !idleHintPending) return false
        running = true
        idleHintPending = false
        return true
    }

    @Synchronized
    fun finishAttempt(): Boolean {
        running = false
        return backendReady && idleHintPending
    }
}

internal class RuntimeStartException(val code: String) : IllegalStateException(code)

/**
 * The one failure envelope this process answers for a submission it could not serve. It is the
 * Runtime envelope's own shape, so a caller reads the code and the reason the failure actually has
 * instead of a transport failure that hides both. Every code it carries is one the Runtime names.
 */
internal fun runtimeFailureEnvelope(requestId: String, code: String, message: String?): ByteArray =
    JSONObject()
        .put("protocol_version", 1)
        .put("request_id", requestId)
        .put("outcome", "error")
        .put(
            "error",
            JSONObject()
                .put("code", runtimeErrorToken(code))
                .put("operation", RUNTIME_SUBMIT_OPERATION)
                .put("retryable", false)
                .apply { message?.let { put("message", it) } },
        )
        .toString()
        .toByteArray(Charsets.UTF_8)

/**
 * A code this process does not recognize is not a code the Runtime can have answered, so it is
 * reported as the unavailability it is rather than passed through.
 */
internal fun runtimeErrorToken(code: String): String =
    DaemonErrorToken.entries.firstOrNull { it.wire == code }?.wire
        ?: DaemonErrorToken.CapabilityUnavailable.wire

/**
 * The code one submission failure carries. A failure the host named keeps its code; anything else
 * is the internal error it is.
 */
internal fun runtimeFailureCode(failure: Throwable): String =
    (failure as? RuntimeStartException)?.code ?: DaemonErrorToken.InternalError.wire

/**
 * The reason one unnamed submission failure carries: the failure's own class and text, bounded, so
 * a caller learns what happened without reading a duplicate of the code it already has.
 */
internal fun runtimeFailureMessage(failure: Throwable): String? {
    if (failure is RuntimeStartException) return null
    val detail = failure.message.orEmpty()
    val named = failure::class.java.simpleName
    val text = if (detail.isEmpty()) named else "$named: $detail"
    return text.take(MAX_FAILURE_MESSAGE_CHARS)
}

/** The `request_id` one submission envelope names, or null when it is not a submission at all. */
internal fun submissionRequestId(envelope: ByteArray): String? =
    runCatching {
        Json.parseToJsonElement(envelope.decodeToString())
            .jsonObject["request_id"]?.jsonPrimitive?.content
    }.getOrNull()?.takeIf { it.isNotEmpty() }

/** A reason long enough to name the failure and short enough to stay one bounded envelope. */
private const val MAX_FAILURE_MESSAGE_CHARS = 256
private const val RUNTIME_SUBMIT_OPERATION = "runtime.submit"
