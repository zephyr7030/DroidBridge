package com.droidbridge.android.client

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.SystemClock
import com.droidbridge.android.runtimehost.IDroidBridgeRuntime
import com.droidbridge.android.runtimehost.IRuntimeCallback
import com.droidbridge.android.runtimehost.IRuntimeEventCallback
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.cancellation.CancellationException
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import org.json.JSONObject

class DroidBridgeClient(
    context: Context,
) {
    private val applicationContext = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val tokenCounter = AtomicLong(0)
    private val refresh = RefreshCoordinator()
    private val pendingRequests = ConcurrentHashMap<String, CancellableContinuation<ByteArray>>()
    private val mutableState = MutableStateFlow<ClientState>(ClientState.Disconnected)
    private var runtime: IDroidBridgeRuntime? = null
    private var bound = false
    private var connection: ServiceConnection? = null
    private var subscription: IRuntimeEventCallback? = null

    val state: StateFlow<ClientState> = mutableState.asStateFlow()

    @Synchronized
    fun bind() {
        if (bound) return
        mutableState.value = ClientState.Connecting
        val token = tokenCounter.incrementAndGet()
        val next = connection(token)
        connection = next
        bound = applicationContext.bindService(
            runtimeServiceIntent().setAction(ACTION_UI_BIND),
            next,
            Context.BIND_AUTO_CREATE,
        )
        if (!bound) mutableState.value = ClientState.Unavailable("RUNTIME_UNAVAILABLE")
    }

    @Synchronized
    fun unbind() {
        val subscribedRuntime = runtime
        val subscribedCallback = subscription
        if (subscribedRuntime != null && subscribedCallback != null) {
            runCatching { subscribedRuntime.unsubscribe(subscribedCallback) }
        }
        if (bound) connection?.let { applicationContext.unbindService(it) }
        bound = false
        connection = null
        runtime = null
        subscription = null
        tokenCounter.incrementAndGet()
        cancelPendingCallbacks()
        refresh.disconnected()
        mutableState.value = ClientState.Disconnected
    }

    fun recheck() {
        runtime?.let { service ->
            runCatching { service.requestCapabilityRecheck() }
            requestRefresh(tokenCounter.get())
        } ?: bind()
    }

    fun requestShizukuAuthorization() {
        runtime?.let { service ->
            runCatching { service.requestShizukuAuthorization() }
            requestRefresh(tokenCounter.get())
        } ?: bind()
    }

    fun deliverMediaProjectionConsent(resultCode: Int, resultData: Intent) {
        applicationContext.startService(
            runtimeServiceIntent()
                .setAction(ACTION_MEDIA_PROJECTION_CONSENT)
                .putExtra(EXTRA_RESULT_CODE, resultCode)
                .putExtra(EXTRA_RESULT_DATA, resultData),
        )
    }

    fun stopMediaProjection() {
        applicationContext.startService(runtimeServiceIntent().setAction(ACTION_MEDIA_PROJECTION_STOP))
    }

    suspend fun submit(envelope: ByteArray): ByteArray {
        val service = runtime ?: error("Runtime unavailable")
        val token = tokenCounter.get()
        val requestId = JSONObject(envelope.toString(Charsets.UTF_8)).getString("request_id")
        return suspendCancellableCoroutine { continuation ->
            check(pendingRequests.putIfAbsent(requestId, continuation) == null)
            val callback = object : IRuntimeCallback.Stub() {
                override fun onResponse(response: ByteArray?) {
                    val current = pendingRequests.remove(requestId) ?: return
                    if (response != null && token == tokenCounter.get() && current.isActive) {
                        current.resume(response)
                    } else {
                        current.cancel(CancellationException("Runtime callback became stale"))
                    }
                }
            }
            continuation.invokeOnCancellation {
                if (pendingRequests.remove(requestId, continuation)) {
                    runCatching { service.cancelRequest(requestId) }
                }
            }
            runCatching { service.submit(envelope, callback) }
                .onFailure { error ->
                    if (pendingRequests.remove(requestId, continuation)) {
                        continuation.resumeWith(Result.failure(error))
                    }
                }
        }
    }

    /** The S-MCP-003 settings reply `{schema_version,enabled,listener,...}` or `{error}`. */
    suspend fun mcpSettings(): String = mcpCall(IDroidBridgeRuntime::getMcpSettings)

    /** Returns only after the listener is live or has failed (S-MCP-004). */
    suspend fun setMcpEnabled(enabled: Boolean): String = mcpCall { it.setMcpEnabled(enabled) }

    suspend fun rotateMcpToken(): String = mcpCall(IDroidBridgeRuntime::rotateMcpToken)

    suspend fun revealMcpToken(): String = mcpCall(IDroidBridgeRuntime::revealMcpToken)

    suspend fun tunnelSettings(): String = mcpCall(IDroidBridgeRuntime::getTunnelSettings)

    suspend fun configureTunnel(tunnelId: String, apiKey: String): String =
        mcpCall { it.configureTunnel(tunnelId, apiKey) }

    suspend fun setTunnelEnabled(enabled: Boolean): String = mcpCall { it.setTunnelEnabled(enabled) }

    suspend fun clearTunnel(): String = mcpCall(IDroidBridgeRuntime::clearTunnel)

    /** The S-UI-017 `{schema_version,blocker,cleanup}` reply or `{error}`. */
    suspend fun maintenanceState(): String = mcpCall(IDroidBridgeRuntime::getMaintenanceState)

    /** The S-UI-017 live diagnostics snapshot; the Service bounds it to 2000 ms. */
    suspend fun diagnosticsSnapshot(): String = mcpCall(IDroidBridgeRuntime::getDiagnosticsSnapshot)

    /** Returns only after the fresh APK Runtime instance is active or the reset failed. */
    suspend fun resetRuntimeData(): String = mcpCall(IDroidBridgeRuntime::resetRuntimeData)

    suspend fun strandedExecutions(): Int {
        val service = runtime ?: error("Runtime unavailable")
        return withContext(Dispatchers.IO) { service.strandedExecutions }
    }

    suspend fun clearStrandedExecutions(): String = mcpCall(IDroidBridgeRuntime::clearStrandedExecutions)

    suspend fun resetRuntimeHostToApk(): String = mcpCall(IDroidBridgeRuntime::resetRuntimeHostToApk)

    /** The S-UPD-002 `{schema_version,configured,module,privileged_install,installed_version_code,record}` reply. */
    suspend fun updateMaintenance(): String = mcpCall(IDroidBridgeRuntime::getUpdateMaintenance)

    suspend fun beginProductUpdate(manifest: ByteArray, signature: ByteArray): String =
        mcpCall { it.beginProductUpdate(manifest, signature) }

    suspend fun beginModuleRepair(manifest: ByteArray, signature: ByteArray): String =
        mcpCall { it.beginModuleRepair(manifest, signature) }

    suspend fun installUpdateApk(updateId: String): String = mcpCall { it.installUpdateApk(updateId) }

    suspend fun installUpdateModule(updateId: String): String = mcpCall { it.installUpdateModule(updateId) }

    suspend fun cancelUpdate(updateId: String): String = mcpCall { it.cancelUpdate(updateId) }

    suspend fun continueWithoutModule(updateId: String): String = mcpCall { it.continueWithoutModule(updateId) }

    private suspend fun mcpCall(call: (IDroidBridgeRuntime) -> String): String {
        val service = runtime ?: error("Runtime unavailable")
        return withContext(Dispatchers.IO) { call(service) }
    }

    fun close() {
        unbind()
        scope.cancel()
    }

    private fun connection(token: Long) = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            if (token != tokenCounter.get()) return
            runtime = IDroidBridgeRuntime.Stub.asInterface(binder)
            val eventCallback = object : IRuntimeEventCallback.Stub() {
                override fun onEvent(projection: String?) {
                    if (token == tokenCounter.get() && projection == CONTEXT_PROJECTION) {
                        requestRefresh(token)
                    }
                }
            }
            subscription = eventCallback
            // A Runtime that dies between binding and this call is reported by the disconnect
            // callback that follows; failing here would take the UI process down with it.
            if (runCatching { runtime?.subscribe(eventCallback) }.isFailure) return
            requestRefresh(token)
        }

        override fun onServiceDisconnected(name: ComponentName) = temporarilyDisconnected(token)
        override fun onBindingDied(name: ComponentName) = permanentlyDisconnected(token)
        override fun onNullBinding(name: ComponentName) = permanentlyDisconnected(token)
    }

    private fun temporarilyDisconnected(token: Long) {
        if (token != tokenCounter.get()) return
        runtime = null
        subscription = null
        cancelPendingCallbacks()
        refresh.disconnected()
        mutableState.value = ClientState.Unavailable("RUNTIME_UNAVAILABLE")
    }

    @Synchronized
    private fun permanentlyDisconnected(token: Long) {
        if (token != tokenCounter.get()) return
        if (bound) connection?.let { current -> runCatching { applicationContext.unbindService(current) } }
        bound = false
        connection = null
        runtime = null
        subscription = null
        tokenCounter.incrementAndGet()
        cancelPendingCallbacks()
        refresh.disconnected()
        mutableState.value = ClientState.Unavailable("RUNTIME_UNAVAILABLE")
    }

    private fun requestRefresh(token: Long) {
        val delayMs = refresh.hint(SystemClock.elapsedRealtime()) ?: return
        scope.launch { refreshLoop(token, delayMs) }
    }

    private suspend fun refreshLoop(token: Long, initialDelayMs: Long) {
        var delayMs: Long? = initialDelayMs
        while (delayMs != null && token == tokenCounter.get()) {
            delay(delayMs)
            refresh.started(SystemClock.elapsedRealtime())
            val prior = mutableState.value as? ClientState.Available
            if (prior != null) mutableState.value = prior.copy(refreshing = true)
            val result = runCatching { submit(contextStatusEnvelope()) }
            if (token == tokenCounter.get()) {
                mutableState.value = resolveContextRefresh(result.getOrNull())
            }
            delayMs = refresh.finished(SystemClock.elapsedRealtime())
        }
    }

    private fun contextStatusEnvelope(): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject()
                .put("tool", "context")
                .put("action", "status")
                .put("input", JSONObject().put("detail", "full")),
        )
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun runtimeServiceIntent(): Intent = Intent().setComponent(
        ComponentName(applicationContext.packageName, RUNTIME_SERVICE_CLASS),
    )

    private fun cancelPendingCallbacks() {
        val callbacks = pendingRequests.entries.toList()
        callbacks.forEach { (requestId, continuation) ->
            if (pendingRequests.remove(requestId, continuation)) {
                continuation.cancel(CancellationException("Runtime disconnected"))
            }
        }
    }

    companion object {
        private const val CONTEXT_PROJECTION = "context.status"
        private const val RUNTIME_SERVICE_CLASS =
            "com.droidbridge.android.runtimehost.DroidBridgeService"
        private const val ACTION_UI_BIND = "com.droidbridge.android.action.UI_BIND"
        private const val ACTION_MEDIA_PROJECTION_CONSENT =
            "com.droidbridge.android.action.MEDIA_PROJECTION_CONSENT"
        private const val ACTION_MEDIA_PROJECTION_STOP =
            "com.droidbridge.android.action.MEDIA_PROJECTION_STOP"
        private const val EXTRA_RESULT_CODE = "result_code"
        private const val EXTRA_RESULT_DATA = "result_data"
    }
}

internal fun resolveContextRefresh(
    response: ByteArray?,
    decode: (ByteArray) -> RuntimeSnapshot = RuntimeSnapshot::fromResponse,
    failureReason: (ByteArray) -> String? = RuntimeSnapshot::failureReason,
): ClientState = response?.let { envelope ->
    runCatching { decode(envelope) }
        .fold(
            onSuccess = { ClientState.Available(it) },
            onFailure = {
                ClientState.Unavailable(
                    failureReason(envelope) ?: "PROTOCOL_MISMATCH",
                )
            },
        )
} ?: ClientState.Unavailable("RUNTIME_UNAVAILABLE")
