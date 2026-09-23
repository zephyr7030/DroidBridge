@file:Suppress("DEPRECATION")

package com.droidbridge.android.execution.android

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.ComponentName
import android.content.Context
import android.provider.Settings
import android.graphics.Bitmap
import android.graphics.Path
import android.graphics.Rect
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import com.droidbridge.android.DroidBridgeApplication
import java.security.MessageDigest
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/**
 * The Accessibility fact before the DroidBridge service connects in this process. A service the
 * user has not enabled is a known unavailability rather than an unknown adapter, and the
 * component's own connection registrations always supersede it with a larger source generation.
 */
internal object AccessibilityServiceStartupFact {
    const val DISABLED_REASON = "SERVICE_DISABLED"
    const val GENERATION = 1L

    fun registration(enabled: Boolean): CapabilityRegistration? =
        if (enabled) {
            null
        } else {
            CapabilityRegistration(
                key = "visual.accessibility",
                state = RegisteredCapabilityState.Unavailable,
                reason = DISABLED_REASON,
                sourceGeneration = GENERATION,
            )
        }

    /**
     * Whether the user enabled the service. The enabled-services setting is read directly because
     * the platform's enabled-service list only reports services that are already bound.
     */
    fun isEnabled(context: Context): Boolean {
        val enabled = Settings.Secure.getString(
            context.contentResolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
        ) ?: return false
        val own = ComponentName(context, DroidBridgeAccessibilityService::class.java)
        return enabled.split(':').any { entry -> ComponentName.unflattenFromString(entry) == own }
    }
}

class DroidBridgeAccessibilityService : AccessibilityService() {
    private val componentGeneration = AtomicLong(SystemClock.elapsedRealtimeNanos().coerceAtLeast(1))
    private val sceneRevision = AtomicLong(1)
    private val scenes = AccessibilitySceneStore<AccessibilityNodeInfo>(
        capacity = 32,
        ttlMillis = 300_000,
        clockMillis = SystemClock::elapsedRealtime,
        release = { node -> runCatching { node.recycle() } },
    )
    private var registered = false
    private val sceneExpiryHandler = Handler(Looper.getMainLooper())
    private val sceneExpiryRunnable = Runnable { scheduleSceneExpiry() }

    override fun onServiceConnected() {
        super.onServiceConnected()
        invalidateScenes()
        val generation = componentGeneration.incrementAndGet()
        val graph = graph()
        check(
            graph.androidExecutionRegistry.register(
                CapabilityRegistration(
                    key = ACCESSIBILITY_KEY,
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = generation,
                    executor = AccessibilityVisualAdapter(
                        service = this,
                        componentGeneration = generation,
                        sceneRevision = sceneRevision,
                        scenes = scenes,
                        display = graph.visualDisplay,
                        encoder = graph.visualEncoder,
                        validatesFence = graph.hostController::validatesFence,
                        onSceneStored = ::scheduleSceneExpiry,
                    ),
                    primitives = setOf(
                        AndroidPrimitive.AccessibilityObserve,
                        AndroidPrimitive.AccessibilityNodeAction,
                        AndroidPrimitive.AccessibilityGesture,
                        AndroidPrimitive.AccessibilityText,
                    ),
                ),
            ),
        )
        registered = true
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        if (event?.eventType !in INVALIDATING_EVENTS) return
        invalidateScenes()
        graph().visualSceneActivity.changed()
    }

    override fun onInterrupt() {
        invalidateScenes()
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        disconnect("SERVICE_DISCONNECTED")
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        disconnect("SERVICE_DISCONNECTED")
        super.onDestroy()
    }

    private fun invalidateScenes() {
        sceneExpiryHandler.removeCallbacks(sceneExpiryRunnable)
        sceneRevision.updateAndGet { current -> if (current == Long.MAX_VALUE) 1 else current + 1 }
        scenes.clear()
    }

    private fun scheduleSceneExpiry() {
        sceneExpiryHandler.removeCallbacks(sceneExpiryRunnable)
        scenes.expireAndNextDelayMillis()?.let { delay ->
            sceneExpiryHandler.postDelayed(sceneExpiryRunnable, delay)
        }
    }

    private fun disconnect(reason: String) {
        invalidateScenes()
        val generation = componentGeneration.incrementAndGet()
        if (registered) {
            graph().androidExecutionRegistry.register(
                CapabilityRegistration(
                    key = ACCESSIBILITY_KEY,
                    state = RegisteredCapabilityState.Unavailable,
                    reason = reason,
                    sourceGeneration = generation,
                ),
            )
        }
        registered = false
    }

    private fun graph() = (application as DroidBridgeApplication).requireRuntimeGraph()

    private companion object {
        const val ACCESSIBILITY_KEY = "visual.accessibility"
        val INVALIDATING_EVENTS = setOf(
            AccessibilityEvent.TYPE_WINDOWS_CHANGED,
            AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED,
            AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED,
            AccessibilityEvent.TYPE_VIEW_SCROLLED,
        )
    }
}

private class AccessibilityVisualAdapter(
    private val service: AccessibilityService,
    private val componentGeneration: Long,
    private val sceneRevision: AtomicLong,
    private val scenes: AccessibilitySceneStore<AccessibilityNodeInfo>,
    private val display: VisualDisplayTracker,
    private val encoder: VisualImageEncoder,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val onSceneStored: () -> Unit,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.payload.size > 1_048_576 || request.descriptors.isNotEmpty()) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        return when (request.primitive) {
            AndroidPrimitive.AccessibilityObserve -> observe(request)
            AndroidPrimitive.AccessibilityNodeAction -> nodeAction(request, textAction = false)
            AndroidPrimitive.AccessibilityText -> text(request)
            AndroidPrimitive.AccessibilityGesture -> gesture(request)
            else -> throw AndroidExecutionException("UNSUPPORTED")
        }
    }

    private suspend fun observe(request: AndroidExecutionRequest): AndroidExecutionResult {
        val input = request.payloadObject()
        return when (input["operation"]?.jsonPrimitive?.contentOrNull) {
            "hierarchy" -> hierarchy(input)
            "screenshot" -> screenshot(input)
            else -> throw AndroidExecutionException("INVALID_ARGUMENT")
        }
    }

    private suspend fun hierarchy(input: JsonObject): AndroidExecutionResult =
        withTimeout(HIERARCHY_TIMEOUT_MS) {
            withContext(Dispatchers.Main.immediate) {
                if (input.keys != HIERARCHY_KEYS) throw AndroidExecutionException("INVALID_ARGUMENT")
                val observationId = input.requiredString("observation_id")
                val maxNodes = input.requiredInt("max_nodes").takeIf { it in 1..5_000 }
                    ?: throw AndroidExecutionException("INVALID_ARGUMENT")
                val admitted = input.expectedDisplay()
                val current = display.snapshot()
                if (admitted != current) throw AndroidExecutionException("STALE_AUTHORITY")
                val root = activeRoot()
                    ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                val hierarchy = collectHierarchy(
                    root,
                    SystemClock.elapsedRealtime() + HIERARCHY_TIMEOUT_MS,
                )
                var handlesTransferred = false
                try {
                    val revision = sceneRevision.get()
                    if (display.snapshot() != current) {
                        throw AndroidExecutionException("STALE_AUTHORITY")
                    }
                    val proof = AccessibilitySceneProof(
                        componentGeneration = componentGeneration,
                        windowId = hierarchy.windowId,
                        sceneRevision = revision,
                        display = current,
                        hierarchySha256 = hierarchy.sha256,
                    )
                    val selected = hierarchy.nodes.take(maxNodes)
                    val handles = linkedMapOf<String, AccessibilityNodeInfo>()
                    val publicNodes = buildJsonArray {
                        selected.forEachIndexed { index, node ->
                            val nodeRef = "$observationId/$index"
                            handles[nodeRef] = node.handle
                            add(node.publicJson(nodeRef))
                        }
                    }
                    scenes.put(observationId, proof, handles)
                    handlesTransferred = true
                    hierarchy.nodes.drop(maxNodes).forEach { node ->
                        runCatching { node.handle.recycle() }
                    }
                    onSceneStored()
                    AndroidExecutionResult(
                        buildJsonObject {
                            put("display", current.geometryJson())
                            put("display_generation", current.generation)
                            root.packageName?.toString()?.boundedUtf8()?.let { packageName ->
                                put("foreground", buildJsonObject { put("package", packageName) })
                            }
                            put("nodes", publicNodes)
                            put("truncated", hierarchy.truncated || hierarchy.nodes.size > maxNodes)
                            put("component_generation", componentGeneration)
                            put("window_id", hierarchy.windowId)
                            put("scene_revision", revision)
                            put("hierarchy_sha256", hierarchy.sha256)
                        }.toString().encodeToByteArray(),
                    )
                } finally {
                    if (!handlesTransferred) {
                        hierarchy.nodes.forEach { node -> runCatching { node.handle.recycle() } }
                    }
                }
            }
        }

    private suspend fun screenshot(input: JsonObject): AndroidExecutionResult {
        if (input.keys != SCREENSHOT_KEYS) throw AndroidExecutionException("INVALID_ARGUMENT")
        val admitted = input.expectedDisplay()
        val before = display.snapshot()
        if (admitted != before) throw AndroidExecutionException("STALE_AUTHORITY")
        val screenshot = withTimeout(SCREENSHOT_TIMEOUT_MS) { takeScreenshot() }
        val buffer = screenshot.hardwareBuffer
        val wrapped = Bitmap.wrapHardwareBuffer(buffer, screenshot.colorSpace)
        if (wrapped == null) {
            buffer.close()
            throw AndroidExecutionException("IO_ERROR")
        }
        val software = try {
            runCatching { wrapped.copy(Bitmap.Config.ARGB_8888, false) }.getOrNull()
        } finally {
            wrapped.recycle()
        }
        if (software == null) {
            buffer.close()
            throw AndroidExecutionException("IO_ERROR")
        }
        return try {
            val captured = display.snapshot()
            if (captured != admitted || software.width != admitted.width || software.height != admitted.height) {
                throw AndroidExecutionException("STALE_AUTHORITY")
            }
            val encoded = encoder.encodeAuto(software)
            try {
                val completed = display.snapshot()
                if (completed != admitted || encoded.width != admitted.width || encoded.height != admitted.height) {
                    throw AndroidExecutionException("STALE_AUTHORITY")
                }
                encoded.transfer(admitted)
            } catch (error: Throwable) {
                encoded.discard()
                throw error
            }
        } finally {
            software.recycle()
            buffer.close()
        }
    }

    private suspend fun nodeAction(
        request: AndroidExecutionRequest,
        textAction: Boolean,
    ): AndroidExecutionResult {
        val deadline = SystemClock.elapsedRealtime() + ACTION_TIMEOUT_MS
        return withTimeout(ACTION_TIMEOUT_MS) {
            withContext(Dispatchers.Main.immediate) {
            val input = request.payloadObject()
            if (input.keys != NODE_ACTION_KEYS) {
                throw AndroidExecutionException("INVALID_ARGUMENT")
            }
            val operation = input.requiredString("operation")
            val nodeRef = input.requiredString("node_ref")
            val observationId = input.requiredString("observation_id")
            val proof = input.sceneProof()
            val handle = scenes.exact(observationId, nodeRef, proof)
                ?: throw AndroidExecutionException("STALE_REFERENCE")
            if (!validateScene(handle, proof, deadline)) {
                throw AndroidExecutionException("STALE_REFERENCE")
            }
            val delivered = when {
                textAction || operation == "text" -> {
                    if (!handle.isEditable) throw AndroidExecutionException("UNSUPPORTED")
                    val text = input.requiredString("text")
                    handle.performAction(
                        AccessibilityNodeInfo.ACTION_SET_TEXT,
                        Bundle().apply {
                            putCharSequence(
                                AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                                text,
                            )
                        },
                    )
                }
                operation == "tap" -> handle.performAction(AccessibilityNodeInfo.ACTION_CLICK)
                operation == "long_press" -> handle.performAction(AccessibilityNodeInfo.ACTION_LONG_CLICK)
                else -> throw AndroidExecutionException("INVALID_ARGUMENT")
            }
            if (!delivered) throw AndroidExecutionException("IO_ERROR")
            deliveredResult()
        }
    }
    }

    private suspend fun text(request: AndroidExecutionRequest): AndroidExecutionResult {
        val input = request.payloadObject()
        return if (input["operation"]?.jsonPrimitive?.contentOrNull == "focused") {
            if (input.keys != setOf("operation", "text")) {
                throw AndroidExecutionException("INVALID_ARGUMENT")
            }
            withTimeout(ACTION_TIMEOUT_MS) {
                withContext(Dispatchers.Main.immediate) {
                    val root = activeRoot()
                        ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                    try {
                        val focused = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
                            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                        try {
                            val delivered = focused.performAction(
                                AccessibilityNodeInfo.ACTION_SET_TEXT,
                                Bundle().apply {
                                    putCharSequence(
                                        AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                                        input.requiredString("text"),
                                    )
                                },
                            )
                            if (!delivered) throw AndroidExecutionException("IO_ERROR")
                            deliveredResult()
                        } finally {
                            focused.recycle()
                        }
                    } finally {
                        root.recycle()
                    }
                }
            }
        } else {
            nodeAction(request, textAction = true)
        }
    }

    private suspend fun gesture(request: AndroidExecutionRequest): AndroidExecutionResult {
        val input = request.payloadObject()
        if (input.keys != GESTURE_KEYS) throw AndroidExecutionException("INVALID_ARGUMENT")
        val admitted = input.expectedDisplay()
        val observationId = input.requiredString("observation_id")
        val proof = input.sceneProof()
        val operation = input.requiredString("operation")
        val fromX = input.requiredInt("from_x")
        val fromY = input.requiredInt("from_y")
        val toX = input["to_x"]?.jsonPrimitive?.intOrNull
        val toY = input["to_y"]?.jsonPrimitive?.intOrNull
        val requestedDuration = input["duration_ms"]?.jsonPrimitive?.longOrNull
        requirePoint(admitted, fromX, fromY)
        val duration = when (operation) {
            "tap" -> 1L
            "long_press" -> 500L
            "swipe" -> requestedDuration?.takeIf { it in 1..10_000 }
                ?: throw AndroidExecutionException("INVALID_ARGUMENT")
            else -> throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        if (operation == "swipe") {
            requirePoint(
                admitted,
                toX ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
                toY ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
            )
        } else if (toX != null || toY != null || requestedDuration != null) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val timeout = minOf(duration + 5_000, 15_000)
        return withTimeout(timeout) {
            withContext(Dispatchers.Main.immediate) {
                val deadline = SystemClock.elapsedRealtime() + ACTION_TIMEOUT_MS
                if (display.snapshot() != admitted ||
                    !scenes.matches(observationId, proof) ||
                    !validateScene(null, proof, deadline)
                ) {
                    throw AndroidExecutionException("STALE_REFERENCE")
                }
                val path = Path().apply {
                    moveTo(fromX.toFloat(), fromY.toFloat())
                    if (operation == "swipe") {
                        lineTo(
                            toX?.toFloat() ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
                            toY?.toFloat() ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
                        )
                    }
                }
                dispatchGesture(path, duration)
                deliveredResult()
            }
        }
    }

    private fun validateScene(
        handle: AccessibilityNodeInfo?,
        proof: AccessibilitySceneProof,
        deadline: Long,
    ): Boolean {
        if (proof.componentGeneration != componentGeneration ||
            proof.sceneRevision != sceneRevision.get() ||
            proof.display != display.snapshot() ||
            (handle != null && (!handle.refresh() || handle.windowId != proof.windowId))
        ) {
            return false
        }
        val root = activeRoot() ?: return false
        val fresh = collectHierarchy(root, deadline)
        return try {
            fresh.windowId == proof.windowId && fresh.sha256 == proof.hierarchySha256
        } finally {
            fresh.nodes.forEach { node -> runCatching { node.handle.recycle() } }
        }
    }

    private suspend fun dispatchGesture(path: Path, duration: Long) {
        suspendCancellableCoroutine { continuation ->
            val gesture = GestureDescription.Builder()
                .addStroke(GestureDescription.StrokeDescription(path, 0, duration))
                .build()
            val accepted = service.dispatchGesture(
                gesture,
                object : AccessibilityService.GestureResultCallback() {
                    override fun onCompleted(gestureDescription: GestureDescription?) {
                        if (continuation.isActive) continuation.resume(Unit)
                    }

                    override fun onCancelled(gestureDescription: GestureDescription?) {
                        if (continuation.isActive) {
                            continuation.resumeWithException(AndroidExecutionException("IO_ERROR"))
                        }
                    }
                },
                null,
            )
            if (!accepted && continuation.isActive) {
                continuation.resumeWithException(AndroidExecutionException("IO_ERROR"))
            }
        }
    }

    private fun activeRoot(): AccessibilityNodeInfo? {
        service.rootInActiveWindow?.let { return it }
        val windows = service.windows
        return try {
            windows.firstOrNull { it.isActive }?.root
                ?: windows.firstOrNull { it.isFocused }?.root
        } finally {
            windows.forEach { window -> runCatching { window.recycle() } }
        }
    }

    private suspend fun takeScreenshot(): AccessibilityService.ScreenshotResult =
        suspendCancellableCoroutine { continuation ->
            service.takeScreenshot(
                Display.DEFAULT_DISPLAY,
                service.mainExecutor,
                object : AccessibilityService.TakeScreenshotCallback {
                    override fun onSuccess(screenshot: AccessibilityService.ScreenshotResult) {
                        continuation.resume(screenshot) { _, result, _ -> result.hardwareBuffer.close() }
                    }

                    override fun onFailure(errorCode: Int) {
                        if (continuation.isActive) {
                            continuation.resumeWithException(
                                AndroidExecutionException(
                                    if (errorCode == AccessibilityService.ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT) {
                                        "RESOURCE_LIMIT"
                                    } else {
                                        "CAPABILITY_UNAVAILABLE"
                                    },
                                ),
                            )
                        }
                    }
                },
            )
        }

    private companion object {
        const val HIERARCHY_TIMEOUT_MS = 5_000L
        const val SCREENSHOT_TIMEOUT_MS = 10_000L
        const val ACTION_TIMEOUT_MS = 5_000L
        val HIERARCHY_KEYS = setOf(
            "operation", "observation_id", "max_nodes", "display", "display_generation",
        )
        val SCREENSHOT_KEYS = setOf("operation", "display", "display_generation")
        val NODE_ACTION_KEYS = setOf(
            "observation_id", "node_ref", "operation", "text", "display",
            "display_generation", "proof",
        )
        val GESTURE_KEYS = setOf(
            "observation_id", "operation", "from_x", "from_y", "to_x", "to_y",
            "duration_ms", "display", "display_generation", "proof",
        )
    }
}

private data class RetainedVisualNode(
    val handle: AccessibilityNodeInfo,
    val facts: JsonObject,
) {
    fun publicJson(nodeRef: String) = buildJsonObject {
        put("node_ref", nodeRef)
        facts.forEach { (key, value) -> put(key, value) }
    }
}

private data class CollectedHierarchy(
    val windowId: Int,
    val nodes: List<RetainedVisualNode>,
    val truncated: Boolean,
    val sha256: String,
)

private fun collectHierarchy(root: AccessibilityNodeInfo, deadline: Long): CollectedHierarchy {
    val queue = ArrayDeque<AccessibilityNodeInfo>()
    val nodes = mutableListOf<RetainedVisualNode>()
    queue.add(root)
    var truncated = false
    var current: AccessibilityNodeInfo? = null
    try {
        while (queue.isNotEmpty() && nodes.size < MAX_HIERARCHY_NODES) {
            if (SystemClock.elapsedRealtime() >= deadline) {
                throw AndroidExecutionException("TIMEOUT")
            }
            val node = queue.removeFirst()
            current = node
            val facts = nodeFacts(node)
            nodes += RetainedVisualNode(node, facts)
            current = null
            for (index in 0 until node.childCount) {
                if (SystemClock.elapsedRealtime() >= deadline) {
                    throw AndroidExecutionException("TIMEOUT")
                }
                if (nodes.size + queue.size >= MAX_HIERARCHY_NODES) {
                    truncated = true
                    break
                }
                node.getChild(index)?.let(queue::addLast)
            }
        }
        if (queue.isNotEmpty()) {
            truncated = true
            queue.forEach { pending -> runCatching { pending.recycle() } }
            queue.clear()
        }
        val windowId = root.windowId
        val fingerprint = buildJsonObject {
            put("window_id", windowId)
            put("truncated", truncated)
            put("nodes", JsonArray(nodes.map { it.facts }))
        }.toString().encodeToByteArray()
        val sha256 = MessageDigest.getInstance("SHA-256")
            .digest(fingerprint)
            .joinToString("") { byte -> "%02x".format(byte) }
        return CollectedHierarchy(windowId, nodes, truncated, sha256)
    } catch (error: Throwable) {
        current?.let { node -> runCatching { node.recycle() } }
        nodes.forEach { node -> runCatching { node.handle.recycle() } }
        queue.forEach { node -> runCatching { node.recycle() } }
        throw error
    }
}

private fun nodeFacts(node: AccessibilityNodeInfo): JsonObject {
    val bounds = Rect()
    node.getBoundsInScreen(bounds)
    return buildJsonObject {
        node.text?.toString()?.boundedUtf8()?.let { put("text", it) }
        node.contentDescription?.toString()?.boundedUtf8()?.let { put("content_description", it) }
        node.viewIdResourceName?.boundedUtf8()?.let { put("resource_id", it) }
        node.className?.toString()?.boundedUtf8()?.let { put("class_name", it) }
        node.packageName?.toString()?.boundedUtf8()?.let { put("package_name", it) }
        put("bounds", buildJsonObject {
            put("left", bounds.left)
            put("top", bounds.top)
            put("right", bounds.right)
            put("bottom", bounds.bottom)
        })
        put("checkable", node.isCheckable)
        put("checked", node.isChecked)
        put("clickable", node.isClickable)
        put("enabled", node.isEnabled)
        put("focusable", node.isFocusable)
        put("focused", node.isFocused)
        put("scrollable", node.isScrollable)
        put("long_clickable", node.isLongClickable)
        put("password", node.isPassword)
        put("selected", node.isSelected)
        put("editable", node.isEditable)
    }
}

private fun JsonObject.expectedDisplay(): VisualDisplaySnapshot {
    val geometry = get("display")?.jsonObject ?: throw AndroidExecutionException("INVALID_ARGUMENT")
    if (geometry.keys != setOf("width", "height", "rotation", "density_dpi")) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    val snapshot = VisualDisplaySnapshot(
        width = geometry.requiredInt("width"),
        height = geometry.requiredInt("height"),
        rotation = geometry.requiredInt("rotation"),
        densityDpi = geometry.requiredInt("density_dpi"),
        generation = get("display_generation")?.jsonPrimitive?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
    )
    if (snapshot.width !in 1..16_384 || snapshot.height !in 1..16_384 ||
        snapshot.rotation !in setOf(0, 90, 180, 270) || snapshot.densityDpi <= 0 ||
        snapshot.generation <= 0
    ) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    return snapshot
}

private fun JsonObject.sceneProof(): AccessibilitySceneProof {
    val proof = get("proof")?.jsonObject ?: throw AndroidExecutionException("INVALID_ARGUMENT")
    if (proof.keys != setOf(
            "component_generation", "window_id", "scene_revision", "hierarchy_sha256",
        )
    ) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    val admitted = expectedDisplay()
    return AccessibilitySceneProof(
        componentGeneration = proof["component_generation"]?.jsonPrimitive?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
        windowId = proof.requiredInt("window_id"),
        sceneRevision = proof["scene_revision"]?.jsonPrimitive?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
        display = admitted,
        hierarchySha256 = proof.requiredString("hierarchy_sha256"),
    )
}

private fun requirePoint(display: VisualDisplaySnapshot, x: Int, y: Int) {
    if (x !in 0 until display.width || y !in 0 until display.height) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
}

private fun AndroidExecutionRequest.payloadObject(): JsonObject = runCatching {
    Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
}.getOrElse { throw AndroidExecutionException("INVALID_ARGUMENT") }

private fun JsonObject.requiredString(key: String): String =
    get(key)?.jsonPrimitive?.contentOrNull ?: throw AndroidExecutionException("INVALID_ARGUMENT")

private fun JsonObject.requiredInt(key: String): Int =
    get(key)?.jsonPrimitive?.intOrNull ?: throw AndroidExecutionException("INVALID_ARGUMENT")

private fun deliveredResult() = AndroidExecutionResult("{\"delivered\":true}".encodeToByteArray())

private fun String.boundedUtf8(): String {
    if (encodeToByteArray().size <= MAX_NODE_STRING_BYTES) return this
    var end = 0
    var bytes = 0
    while (end < length) {
        val codePoint = codePointAt(end)
        val charCount = Character.charCount(codePoint)
        val encoded = substring(end, end + charCount).encodeToByteArray().size
        if (bytes + encoded > MAX_NODE_STRING_BYTES) break
        bytes += encoded
        end += charCount
    }
    return substring(0, end)
}

private const val MAX_HIERARCHY_NODES = 5_000
private const val MAX_NODE_STRING_BYTES = 4_096
