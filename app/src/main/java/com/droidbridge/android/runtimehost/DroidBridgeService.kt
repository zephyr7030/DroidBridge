package com.droidbridge.android.runtimehost

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Binder
import android.os.IBinder
import android.os.RemoteCallbackList
import androidx.core.app.NotificationCompat
import com.droidbridge.android.DroidBridgeApplication
import com.droidbridge.android.R
import com.droidbridge.android.execution.shizuku.ShizukuController
import com.droidbridge.android.execution.android.AccessibilityServiceStartupFact
import com.droidbridge.android.execution.android.MediaProjectionVisualController
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
        foregroundReasons = ForegroundReasonRegistry(this)
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
     * Only the default-process client bind restarts an enabled listener (S-MCP-004); broadcast
     * keeper starts and NotificationListener binds never do, so boot alone never starts MCP.
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
     * The Magisk module starts this service as a foreground service when an enabled connection
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
        foregroundReasons.set(ForegroundReason.SpecialUse, false, KEEPALIVE_WAKE_OWNER)
    }

    private fun setTunnelForeground(active: Boolean) {
        if (active) startService(Intent(this, DroidBridgeService::class.java))
        foregroundReasons.set(ForegroundReason.SpecialUse, active, TUNNEL_FOREGROUND_OWNER)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        hostController.start()
        when (intent?.action) {
            ACTION_KEEPALIVE_WAKE -> keepAliveWake()
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
        return if (foregroundReasons.hasPersistentReason()) START_STICKY else START_NOT_STICKY
    }

    override fun onDestroy() {
        events.kill()
        // The listener never outlives its foreground keeper; the committed preference stays.
        mcpSettings.suspendListener()
        tunnelSettings.enabledObserver = null
        tunnelSettings.suspendRuntime(::setTunnelForeground)
        mediaProjection.stop()
        shizukuController.stop()
        (application as DroidBridgeApplication).requireRuntimeGraph()
            .setNetworkDefaultForegroundSink(null)
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
        /** Sent by the Magisk module's daemon (`app_keepalive.rs`); both sides spell it identically. */
        const val ACTION_KEEPALIVE_WAKE = "com.droidbridge.android.action.KEEPALIVE_WAKE"
        private const val KEEPALIVE_WAKE_OWNER = "keepalive_wake"
        private const val MCP_FOREGROUND_OWNER = "mcp"
        private const val TUNNEL_FOREGROUND_OWNER = "tunnel"
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
    MediaProjection,
}

internal class ForegroundReasonRegistry(
    private val service: Service,
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
            return
        }
        val manager = service.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(RUNTIME_CHANNEL, service.getString(R.string.cap_runtime_title), NotificationManager.IMPORTANCE_LOW),
        )
        manager.createNotificationChannel(
            NotificationChannel(TASK_CHANNEL, service.getString(R.string.nav_tasks), NotificationManager.IMPORTANCE_LOW),
        )
        val notification: Notification = NotificationCompat.Builder(service, RUNTIME_CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_droidbridge)
            .setContentTitle(
                service.getString(
                    if (ForegroundReason.MediaProjection in active) {
                        R.string.notification_screen_capture_title
                    } else {
                        R.string.cap_runtime_title
                    },
                ),
            )
            .setContentText(
                service.getString(
                    if (ForegroundReason.MediaProjection in active) {
                        R.string.notification_screen_capture_body
                    } else {
                        R.string.mcp_state_running
                    },
                ),
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
            ForegroundReason.MediaProjection -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
        }
    }

    companion object {
        const val RUNTIME_CHANNEL = "droidbridge_runtime"
        const val TASK_CHANNEL = "droidbridge_tasks"
        private const val NOTIFICATION_ID = 1001
    }
}
