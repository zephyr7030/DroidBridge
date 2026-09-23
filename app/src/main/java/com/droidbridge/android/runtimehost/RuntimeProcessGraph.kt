package com.droidbridge.android.runtimehost

import android.app.Application
import android.net.ConnectivityManager
import android.net.Network
import com.droidbridge.android.BuildConfig
import com.droidbridge.android.execution.android.AndroidConnectivityAccess
import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidMotherToolAdapter
import com.droidbridge.android.execution.android.AndroidMotherToolPlatformAccess
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidContentResolverAccess
import com.droidbridge.android.execution.android.AndroidNetworkSnapshotAdapter
import com.droidbridge.android.execution.android.AndroidNetworkDefaultCallbackAccess
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.AppProcessExecutor
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.AndroidExactAlarmAccess
import com.droidbridge.android.execution.android.ContentResolverFilesystemAdapter
import com.droidbridge.android.execution.android.ExactAlarmAdapter
import com.droidbridge.android.execution.android.NativeAndroidExecutionDispatcher
import com.droidbridge.android.execution.android.NetworkDefaultCallbackAdapter
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.execution.android.VisualDisplayTracker
import com.droidbridge.android.execution.android.VisualFrameworkAdapter
import com.droidbridge.android.execution.android.VisualImageEncoder
import com.droidbridge.android.execution.android.VisualSceneActivity
import java.io.File
import java.util.concurrent.atomic.AtomicReference

internal class RuntimeProcessGraph(application: Application) {
    val hostController: RuntimeHostController
    val mcpSettings: McpSettingsController
    val tunnelSettings: TunnelSettingsController
    val androidExecutionRegistry: AndroidExecutionRegistry
    val visualDisplay: VisualDisplayTracker
    val visualEncoder: VisualImageEncoder
    val visualSceneActivity = VisualSceneActivity()
    private val shizukuPrimitives = AtomicReference<AndroidExecutionBridge?>(null)
    private val networkDefaultForeground = AtomicReference<((Boolean) -> Unit)?>(null)
    private val networkAttachment = AndroidProcessNetworkAttachment(
        application.getSystemService(ConnectivityManager::class.java),
    ) { hostController.scheduleNetworkAttachment() }

    init {
        System.loadLibrary("app_native")
        hostController = RuntimeHostController(application)
        check(networkAttachment.start()) { "Android default-network attachment failed" }
        hostController.setNetworkAttachmentSource(networkAttachment::boundHandle)
        McpHostBridge.install(hostController)
        val mcpPort = if (application.packageName.endsWith(DEBUG_PACKAGE_SUFFIX)) MCP_DEBUG_PORT else MCP_STABLE_PORT
        val settingsDirectory = File(application.createDeviceProtectedStorageContext().filesDir, "droidbridge")
        mcpSettings = McpSettingsController(
            settingsDirectory,
            NativeMcpListener(BuildConfig.VERSION_NAME),
            mcpPort,
            AndroidMcpSettingsFileSystem(),
        )
        tunnelSettings = TunnelSettingsController(
            settingsDirectory,
            NativeTunnelRuntime(mcpPort, BuildConfig.VERSION_NAME),
            AndroidTunnelNetworkMonitor(application.getSystemService(ConnectivityManager::class.java)),
            AndroidTunnelCredentialCipher(),
            AndroidMcpSettingsFileSystem(),
        )
        visualDisplay = VisualDisplayTracker(application)
        visualEncoder = VisualImageEncoder(application)
        androidExecutionRegistry = AndroidExecutionRegistry { key, state, reason, generation, hasExecutor ->
            if (key == ANDROID_FRAMEWORK_KEY) {
                state == RegisteredCapabilityState.Available.wireValue &&
                    reason.isEmpty() && hasExecutor && generation > 0
            } else {
                hostController.register(key, state, reason, generation, hasExecutor)
            }
        }
        val networkDefault = NetworkDefaultCallbackAdapter(
            AndroidNetworkDefaultCallbackAccess(
                application.getSystemService(ConnectivityManager::class.java),
            ),
            publishes = hostController::publishNetworkDefault,
            validatesFence = hostController::validatesFence,
            setsSpecialUse = { active ->
                val sink = networkDefaultForeground.get()
                if (sink == null && active) {
                    throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                }
                sink?.invoke(active)
            },
        )
        val exactAlarms = AndroidExactAlarmAccess(application, ExactAlarmReceiver::class.java)
        hostController.setApkProjectionReleasedSink(exactAlarms::cancel)
        val framework = RoutedAndroidExecution(
            ContentResolverFilesystemAdapter(
                AndroidContentResolverAccess(application.contentResolver),
                hostController::validatesFence,
            ),
            AppProcessExecutor(hostController::validatesFence),
            AndroidNetworkSnapshotAdapter(
                AndroidConnectivityAccess(application.getSystemService(ConnectivityManager::class.java)),
                hostController::validatesFence,
            ),
            networkDefault,
            ExactAlarmAdapter(exactAlarms, hostController::validatesFence),
            VisualFrameworkAdapter(visualDisplay, visualEncoder, hostController::validatesFence, visualSceneActivity),
            AndroidMotherToolAdapter(
                AndroidMotherToolPlatformAccess(application),
                hostController::validatesFence,
            ),
            shizukuPrimitives::get,
            { androidExecutionRegistry.executor(ACCESSIBILITY_KEY) },
        )
        NativeAndroidExecutionDispatcher.install(androidExecutionRegistry)
        hostController.setFrameworkReadySink { generation ->
            check(
                androidExecutionRegistry.register(
                    CapabilityRegistration(
                        key = ANDROID_FRAMEWORK_KEY,
                        state = RegisteredCapabilityState.Available,
                        reason = null,
                        sourceGeneration = generation,
                        executor = framework,
                        primitives = ROUTED_PRIMITIVES,
                    ),
                ),
            )
        }
        hostController.setCompanionDisconnectedSink(networkDefault::close)
    }

    /**
     * Supplies the session bridge the App surface routes its shell process primitives
     * through. Those primitives resolve at the live host generation like every other
     * App-local primitive, so the Shizuku session stays a primitive provider for the one
     * Command implementation instead of a registry entry with its own generation.
     */
    fun setShizukuPrimitiveBridge(bridge: AndroidExecutionBridge?) {
        shizukuPrimitives.set(bridge)
    }

    fun setNetworkDefaultForegroundSink(sink: ((Boolean) -> Unit)?) {
        networkDefaultForeground.set(sink)
    }

    fun setTaskActivitySink(sink: ((Long) -> Unit)?) {
        NativeAndroidExecutionDispatcher.installTaskActivitySink(sink)
    }

    private companion object {
        const val ANDROID_FRAMEWORK_KEY = "android.framework"
        const val ACCESSIBILITY_KEY = "visual.accessibility"
        const val DEBUG_PACKAGE_SUFFIX = ".debug"
        const val MCP_STABLE_PORT = 8765
        const val MCP_DEBUG_PORT = 18765
        val ROUTED_PRIMITIVES = setOf(
            AndroidPrimitive.ContentInspect,
            AndroidPrimitive.ContentOpenRead,
            AndroidPrimitive.AndroidNetworkSnapshot,
            AndroidPrimitive.NetworkDefaultSubscribe,
            AndroidPrimitive.NetworkDefaultUnsubscribe,
            AndroidPrimitive.AlarmSchedule,
            AndroidPrimitive.AlarmCancel,
            AndroidPrimitive.VisualDisplaySnapshot,
            AndroidPrimitive.AccessibilityObserve,
            AndroidPrimitive.AccessibilityNodeAction,
            AndroidPrimitive.AccessibilityGesture,
            AndroidPrimitive.AccessibilityText,
            AndroidPrimitive.VisualImageTransform,
            AndroidPrimitive.PackageInspect,
            AndroidPrimitive.LaunchActivity,
            AndroidPrimitive.IntentStart,
            AndroidPrimitive.ClipboardRead,
            AndroidPrimitive.ClipboardWrite,
            AndroidPrimitive.ClipboardClear,
            AndroidPrimitive.AppProcessStart,
            AndroidPrimitive.AppProcessCancel,
            AndroidPrimitive.ShizukuProcessStart,
            AndroidPrimitive.ShizukuProcessCancel,
        )
    }
}

/**
 * Routes each App-local primitive to the adapter that owns it. The Command adapters only
 * carry an already admitted request, so this decides which of the surface's own
 * primitives serves it and never substitutes one identity's runner for another's.
 */
private class RoutedAndroidExecution(
    private val framework: AndroidExecutionBridge,
    private val app: AndroidExecutionBridge,
    private val network: AndroidExecutionBridge,
    private val networkDefault: AndroidExecutionBridge,
    private val exactAlarm: AndroidExecutionBridge,
    private val visualFramework: AndroidExecutionBridge,
    private val motherTool: AndroidExecutionBridge,
    private val shizuku: () -> AndroidExecutionBridge?,
    private val accessibility: () -> AndroidExecutionBridge?,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult =
        when (request.primitive) {
            AndroidPrimitive.ContentInspect,
            AndroidPrimitive.ContentOpenRead,
            -> framework.execute(request)
            AndroidPrimitive.AndroidNetworkSnapshot,
            -> network.execute(request)
            AndroidPrimitive.NetworkDefaultSubscribe,
            AndroidPrimitive.NetworkDefaultUnsubscribe,
            -> networkDefault.execute(request)
            AndroidPrimitive.AlarmSchedule,
            AndroidPrimitive.AlarmCancel,
            -> exactAlarm.execute(request)
            AndroidPrimitive.VisualDisplaySnapshot,
            AndroidPrimitive.VisualImageTransform,
            -> visualFramework.execute(request)
            // The accessibility service registers its own executor when the user enables it, so the
            // observation reaches the component that owns the node handles rather than the
            // framework adapter that only reads the display.
            AndroidPrimitive.AccessibilityObserve,
            AndroidPrimitive.AccessibilityNodeAction,
            AndroidPrimitive.AccessibilityGesture,
            AndroidPrimitive.AccessibilityText,
            -> (accessibility() ?: throw AndroidExecutionException(CAPABILITY_UNAVAILABLE))
                .execute(request)
            AndroidPrimitive.PackageInspect,
            AndroidPrimitive.LaunchActivity,
            AndroidPrimitive.IntentStart,
            AndroidPrimitive.ClipboardRead,
            AndroidPrimitive.ClipboardWrite,
            AndroidPrimitive.ClipboardClear,
            -> motherTool.execute(request)
            AndroidPrimitive.AppProcessStart,
            AndroidPrimitive.AppProcessCancel,
            -> app.execute(request)
            AndroidPrimitive.ShizukuProcessStart,
            AndroidPrimitive.ShizukuProcessCancel,
            -> (shizuku() ?: throw AndroidExecutionException(CAPABILITY_UNAVAILABLE))
                .execute(request)
            else -> throw AndroidExecutionException(UNSUPPORTED)
        }

    private companion object {
        const val CAPABILITY_UNAVAILABLE = "CAPABILITY_UNAVAILABLE"
        const val UNSUPPORTED = "UNSUPPORTED"
    }
}

/**
 * Keeps the whole runtime process on the system default network. Sockets created without an
 * explicit selection do not necessarily follow that network, and the tunnel client must reach
 * OpenAI through the network the device actually uses, including when the user's VPN supplies
 * it. Independent of the tunnel lifecycle because credential validation runs before any tunnel
 * is enabled.
 *
 * The binding is also the fact the Magisk host follows: this process is the device's own client,
 * so the network it is attached to is the one the host attaches to as well, and every later
 * change here is reported. The binding this class starts with is not a change: the host reads it
 * when it becomes the host.
 */
private class AndroidProcessNetworkAttachment(
    private val connectivity: ConnectivityManager,
    private val report: () -> Unit,
) {
    private val lock = Any()
    private var registered: ConnectivityManager.NetworkCallback? = null
    private var bound: Network? = null

    /** The handle of the network this process is bound to, or null when it has none. */
    fun boundHandle(): String? = synchronized(lock) { bound?.networkHandle?.toString() }

    fun start(): Boolean = synchronized(lock) {
        if (registered != null) return true
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                if (attach(network)) report()
            }

            override fun onLost(network: Network) {
                if (attach(connectivity.activeNetwork)) report()
            }
        }
        runCatching { connectivity.registerDefaultNetworkCallback(callback) }
            .onSuccess {
                registered = callback
                attach(connectivity.activeNetwork)
            }
            .isSuccess
    }

    /**
     * Binds this process to `network` and answers whether that replaced the binding. The binding
     * is recorded only after the platform accepted it, so a fact that repeats costs no call.
     */
    private fun attach(network: Network?): Boolean = synchronized(lock) {
        if (bound == network) return false
        connectivity.bindProcessToNetwork(network)
        bound = network
        true
    }
}
