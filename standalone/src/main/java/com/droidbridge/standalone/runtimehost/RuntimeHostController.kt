package com.droidbridge.standalone.runtimehost

import android.Manifest
import android.app.AlarmManager
import android.app.Application
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelFileDescriptor
import com.droidbridge.standalone.BuildConfig
import com.droidbridge.standalone.execution.android.NetworkDefaultObservation
import com.droidbridge.standalone.product.release.ReleaseConfig
import com.droidbridge.ui.product.deviceName
import java.io.File
import java.time.ZoneId
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.json.JSONObject

private const val DIAGNOSTICS_DEADLINE_MILLIS = 2_000L
private const val MAINTENANCE_RESET = """{"schema_version":1,"reset":true}"""
private const val OWNER_CORRUPT = "owner_corrupt"

internal data class RuntimeFence(
    val runtimeEpoch: String,
    val hostGeneration: Long,
    val runtimeInstanceId: String,
)

internal data class RuntimeSessionState(
    val started: Boolean = false,
    val activeFence: RuntimeFence? = null,
    val startFailure: String = "RUNTIME_UNAVAILABLE",
) {
    init {
        require(started == (activeFence != null))
        require(!started || startFailure.isEmpty())
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

/**
 * The App's one Runtime host: it starts the native Runtime in this process, publishes the App's
 * platform facts to it and owns every maintenance operation on its canonical store.
 */
internal class RuntimeHostController(
    private val application: Application,
) {
    private val runtimeSession = AtomicReference(RuntimeSessionState())
    private val hintSink = AtomicReference<((String) -> Unit)?>(null)
    private val frameworkReadySink = AtomicReference<((Long) -> Unit)?>(null)
    private val guardScopeSink = AtomicReference<(() -> Unit)?>(null)
    private val apkProjectionReleasedSink = AtomicReference<(() -> Unit)?>(null)
    private val platformGeneration = AtomicLong(0)
    private val capabilityFacts = CapabilityFacts()
    private val maintenanceExecutor = AtomicReference<ExecutorService?>(null)
    private val deviceContext = application.createDeviceProtectedStorageContext()
    private val canonicalBase = File(deviceContext.filesDir, "droidbridge")

    /** One bounded worker for S-UI-017 status reads; a timed-out read never blocks the next caller. */
    private val diagnosticsReads = Executors.newSingleThreadExecutor { task ->
        Thread(task, "droidbridge-diagnostics").apply { isDaemon = true }
    }

    @Synchronized
    fun start(): Boolean {
        val observedSession = runtimeSession.get()
        if (observedSession.started) return true
        // A reset in progress owns the store until it activates the fresh instance itself.
        if (observedSession.startFailure == ErrorToken.HostTransitionPending.wire) return false
        val packageInfo = application.packageManager.getPackageInfo(application.packageName, 0)
        val environment = JSONObject()
            .put("sdk_int", Build.VERSION.SDK_INT)
            .put("abi", Build.SUPPORTED_ABIS.firstOrNull().orEmpty())
            .put("timezone", ZoneId.systemDefault().id)
            .put("name", deviceName(application))
            .put("manufacturer", Build.MANUFACTURER)
            .put("model", Build.MODEL)
            .put("device", Build.DEVICE)
            .put("build_fingerprint", Build.FINGERPRINT)
            .put("version_name", packageInfo.versionName.orEmpty())
            .put("version_code", packageInfo.longVersionCode)
            .put("runtime_epoch", "00000000-0000-4000-8000-000000000000")
            .put("host_generation", 1)
        if (File(canonicalBase, "runtime-reset-intent.json").exists()) {
            // A recorded S-UPD-006 reset only moves forward, and before any activation (S-UI-017).
            val recovered = runCatching {
                NativeRuntime.nativeResetRuntimeData(canonicalBase.absolutePath)?.let(::JSONObject)
            }.getOrNull()
            if (recovered?.optBoolean("reset", false) != true) {
                runtimeSession.compareAndSet(
                    observedSession,
                    inactiveSession(recovered?.optString("code").orEmpty().ifEmpty { ErrorToken.IoError.wire }),
                )
                return false
            }
        }
        val result = runCatching {
            JSONObject(NativeRuntime.nativeStart(canonicalBase.absolutePath, environment.toString()))
        }.getOrNull() ?: return false
        if (!result.optBoolean("ready", false)) {
            runtimeSession.compareAndSet(
                observedSession,
                inactiveSession(result.optString("code", "RUNTIME_UNAVAILABLE")),
            )
            return false
        }
        val generation = result.getLong("host_generation")
        val activated = activeSession(
            result.getString("runtime_epoch"),
            generation,
            result.getString("runtime_instance_id"),
        )
        if (!runtimeSession.compareAndSet(observedSession, activated)) {
            NativeRuntime.nativeRecordHostFault(ErrorToken.StaleAuthority.wire, "host_start_projection")
            return runtimeSession.get().started
        }
        replayFacts()
        registerPlatformFacts()
        frameworkReadySink.get()?.invoke(generation)
        recoverMaintenance()
        val guard = File(application.applicationInfo.nativeLibraryDir, "libdroidbridge_exec_guard.so")
        NativeRuntime.nativeProbeAppGuard(guard.absolutePath)
        guardScopeSink.get()?.invoke()
        return true
    }

    /**
     * Replays App facts recorded before the Runtime started into it, so those sources are not
     * stranded as not ready until they next change.
     */
    private fun replayFacts() {
        capabilityFacts.replay().forEach { fact ->
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
        val recorded = capabilityFacts.register(
            key,
            state,
            if (state == "available") "" else reason,
            generation,
            hasExecutor,
        )
        if (!recorded) return false
        if (!runtimeSession.get().started) {
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
        return accepted
    }

    fun submit(envelope: ByteArray): ByteArray {
        if (!start()) throw RuntimeStartException(runtimeSession.get().startFailure)
        val fence = runtimeSession.get().activeFence ?: throw RuntimeStartException(runtimeSession.get().startFailure)
        frameworkReadySink.get()?.invoke(fence.hostGeneration)
        registerPlatformFacts()
        return NativeRuntime.nativeSubmit(envelope)
    }

    /** Answers one S-MCP-006 internal artifact query from this host's own artifact store. */
    fun queryArtifacts(query: ByteArray): McpArtifactQueryReply {
        if (!start()) throw RuntimeStartException(runtimeSession.get().startFailure)
        val slot = intArrayOf(-1)
        val payload = NativeRuntime.nativeQueryArtifacts(query, slot)
        val descriptor = slot[0].takeIf { it >= 0 }?.let(ParcelFileDescriptor::adoptFd)
        if (payload == null) {
            descriptor?.close()
            throw RuntimeStartException(ErrorToken.InternalError.wire)
        }
        return McpArtifactQueryReply(payload, descriptor)
    }

    /** S-UI-017 `getMaintenanceState`: canonical-file and guard-proof facts only; no Core starts. */
    fun maintenanceState(): String {
        val result = runCatching {
            NativeRuntime.nativeMaintenanceState(canonicalBase.absolutePath)?.let(::JSONObject)
        }.getOrNull() ?: return maintenanceFailure(ErrorToken.InternalError.wire)
        if (result.has("code")) return maintenanceFailure(result.getString("code"))
        return result.toString()
    }

    private val maintenanceHost = object : MaintenanceHost {
        override fun ensureApkHost(): String? =
            if (start()) null else runtimeSession.get().startFailure.ifEmpty { ErrorToken.HostTransitionPending.wire }

        override fun closeAdmission(): String? {
            val result = runCatching { NativeRuntime.nativeCloseAdmissionForMaintenance()?.let(::JSONObject) }.getOrNull()
            return if (result?.optBoolean("closed", false) == true) {
                null
            } else {
                result?.optString("code").orEmpty().ifEmpty { ErrorToken.InternalError.wire }
            }
        }

        override fun reopenAdmission(): Boolean = NativeRuntime.nativeReopenAdmission(canonicalBase.absolutePath)
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
    fun updateMaintenanceState(): String = onMaintenanceExecutor {
        runCatching { maintenance.recover() }
            .onFailure { NativeRuntime.nativeRecordHostFault(ErrorToken.IoError.wire, "update_maintenance_recover") }
        maintenance.state()
    }

    fun beginProductUpdate(manifest: ByteArray, signature: ByteArray): String =
        onMaintenanceExecutor { maintenance.beginProductUpdate(manifest, signature) }

    fun installUpdateApk(updateId: String): String = onMaintenanceExecutor { maintenance.installApk(updateId) }

    fun cancelUpdate(updateId: String): String = onMaintenanceExecutor { maintenance.cancel(updateId) }

    /** Observation-based maintenance reconciliation; a failure is recorded, never retried silently. */
    private fun recoverMaintenance() {
        executor().execute {
            runCatching { maintenance.recover() }
                .onFailure { NativeRuntime.nativeRecordHostFault(ErrorToken.IoError.wire, "update_maintenance_recover") }
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
                ?.let { response -> runCatching { Json.parseToJsonElement(response.decodeToString()).jsonObject }.getOrNull() }
                ?.takeIf { response -> response["outcome"]?.jsonPrimitive?.contentOrNull == "success" }
                ?.get("result")
        } else {
            null
        }
        return buildJsonObject {
            put("schema_version", 1)
            put("session", buildJsonObject {
                put("started", session.started)
                put("host", "apk_runtime")
                if (!session.started) put("start_failure", session.startFailure)
            })
            status?.let { put("status", it) }
        }.toString()
    }

    /** Executions a lost instance left running, which stop the Runtime from starting again. */
    fun strandedExecutions(): Int = NativeRuntime.nativeStrandedExecutions(canonicalBase.absolutePath)

    /**
     * Settles those executions as interrupted so the Runtime can start again. The native side
     * refuses while a live Runtime owns the store, so a working host is never touched.
     */
    fun clearStrandedExecutions(): String = onMaintenanceExecutor {
        val reply = NativeRuntime.nativeClearStrandedExecutions(canonicalBase.absolutePath)
            ?: return@onMaintenanceExecutor maintenanceFailure(ErrorToken.InternalError.wire)
        if (JSONObject(reply).has("cleared")) start()
        reply
    }

    /**
     * S-UI-017 `resetRuntimeData`. A malformed owner is replaced; otherwise the live instance is
     * released, the store emptied, and a fresh instance is active before success returns.
     */
    fun resetRuntimeData(): String = onMaintenanceExecutor {
        val blocker = runCatching { JSONObject(maintenanceState()).optString("blocker") }.getOrNull()
        if (!runtimeSession.get().started && blocker == OWNER_CORRUPT) {
            val result = runCatching {
                NativeRuntime.nativeResetRuntimeHostToApk(canonicalBase.absolutePath)?.let(::JSONObject)
            }.getOrNull()
            if (result?.optBoolean("reset", false) != true) {
                return@onMaintenanceExecutor maintenanceFailure(
                    result?.optString("code").orEmpty().ifEmpty { ErrorToken.InternalError.wire },
                )
            }
            return@onMaintenanceExecutor activateAfterMaintenance()
        }
        val source = runtimeSession.get()
        val withdrawn = inactiveSession(ErrorToken.HostTransitionPending.wire)
        if (source.started && !runtimeSession.compareAndSet(source, withdrawn)) {
            return@onMaintenanceExecutor maintenanceFailure(ErrorToken.HostTransitionPending.wire)
        }
        val result = runCatching {
            NativeRuntime.nativeResetRuntimeData(canonicalBase.absolutePath)?.let(::JSONObject)
        }.getOrNull()
        val reset = result?.optBoolean("reset", false) == true
        val code = result?.optString("code").orEmpty().ifEmpty { ErrorToken.InternalError.wire }
        if (source.started) {
            if (reset || result?.optBoolean("released", false) == true) {
                releaseApkProjection()
                if (!reset) runtimeSession.compareAndSet(withdrawn, inactiveSession(code))
            } else {
                // The refused reset reopened the unchanged instance.
                runtimeSession.compareAndSet(withdrawn, source)
            }
        }
        if (!reset) return@onMaintenanceExecutor maintenanceFailure(code)
        activateAfterMaintenance()
    }

    private fun activateAfterMaintenance(): String {
        runtimeSession.updateAndGet { current ->
            if (current.started) current else inactiveSession("RUNTIME_UNAVAILABLE")
        }
        return if (start()) MAINTENANCE_RESET else maintenanceFailure(runtimeSession.get().startFailure)
    }

    private fun releaseApkProjection() {
        runCatching { apkProjectionReleasedSink.get()?.invoke() }
            .onFailure {
                // A retained alarm reaches only a released instance and delivers nothing there.
                NativeRuntime.nativeRecordHostFault(ErrorToken.IoError.wire, "automation_alarm_release")
            }
    }

    private fun onMaintenanceExecutor(action: () -> String): String =
        runCatching { executor().submit<String> { action() }.get() }
            .getOrElse { maintenanceFailure(ErrorToken.InternalError.wire) }

    private fun maintenanceFailure(code: String): String =
        JSONObject().put("schema_version", 1).put("error", code).toString()

    fun validatesFence(
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): Boolean = runtimeSession.get().validates(runtimeEpoch, hostGeneration, runtimeInstanceId)

    /**
     * Delivers one exact-alarm or reconcile broadcast to the Runtime's Automation scheduler, which
     * rescans canonical due truth and re-arms its single alarm (S-LIFE-003/004).
     */
    fun wakeAutomation(): Boolean {
        registerPlatformFacts()
        if (!start()) return false
        return NativeRuntime.nativeAutomationWake()
    }

    fun publishNetworkDefault(observed: NetworkDefaultObservation): Boolean {
        if (
            !runtimeSession.get().validates(
                observed.runtimeEpoch,
                observed.hostGeneration,
                observed.runtimeInstanceId,
            )
        ) return true
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

    fun setHintSink(sink: ((String) -> Unit)?) {
        hintSink.set(sink)
    }

    fun setFrameworkReadySink(sink: ((Long) -> Unit)?) {
        frameworkReadySink.set(sink)
    }

    /**
     * Receives each replacement of the execution-guard scope, so identity guards whose
     * proofs belong to that scope are proven against the scope that is now live.
     */
    fun setGuardScopeSink(sink: (() -> Unit)?) {
        guardScopeSink.set(sink)
    }

    /**
     * Receives the release of the Runtime instance, so its wake projection (the single exact alarm)
     * is cancelled before a new instance builds its own (S-LIFE-003).
     */
    fun setApkProjectionReleasedSink(sink: (() -> Unit)?) {
        apkProjectionReleasedSink.set(sink)
    }

    private fun activeSession(
        runtimeEpoch: String,
        hostGeneration: Long,
        runtimeInstanceId: String,
    ): RuntimeSessionState = RuntimeSessionState(
        started = true,
        activeFence = RuntimeFence(runtimeEpoch, hostGeneration, runtimeInstanceId),
        startFailure = "",
    )

    private fun inactiveSession(code: String): RuntimeSessionState =
        RuntimeSessionState(
            started = false,
            activeFence = null,
            startFailure = code.ifEmpty { "RUNTIME_UNAVAILABLE" },
        )

    private fun executor(): ExecutorService {
        maintenanceExecutor.get()?.let { return it }
        val created = Executors.newSingleThreadExecutor { task ->
            Thread(task, "droidbridge-host-maintenance").apply { isDaemon = true }
        }
        if (maintenanceExecutor.compareAndSet(null, created)) return created
        created.shutdownNow()
        return requireNotNull(maintenanceExecutor.get())
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
    ErrorToken.entries.firstOrNull { it.wire == code }?.wire
        ?: ErrorToken.CapabilityUnavailable.wire

/**
 * The code one submission failure carries. A failure the host named keeps its code; anything else
 * is the internal error it is.
 */
internal fun runtimeFailureCode(failure: Throwable): String =
    (failure as? RuntimeStartException)?.code ?: ErrorToken.InternalError.wire

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
