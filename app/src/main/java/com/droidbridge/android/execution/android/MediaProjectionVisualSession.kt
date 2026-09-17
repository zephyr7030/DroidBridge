package com.droidbridge.android.execution.android

import android.app.Activity
import android.app.Service
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.Image
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Handler
import android.os.HandlerThread
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

internal class MediaProjectionVisualController(
    private val service: Service,
    private val registry: AndroidExecutionRegistry,
    private val display: VisualDisplayTracker,
    private val encoder: VisualImageEncoder,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val setForeground: (Boolean) -> Unit,
) {
    private val componentGeneration = AtomicLong(1)
    private val slot = ProjectionSessionSlot<MediaProjectionVisualSession>()

    @Synchronized
    fun start(resultCode: Int, data: Intent) {
        if (slot.current() != null) return
        if (resultCode != Activity.RESULT_OK) {
            publishUnavailable("USER_CONSENT_REQUIRED")
            return
        }
        setForeground(true)
        val projection = try {
            service.getSystemService(MediaProjectionManager::class.java)
                .getMediaProjection(resultCode, data)
                ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        } catch (_: Exception) {
            setForeground(false)
            publishUnavailable("USER_CONSENT_REQUIRED")
            return
        }
        val generation = componentGeneration.incrementAndGet()
        val session = try {
            MediaProjectionVisualSession.create(
                projection = projection,
                display = display,
                encoder = encoder,
                onStopped = ::stop,
            )
        } catch (_: Exception) {
            runCatching { projection.stop() }
            setForeground(false)
            publishUnavailable("USER_CONSENT_REQUIRED")
            return
        }
        try {
            slot.publish(session)
            check(
                registry.register(
                    CapabilityRegistration(
                        key = PROJECTION_KEY,
                        state = RegisteredCapabilityState.Available,
                        reason = null,
                        sourceGeneration = generation,
                        executor = MediaProjectionVisualAdapter(
                            session,
                            validatesFence,
                            onFatal = ::stop,
                        ),
                        primitives = setOf(AndroidPrimitive.MediaProjectionCapture),
                    ),
                ),
            )
        } catch (_: Exception) {
            val removed = slot.take()
            publishUnavailable("USER_CONSENT_REQUIRED")
            removed?.close()
            setForeground(false)
        }
    }

    @Synchronized
    fun stop() {
        val removed = slot.take() ?: return
        publishUnavailable("USER_CONSENT_REQUIRED")
        removed.close()
        setForeground(false)
    }

    @Synchronized
    fun publishInitialUnavailable() {
        if (slot.current() == null) publishUnavailable("USER_CONSENT_REQUIRED")
    }

    private fun publishUnavailable(reason: String) {
        val generation = componentGeneration.incrementAndGet()
        registry.register(
            CapabilityRegistration(
                key = PROJECTION_KEY,
                state = RegisteredCapabilityState.Unavailable,
                reason = reason,
                sourceGeneration = generation,
            ),
        )
    }

    private companion object {
        const val PROJECTION_KEY = "visual.media_projection_session"
    }
}

private class MediaProjectionVisualAdapter(
    private val session: MediaProjectionVisualSession,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val onFatal: () -> Unit,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (request.primitive != AndroidPrimitive.MediaProjectionCapture) {
            throw AndroidExecutionException("UNSUPPORTED")
        }
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.descriptors.isNotEmpty() || request.payload.size > 1_048_576) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val input = runCatching {
            Json.parseToJsonElement(
                request.payload.decodeToString(throwOnInvalidSequence = true),
            ).jsonObject
        }.getOrElse { throw AndroidExecutionException("INVALID_ARGUMENT") }
        return try {
            session.capture(input)
        } catch (error: ProjectionSessionFailure) {
            onFatal()
            throw AndroidExecutionException(error.code)
        }
    }
}

private class MediaProjectionVisualSession private constructor(
    private val projection: MediaProjection,
    private val display: VisualDisplayTracker,
    private val encoder: VisualImageEncoder,
    private val virtualDisplay: VirtualDisplay,
    private val callback: MediaProjection.Callback,
    private val thread: HandlerThread,
    private val handler: Handler,
    private var reader: ImageReader,
    private var geometry: VisualDisplaySnapshot,
) {
    private val captures = Mutex()
    @Volatile private var closed = false
    private var pending: CancellableContinuation<Image>? = null

    suspend fun capture(input: JsonObject): AndroidExecutionResult = captures.withLock {
        if (closed) throw ProjectionSessionFailure("CAPABILITY_UNAVAILABLE")
        val admitted = input.expectedProjectionDisplay()
        val current = display.snapshot()
        if (current != geometry) reconfigure(current)
        if (admitted != current) throw AndroidExecutionException("STALE_AUTHORITY")
        val image = withTimeout(CAPTURE_TIMEOUT_MS) { awaitNextImage() }
        val bitmap = try {
            withContext(Dispatchers.Default) { image.toBitmap(current.width, current.height) }
        } finally {
            image.close()
        }
        try {
            val encoded = encoder.encodeAuto(bitmap)
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
            bitmap.recycle()
        }
    }

    fun close() {
        val waiter = synchronized(this) {
            if (closed) return
            closed = true
            pending.also { pending = null }
        }
        runCatching { reader.setOnImageAvailableListener(null, null) }
        waiter?.resumeWithException(ProjectionSessionFailure("CAPABILITY_UNAVAILABLE"))
        runCatching { virtualDisplay.setSurface(null) }
        runCatching { reader.close() }
        runCatching { virtualDisplay.release() }
        runCatching { projection.unregisterCallback(callback) }
        runCatching { projection.stop() }
        thread.quitSafely()
    }

    private fun reconfigure(snapshot: VisualDisplaySnapshot) {
        if (closed) throw ProjectionSessionFailure("CAPABILITY_UNAVAILABLE")
        val replacement = newReader(snapshot)
        try {
            virtualDisplay.resize(snapshot.width, snapshot.height, snapshot.densityDpi)
            virtualDisplay.surface = replacement.surface
        } catch (error: Exception) {
            replacement.close()
            throw ProjectionSessionFailure("CAPABILITY_UNAVAILABLE")
        }
        val previous = reader
        reader = replacement
        geometry = snapshot
        previous.close()
    }

    private suspend fun awaitNextImage(): Image = suspendCancellableCoroutine { continuation ->
        val source = reader
        while (true) {
            val stale = source.acquireLatestImage() ?: break
            stale.close()
        }
        val startedAt = System.nanoTime()
        val listener = ImageReader.OnImageAvailableListener { available ->
            val image = runCatching { available.acquireLatestImage() }.getOrNull()
            if (image != null) {
                if (image.timestamp < startedAt) {
                    image.close()
                } else {
                    val ownsWaiter = synchronized(this) {
                        if (pending === continuation) {
                            pending = null
                            true
                        } else {
                            false
                        }
                    }
                    if (ownsWaiter) {
                        runCatching { source.setOnImageAvailableListener(null, null) }
                        continuation.resume(image) { _, acquired, _ -> acquired.close() }
                    } else {
                        image.close()
                    }
                }
            }
        }
        synchronized(this) {
            if (closed) {
                continuation.resumeWithException(ProjectionSessionFailure("CAPABILITY_UNAVAILABLE"))
                return@suspendCancellableCoroutine
            }
            pending = continuation
            try {
                source.setOnImageAvailableListener(listener, handler)
            } catch (error: Exception) {
                pending = null
                continuation.resumeWithException(ProjectionSessionFailure("CAPABILITY_UNAVAILABLE"))
                return@suspendCancellableCoroutine
            }
        }
        continuation.invokeOnCancellation {
            val owned = synchronized(this) {
                if (pending === continuation) {
                    pending = null
                    true
                } else {
                    false
                }
            }
            if (owned) runCatching { source.setOnImageAvailableListener(null, null) }
        }
    }

    companion object {
        fun create(
            projection: MediaProjection,
            display: VisualDisplayTracker,
            encoder: VisualImageEncoder,
            onStopped: () -> Unit,
        ): MediaProjectionVisualSession {
            val thread = HandlerThread("DroidBridgeProjection").apply { start() }
            var reader: ImageReader? = null
            val callback = object : MediaProjection.Callback() {
                override fun onStop() {
                    onStopped()
                }
            }
            try {
                val handler = Handler(thread.looper)
                val snapshot = display.snapshot()
                val activeReader = newReader(snapshot)
                reader = activeReader
                projection.registerCallback(callback, handler)
                val virtualDisplay = projection.createVirtualDisplay(
                    "DroidBridgeCapture",
                    snapshot.width,
                    snapshot.height,
                    snapshot.densityDpi,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    activeReader.surface,
                    null,
                    handler,
                ) ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                return MediaProjectionVisualSession(
                    projection,
                    display,
                    encoder,
                    virtualDisplay,
                    callback,
                    thread,
                    handler,
                    activeReader,
                    snapshot,
                )
            } catch (error: Exception) {
                runCatching { projection.unregisterCallback(callback) }
                reader?.close()
                thread.quitSafely()
                throw error
            }
        }

        private fun newReader(snapshot: VisualDisplaySnapshot): ImageReader = ImageReader.newInstance(
            snapshot.width,
            snapshot.height,
            PixelFormat.RGBA_8888,
            2,
        )

        private const val CAPTURE_TIMEOUT_MS = 10_000L
    }
}

private class ProjectionSessionFailure(val code: String) : IllegalStateException(code)

private fun Image.toBitmap(width: Int, height: Int): Bitmap {
    if (width <= 0 || height <= 0 || width.toLong() * height * 4 > MAX_PROJECTION_RAW_BYTES) {
        throw AndroidExecutionException("RESOURCE_LIMIT")
    }
    val plane = planes.singleOrNull() ?: throw AndroidExecutionException("IO_ERROR")
    if (plane.pixelStride != 4 || plane.rowStride < width * 4) {
        throw AndroidExecutionException("IO_ERROR")
    }
    val source = plane.buffer.duplicate()
    val start = source.position()
    val required = start.toLong() + (height - 1L) * plane.rowStride + width.toLong() * 4
    if (required > source.limit()) throw AndroidExecutionException("IO_ERROR")
    val pixels = IntArray(width * height)
    for (y in 0 until height) {
        val row = start + y * plane.rowStride
        for (x in 0 until width) {
            val offset = row + x * plane.pixelStride
            val red = source.get(offset).toInt() and 0xff
            val green = source.get(offset + 1).toInt() and 0xff
            val blue = source.get(offset + 2).toInt() and 0xff
            val alpha = source.get(offset + 3).toInt() and 0xff
            pixels[y * width + x] =
                (alpha shl 24) or (red shl 16) or (green shl 8) or blue
        }
    }
    return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888)
}

private fun JsonObject.expectedProjectionDisplay(): VisualDisplaySnapshot {
    if (keys != setOf("display", "display_generation")) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    val geometry = get("display")?.jsonObject ?: throw AndroidExecutionException("INVALID_ARGUMENT")
    if (geometry.keys != setOf("width", "height", "rotation", "density_dpi")) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    fun integer(key: String) = geometry[key]?.jsonPrimitive?.longOrNull
        ?.takeIf { it in Int.MIN_VALUE..Int.MAX_VALUE }?.toInt()
        ?: throw AndroidExecutionException("INVALID_ARGUMENT")
    val snapshot = VisualDisplaySnapshot(
        integer("width"),
        integer("height"),
        integer("rotation"),
        integer("density_dpi"),
        get("display_generation")?.jsonPrimitive?.longOrNull
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

private const val MAX_PROJECTION_RAW_BYTES = 67_108_864L
