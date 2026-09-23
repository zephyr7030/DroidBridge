package com.droidbridge.android.runtimehost

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Binder
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.RemoteCallbackList
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.droidbridge.android.DroidBridgeApplication
import com.droidbridge.android.R
import com.droidbridge.android.execution.shizuku.ShizukuController
import com.droidbridge.android.execution.android.AccessibilityServiceStartupFact
import com.droidbridge.android.execution.android.MediaProjectionVisualController
import com.droidbridge.android.execution.android.NativeAndroidExecutionDispatcher
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import org.json.JSONObject

class DroidBridgeService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val events = RemoteCallbackList<IRuntimeEventCallback>()
    private val requests = ConcurrentHashMap<String, PendingRequest>()
    private lateinit var hostController: RuntimeHostController
    private lateinit var foregroundReasons: ForegroundReasonRegistry
    private lateinit var shizukuController: ShizukuController
    private lateinit var mediaProjection: MediaProjectionVisualController
    private lateinit var mcpSettings: McpSettingsController
    private lateinit var tunnelSettings: TunnelSettingsController
    private val mainHandler = Handler(Looper.getMainLooper())
    @Volatile private var activeTaskCount = 0L
    @Volatile private var latestStartId = 0

    private data class PendingRequest(
        val callback: AtomicReference<IRuntimeCallback?>,
        var job: Job? = null,
    )

    private val binder = object : IDroidBridgeRuntime.Stub() {
        override fun submit(envelope: ByteArray?, callback: IRuntimeCallback?) {
            verifyCaller()
            requireNotNull(envelope)
            requireNotNull(callback)
            require(envelope.isNotEmpty() && envelope.size <= MAX_ENVELOPE_BYTES)
            val requestId = requestId(envelope)
            val pending = PendingRequest(AtomicReference(callback))
            synchronized(requests) {
                require(requests.size < MAX_OUTSTANDING_REQUESTS)
                require(requests.putIfAbsent(requestId, pending) == null)
            }
            val deathRecipient = IBinder.DeathRecipient { pending.callback.set(null) }
            runCatching { callback.asBinder().linkToDeath(deathRecipient, 0) }
                .onFailure { pending.callback.set(null) }
            pending.job = scope.launch {
                try {
                    val response = runCatching { hostController.submit(envelope) }
                        .getOrElse { error ->
                            unavailableEnvelope(
                                requestId,
                                (error as? RuntimeStartException)?.code
                                    ?: DaemonErrorToken.CapabilityUnavailable.wire,
                            )
                        }
                    pending.callback.getAndSet(null)?.let { current ->
                        runCatching { current.onResponse(response) }
                    }
                } finally {
                    requests.remove(requestId, pending)
                    runCatching { callback.asBinder().unlinkToDeath(deathRecipient, 0) }
                }
            }
        }

        override fun cancelRequest(requestId: String?) {
            verifyCaller()
            if (requestId == null) return
            requests[requestId]?.let { pending ->
                pending.callback.set(null)
                pending.job?.cancel()
            }
        }

        override fun subscribe(callback: IRuntimeEventCallback?) {
            verifyCaller()
            requireNotNull(callback)
            events.register(callback)
            runCatching { callback.onEvent(PROJECTION_CONTEXT) }
        }

        override fun unsubscribe(callback: IRuntimeEventCallback?) {
            verifyCaller()
            if (callback != null) events.unregister(callback)
        }

        override fun requestCapabilityRecheck() {
            verifyCaller()
            hostController.registerPlatformFacts()
            shizukuController.recheck()
        }

        override fun requestShizukuAuthorization(): Boolean {
            verifyCaller()
            return shizukuController.requestAuthorization()
        }

        override fun getMcpSettings(): String {
            verifyCaller()
            return this@DroidBridgeService.mcpSettings.settings()
        }

        override fun setMcpEnabled(enabled: Boolean): String {
            verifyCaller()
            return this@DroidBridgeService.mcpSettings.setEnabled(enabled, ::setMcpForeground)
        }

        override fun rotateMcpToken(): String {
            verifyCaller()
            return this@DroidBridgeService.mcpSettings.rotate()
        }

        override fun revealMcpToken(): String {
            verifyCaller()
            return this@DroidBridgeService.mcpSettings.reveal()
        }

        override fun getTunnelSettings(): String {
            verifyCaller()
            return this@DroidBridgeService.tunnelSettings.settings()
        }

        override fun configureTunnel(tunnelId: String?, apiKey: String?): String {
            verifyCaller()
            return this@DroidBridgeService.tunnelSettings.configure(
                requireNotNull(tunnelId),
                requireNotNull(apiKey),
                ::setTunnelForeground,
            )
        }

        override fun setTunnelEnabled(enabled: Boolean): String {
            verifyCaller()
            return this@DroidBridgeService.tunnelSettings.setEnabled(enabled, ::setTunnelForeground)
        }

        override fun clearTunnel(): String {
            verifyCaller()
            return this@DroidBridgeService.tunnelSettings.clear(::setTunnelForeground)
        }

        override fun getMaintenanceState(): String {
            verifyCaller()
            return hostController.maintenanceState()
        }

        override fun getDiagnosticsSnapshot(): String {
            verifyCaller()
            return hostController.diagnosticsSnapshot()
        }

        override fun getStrandedExecutions(): Int {
            verifyCaller()
            return hostController.strandedExecutions()
        }

        override fun clearStrandedExecutions(): String {
            verifyCaller()
            return hostController.clearStrandedExecutions().also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun resetRuntimeData(): String {
            verifyCaller()
            return hostController.resetRuntimeData().also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun resetRuntimeHostToApk(): String {
            verifyCaller()
            return hostController.resetRuntimeHostToApk().also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun getUpdateMaintenance(): String {
            verifyCaller()
            return hostController.updateMaintenanceState()
        }

        override fun beginProductUpdate(manifest: ByteArray?, signature: ByteArray?): String {
            verifyCaller()
            return hostController.beginProductUpdate(requireNotNull(manifest), requireNotNull(signature))
                .also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun beginModuleRepair(manifest: ByteArray?, signature: ByteArray?): String {
            verifyCaller()
            return hostController.beginModuleRepair(requireNotNull(manifest), requireNotNull(signature))
                .also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun installUpdateApk(updateId: String?): String {
            verifyCaller()
            return hostController.installUpdateApk(requireNotNull(updateId))
        }

        override fun installUpdateModule(updateId: String?): String {
            verifyCaller()
            return hostController.installUpdateModule(requireNotNull(updateId)).also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun cancelUpdate(updateId: String?): String {
            verifyCaller()
            return hostController.cancelUpdate(requireNotNull(updateId)).also { publishHint(PROJECTION_CONTEXT) }
        }

        override fun continueWithoutModule(updateId: String?): String {
            verifyCaller()
            return hostController.continueWithoutModule(requireNotNull(updateId)).also { publishHint(PROJECTION_CONTEXT) }
        }
    }

    override fun onCreate() {
        super.onCreate()
        val graph = (application as DroidBridgeApplication).requireRuntimeGraph()
        hostController = graph.hostController
        mcpSettings = graph.mcpSettings
        tunnelSettings = graph.tunnelSettings
        hostController.setHintSink(::publishHint)
        foregroundReasons = ForegroundReasonRegistry(
            service = this,
            taskCount = { activeTaskCount },
            onEmpty = ::stopStartedIfIdle,
        )
        graph.setTaskActivitySink(::setTaskActivity)
        graph.setNetworkDefaultForegroundSink { active ->
            foregroundReasons.set(ForegroundReason.SpecialUse, active)
        }
        tunnelSettings.restore(::setTunnelForeground)
        hostController.start()
        mediaProjection = MediaProjectionVisualController(
            service = this,
            registry = graph.androidExecutionRegistry,
            display = graph.visualDisplay,
            encoder = graph.visualEncoder,
            validatesFence = hostController::validatesFence,
            setForeground = { active ->
                foregroundReasons.set(ForegroundReason.MediaProjection, active)
            },
        )
        mediaProjection.publishInitialUnavailable()
        AccessibilityServiceStartupFact.registration(AccessibilityServiceStartupFact.isEnabled(this))
            ?.let(graph.androidExecutionRegistry::register)
        shizukuController = ShizukuController(
            this@DroidBridgeService.application,
            graph.androidExecutionRegistry,
            hostController::validatesFence,
            scope,
        )
        graph.setShizukuPrimitiveBridge(shizukuController.primitiveBridge())
        shizukuController.start()
        tunnelSettings.enabledObserver = shizukuController::setKeepAliveWanted
        hostController.setGuardScopeSink(shizukuController::onGuardScopeReplaced)
    }

    override fun onBind(intent: Intent?): IBinder {
        hostController.start()
        restoreMcpForUi(intent)
        return binder
    }

    override fun onRebind(intent: Intent?) {
        hostController.start()
        restoreMcpForUi(intent)
    }

    /** Requests [onRebind], so every later UI bind can restart a stopped MCP listener. */
    override fun onUnbind(intent: Intent?): Boolean = true

    /**
     * Only the default-process client bind restarts an enabled listener from a bind (S-MCP-004);
     * broadcast keeper starts and NotificationListener binds never do. The root module's keep-alive
     * wake is the only other caller, and it asks only for a listener it saw this device run, so
     * boot alone still never starts MCP.
     */
    private fun restoreMcpForUi(intent: Intent?) {
        if (intent?.action != ACTION_UI_BIND) return
        scope.launch { mcpSettings.restore(::setMcpForeground) }
    }

    /**
     * An enabled listener needs a started Service, since a bound-only one ends with its last
     * client even while foreground. A rejected start is the listener's FGS_START_REJECTED.
     */
    private fun setMcpForeground(active: Boolean) {
        if (active) startService(Intent(this, DroidBridgeService::class.java))
        foregroundReasons.set(ForegroundReason.SpecialUse, active, MCP_FOREGROUND_OWNER)
    }

    /**
     * The root module starts this service as a foreground service when an enabled connection
     * has no Runtime answering it, so the service enters the foreground once to honor that start
     * and then keeps only the reasons its restored connections hold.
     */
    private fun keepAliveWake() {
        try {
            foregroundReasons.set(ForegroundReason.SpecialUse, true, KEEPALIVE_WAKE_OWNER)
        } catch (_: RuntimeException) {
            NativeRuntime.nativeRecordHostFault("FGS_START_REJECTED", "keepalive_wake")
            return
        }
        tunnelSettings.restore(::setTunnelForeground)
        // The daemon sends this for an enabled listener only once it has seen this device run one,
        // so restoring it here returns a listener the system ended; a boot still never opens one.
        mcpSettings.restore(::setMcpForeground)
        foregroundReasons.set(ForegroundReason.SpecialUse, false, KEEPALIVE_WAKE_OWNER)
    }

    private fun setTunnelForeground(active: Boolean) {
        if (active) startService(Intent(this, DroidBridgeService::class.java))
        foregroundReasons.set(ForegroundReason.SpecialUse, active, TUNNEL_FOREGROUND_OWNER)
    }

    private fun setTaskActivity(activeTasks: Long) {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            mainHandler.post { setTaskActivity(activeTasks) }
            return
        }
        val wasActive = activeTaskCount > 0
        activeTaskCount = activeTasks
        // A platform that refuses the start leaves the Tasks running without the hold, which is the
        // host fault the other wake paths already report; it never takes the Runtime process down.
        try {
            if (activeTasks > 0 && !wasActive) {
                ContextCompat.startForegroundService(this, Intent(this, DroidBridgeService::class.java))
            }
            foregroundReasons.set(ForegroundReason.Task, activeTasks > 0, TASK_FOREGROUND_OWNER)
        } catch (_: RuntimeException) {
            NativeRuntime.nativeRecordHostFault("FGS_START_REJECTED", TASK_FOREGROUND_OWNER)
        }
    }

    /**
     * The root module's daemon hosts the canonical Runtime, and this process serves the Android
     * primitives its Tasks execute. Its published count therefore holds this service in the
     * foreground exactly as an App-hosted Runtime's own count does. A start that carries no
     * readable count leaves no reason behind, so `onStartCommand` releases the start it made.
     */
    private fun daemonTaskActivity(intent: Intent) {
        val epoch = intent.getStringExtra(EXTRA_RUNTIME_EPOCH).orEmpty()
        val activeTasks = intent.getIntExtra(EXTRA_ACTIVE_TASKS, -1)
        val revision = intent.getLongExtra(EXTRA_CANONICAL_REVISION, -1L)
        if (epoch.isEmpty() || activeTasks < 0 || revision < 0) return
        NativeAndroidExecutionDispatcher.daemonTaskActivityChanged(
            epoch,
            activeTasks.toLong(),
            revision,
        )
    }

    private fun automationWake(phase: String) {
        try {
            foregroundReasons.set(
                ForegroundReason.SpecialUse,
                true,
                AUTOMATION_WAKE_OWNER,
            )
        } catch (_: RuntimeException) {
            NativeRuntime.nativeRecordHostFault("FGS_START_REJECTED", phase)
            return
        }
        scope.launch {
            try {
                if (!hostController.wakeAutomation()) {
                    NativeRuntime.nativeRecordHostFault("RUNTIME_UNAVAILABLE", phase)
                }
            } finally {
                foregroundReasons.set(
                    ForegroundReason.SpecialUse,
                    false,
                    AUTOMATION_WAKE_OWNER,
                )
            }
        }
    }

    private fun stopStartedIfIdle() {
        val startId = latestStartId
        if (startId > 0 && stopSelfResult(startId)) latestStartId = 0
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStartId = startId
        hostController.start()
        when (intent?.action) {
            ACTION_KEEPALIVE_WAKE -> keepAliveWake()
            ACTION_TASK_ACTIVITY -> daemonTaskActivity(intent)
            ACTION_AUTOMATION_WAKE -> automationWake(
                intent.getStringExtra(EXTRA_AUTOMATION_PHASE) ?: "automation_wake",
            )
            ACTION_MEDIA_PROJECTION_STOP -> {
                mediaProjection.stop()
                publishHint(PROJECTION_CONTEXT)
            }
            ACTION_MEDIA_PROJECTION_CONSENT -> {
                val resultData = intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
                if (resultData != null) {
                    mediaProjection.start(
                        intent.getIntExtra(EXTRA_RESULT_CODE, android.app.Activity.RESULT_CANCELED),
                        resultData,
                    )
                } else {
                    mediaProjection.publishInitialUnavailable()
                }
                publishHint(PROJECTION_CONTEXT)
            }
        }
        return if (foregroundReasons.hasPersistentReason()) {
            START_STICKY
        } else {
            stopStartedIfIdle()
            START_NOT_STICKY
        }
    }

    override fun onDestroy() {
        events.kill()
        // The listener never outlives its foreground keeper; the committed preference stays.
        mcpSettings.suspendListener()
        tunnelSettings.enabledObserver = null
        tunnelSettings.suspendRuntime(::setTunnelForeground)
        mediaProjection.stop()
        shizukuController.stop()
        (application as DroidBridgeApplication).requireRuntimeGraph().let { graph ->
            graph.setNetworkDefaultForegroundSink(null)
            graph.setTaskActivitySink(null)
        }
        scope.cancel()
        foregroundReasons.clear()
        hostController.setGuardScopeSink(null)
        hostController.setHintSink(null)
        super.onDestroy()
    }

    fun setForegroundReason(reason: ForegroundReason, active: Boolean) {
        foregroundReasons.set(reason, active)
    }

    private fun publishHint(projection: String) {
        val count = events.beginBroadcast()
        try {
            repeat(count) { index -> runCatching { events.getBroadcastItem(index).onEvent(projection) } }
        } finally {
            events.finishBroadcast()
        }
    }

    private fun verifyCaller() {
        if (Binder.getCallingUid() != applicationInfo.uid) throw SecurityException("caller UID rejected")
    }

    private fun requestId(envelope: ByteArray): String {
        val value = JSONObject(envelope.toString(Charsets.UTF_8)).getString("request_id")
        require(REQUEST_ID.matches(value))
        return value
    }

    /**
     * A caller of this binder surface is a UI client, so a start code outside the set it can act on
     * is reported as the unavailability it is rather than passed through.
     */
    private fun unavailableEnvelope(requestId: String, code: String): ByteArray =
        runtimeFailureEnvelope(
            requestId,
            code.takeIf(ALLOWED_START_ERRORS::contains)
                ?: DaemonErrorToken.CapabilityUnavailable.wire,
            null,
        )

    companion object {
        const val ACTION_MEDIA_PROJECTION_CONSENT = "com.droidbridge.android.action.MEDIA_PROJECTION_CONSENT"
        const val ACTION_MEDIA_PROJECTION_STOP = "com.droidbridge.android.action.MEDIA_PROJECTION_STOP"
        const val ACTION_UI_BIND = "com.droidbridge.android.action.UI_BIND"
        /** Sent by the root module's daemon (`app_keepalive.rs`); both sides spell it identically. */
        const val ACTION_KEEPALIVE_WAKE = "com.droidbridge.android.action.KEEPALIVE_WAKE"
        /** Sent by the root module's daemon (`app_keepalive.rs`); both sides spell it identically. */
        const val ACTION_TASK_ACTIVITY = "com.droidbridge.android.action.TASK_ACTIVITY"
        const val EXTRA_RUNTIME_EPOCH = "runtime_epoch"
        const val EXTRA_ACTIVE_TASKS = "active_tasks"
        const val EXTRA_CANONICAL_REVISION = "canonical_revision"
        const val ACTION_AUTOMATION_WAKE = "com.droidbridge.android.action.AUTOMATION_WAKE"
        const val EXTRA_AUTOMATION_PHASE = "automation_phase"
        private const val KEEPALIVE_WAKE_OWNER = "keepalive_wake"
        private const val MCP_FOREGROUND_OWNER = "mcp"
        private const val TUNNEL_FOREGROUND_OWNER = "tunnel"
        private const val TASK_FOREGROUND_OWNER = "task"
        private const val AUTOMATION_WAKE_OWNER = "automation_wake"
        private const val EXTRA_RESULT_CODE = "result_code"
        private const val EXTRA_RESULT_DATA = "result_data"
        private const val PROJECTION_CONTEXT = "context.status"
        private const val MAX_ENVELOPE_BYTES = 262_144
        private const val MAX_OUTSTANDING_REQUESTS = 64
        private val ALLOWED_START_ERRORS = setOf(
            DaemonErrorToken.CapabilityUnavailable.wire,
            DaemonErrorToken.HostTransitionPending.wire,
            DaemonErrorToken.IoError.wire,
            DaemonErrorToken.ProtocolIncompatible.wire,
            DaemonErrorToken.StaleAuthority.wire,
        )
        private val REQUEST_ID = Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
    }
}

enum class ForegroundReason {
    SpecialUse,
    Task,
    MediaProjection,
}

internal class ForegroundReasonRegistry(
    private val service: Service,
    private val taskCount: () -> Long = { 0 },
    private val onEmpty: () -> Unit = {},
) {
    /** Each type stays in the mask while any logical owner still needs it. */
    private val owners = linkedMapOf<ForegroundReason, MutableSet<String>>()
    private val active: Set<ForegroundReason> get() = owners.keys

    @Synchronized
    fun set(reason: ForegroundReason, enabled: Boolean, owner: String = reason.name) {
        val previous = owners.mapValues { (_, holders) -> holders.toSet() }
        if (enabled) {
            owners.getOrPut(reason, ::linkedSetOf) += owner
        } else {
            owners[reason]?.let { holders ->
                holders -= owner
                if (holders.isEmpty()) owners -= reason
            }
        }
        try {
            apply()
        } catch (error: RuntimeException) {
            owners.clear()
            previous.forEach { (held, holders) -> owners[held] = holders.toMutableSet() }
            throw error
        }
    }

    private fun apply() {
        if (active.isEmpty()) {
            service.stopForeground(Service.STOP_FOREGROUND_REMOVE)
            onEmpty()
            return
        }
        val manager = service.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(RUNTIME_CHANNEL, service.getString(R.string.cap_runtime_title), NotificationManager.IMPORTANCE_LOW),
        )
        manager.createNotificationChannel(
            NotificationChannel(TASK_CHANNEL, service.getString(R.string.nav_tasks), NotificationManager.IMPORTANCE_LOW),
        )
        val taskOnly = active == setOf(ForegroundReason.Task)
        val notification: Notification = NotificationCompat.Builder(
            service,
            if (taskOnly) TASK_CHANNEL else RUNTIME_CHANNEL,
        )
            .setSmallIcon(R.drawable.ic_stat_droidbridge)
            .setContentTitle(
                service.getString(
                    if (ForegroundReason.MediaProjection in active) {
                        R.string.notification_screen_capture_title
                    } else if (taskOnly) {
                        R.string.nav_tasks
                    } else {
                        R.string.cap_runtime_title
                    },
                ),
            )
            .setContentText(
                when {
                    ForegroundReason.MediaProjection in active ->
                        service.getString(R.string.notification_screen_capture_body)
                    taskOnly -> service.getString(
                        R.string.notification_tasks_body,
                        taskCount(),
                    )
                    else -> service.getString(R.string.mcp_state_running)
                },
            )
            .setOngoing(true)
            .apply {
                if (ForegroundReason.MediaProjection in active) {
                    addAction(
                        0,
                        service.getString(R.string.action_stop_capture),
                        PendingIntent.getService(
                            service,
                            0,
                            Intent(service, DroidBridgeService::class.java)
                                .setAction(DroidBridgeService.ACTION_MEDIA_PROJECTION_STOP),
                            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                        ),
                    )
                }
            }
            .build()
        if (android.os.Build.VERSION.SDK_INT >= 34) {
            service.startForeground(NOTIFICATION_ID, notification, typeMask())
        } else {
            @Suppress("DEPRECATION")
            service.startForeground(NOTIFICATION_ID, notification)
        }
    }

    @Synchronized
    fun hasPersistentReason(): Boolean = active.isNotEmpty()

    @Synchronized
    fun clear() {
        owners.clear()
        service.stopForeground(Service.STOP_FOREGROUND_REMOVE)
    }

    @androidx.annotation.RequiresApi(34)
    private fun typeMask(): Int = active.fold(0) { mask, reason ->
        mask or when (reason) {
            ForegroundReason.SpecialUse -> ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
            ForegroundReason.Task -> ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
            ForegroundReason.MediaProjection -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
        }
    }

    companion object {
        const val RUNTIME_CHANNEL = "droidbridge_runtime"
        const val TASK_CHANNEL = "droidbridge_tasks"
        private const val NOTIFICATION_ID = 1001
    }
}
