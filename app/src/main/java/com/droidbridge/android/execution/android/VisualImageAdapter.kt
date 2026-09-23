package com.droidbridge.android.execution.android

import android.content.Context
import android.content.res.AssetFileDescriptor
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.os.ParcelFileDescriptor
import android.view.Display
import android.view.Surface
import android.view.WindowManager
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.Callable
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal data class VisualDisplaySnapshot(
    val width: Int,
    val height: Int,
    val rotation: Int,
    val densityDpi: Int,
    val generation: Long,
) {
    fun geometryJson() = buildJsonObject {
        put("width", width)
        put("height", height)
        put("rotation", rotation)
        put("density_dpi", densityDpi)
    }
}

internal class VisualDisplayTracker(context: Context) {
    private val windowManager = context.getSystemService(WindowManager::class.java)
    private val displayManager = context.getSystemService(DisplayManager::class.java)
    private val resources = context.resources
    private var generation = 1L
    private var lastGeometry: List<Int>? = null

    @Synchronized
    fun snapshot(): VisualDisplaySnapshot {
        val bounds = windowManager.currentWindowMetrics.bounds
        val rotation = displayManager.getDisplay(Display.DEFAULT_DISPLAY)?.rotation
            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        val width = bounds.width()
        val height = bounds.height()
        val densityDpi = resources.configuration.densityDpi
        if (width !in 1..16_384 || height !in 1..16_384 || densityDpi <= 0) {
            throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        }
        val geometry = listOf(width, height, visualRotationDegrees(rotation), densityDpi)
        if (lastGeometry != null && lastGeometry != geometry) {
            if (generation == Long.MAX_VALUE) throw AndroidExecutionException("RESOURCE_LIMIT")
            generation += 1
        }
        lastGeometry = geometry
        return VisualDisplaySnapshot(
            width = geometry[0],
            height = geometry[1],
            rotation = geometry[2],
            densityDpi = geometry[3],
            generation = generation,
        )
    }
}

internal data class EncodedVisualImage(
    val file: File,
    val format: String,
    val mime: String,
    val width: Int,
    val height: Int,
) {
    fun discard() {
        if (file.exists() && !file.delete()) throw AndroidExecutionException("IO_ERROR")
    }

    fun transfer(display: VisualDisplaySnapshot? = null): AndroidExecutionResult {
        val size = file.length()
        if (size !in 1..MAX_ENCODED_BYTES || width !in 1..16_384 || height !in 1..16_384) {
            file.delete()
            throw AndroidExecutionException("RESOURCE_LIMIT")
        }
        val descriptor = try {
            ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
        } catch (_: Exception) {
            file.delete()
            throw AndroidExecutionException("IO_ERROR")
        }
        if (!file.delete()) {
            descriptor.close()
            throw AndroidExecutionException("IO_ERROR")
        }
        val payload = buildJsonObject {
            put("format", format)
            put("mime", mime)
            put("size", size)
            put("width", width)
            put("height", height)
            display?.let {
                put("display", it.geometryJson())
                put("display_generation", it.generation)
            }
        }
        return AndroidExecutionResult(
            payload.toString().encodeToByteArray(),
            listOf(RoleDescriptor("visual_encoded_image", descriptor)),
        )
    }

    companion object {
        const val MAX_ENCODED_BYTES = 8_388_608L
    }
}

internal class VisualImageEncoder(private val context: Context) {
    init {
        val leftovers = context.cacheDir.listFiles { file ->
            file.isFile && file.name.startsWith(OUTPUT_PREFIX)
        } ?: throw AndroidExecutionException("IO_ERROR")
        if (leftovers.any { file -> !file.delete() }) {
            throw AndroidExecutionException("IO_ERROR")
        }
    }

    suspend fun encodeAuto(bitmap: Bitmap): EncodedVisualImage {
        var ownedFile: File? = null
        return try {
            withTimeout(PHASE_TIMEOUT_MS) {
                withContext(Dispatchers.IO) {
                    val file = File.createTempFile(OUTPUT_PREFIX, ".jpg", context.cacheDir)
                    ownedFile = file
                    try {
                        FileOutputStream(file).use { output ->
                            if (!bitmap.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, output)) {
                                throw AndroidExecutionException("IO_ERROR")
                            }
                            output.fd.sync()
                        }
                        EncodedVisualImage(file, "jpeg", "image/jpeg", bitmap.width, bitmap.height)
                    } catch (error: Throwable) {
                        file.delete()
                        throw error
                    }
                }
            }.also { ownedFile = null }
        } catch (error: Throwable) {
            ownedFile?.delete()
            throw error
        }
    }

    private companion object {
        const val PHASE_TIMEOUT_MS = 10_000L
        const val JPEG_QUALITY = 85
        const val OUTPUT_PREFIX = "droidbridge-visual-"
    }
}

internal class VisualFrameworkAdapter(
    private val display: VisualDisplayTracker,
    private val encoder: VisualImageEncoder,
    private val validatesFence: (String, Long, String) -> Boolean,
    private val sceneActivity: VisualSceneActivity,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        validateVisualRequest(request)
        return when (request.primitive) {
            AndroidPrimitive.VisualDisplaySnapshot -> display(request)
            AndroidPrimitive.VisualImageTransform -> transform(request)
            else -> throw AndroidExecutionException("UNSUPPORTED")
        }
    }

    /** The first step of every observation, so the image and nodes that follow share a still scene. */
    private suspend fun display(request: AndroidExecutionRequest): AndroidExecutionResult {
        requireNoDescriptors(request)
        val input = request.objectPayload(setOf("operation"))
        if (input["operation"]?.jsonPrimitive?.contentOrNull != "display") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        sceneActivity.awaitQuiet()
        val snapshot = display.snapshot()
        return AndroidExecutionResult(
            buildJsonObject {
                put("display", snapshot.geometryJson())
                put("display_generation", snapshot.generation)
            }.toString().encodeToByteArray(),
        )
    }

    private suspend fun transform(request: AndroidExecutionRequest): AndroidExecutionResult {
        val input = request.objectPayload(setOf("region"))
        if (request.descriptors.size != 1 || request.descriptors.single().role != "visual_source_image") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        var ownedDecoded: Bitmap? = null
        val decoded = try {
            withTimeout(10_000) {
                withContext(Dispatchers.IO) {
                    ImageDecoder.decodeBitmap(
                        request.descriptors.single().descriptor.imageDecoderSource(),
                    ) { decoder, info, _ ->
                        val size = info.size
                        if (size.width <= 0 || size.height <= 0 || size.width > 16_384 || size.height > 16_384 ||
                            size.width.toLong() * size.height * 4 > 67_108_864L
                        ) {
                            throw AndroidExecutionException("RESOURCE_LIMIT")
                        }
                        decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
                    }.also { ownedDecoded = it }
                }
            }
        } catch (error: Throwable) {
            ownedDecoded?.recycle()
            throw error
        }
        ownedDecoded = null
        val transformed = try {
            // The Runtime always names `region`; a JSON null means the whole image.
            input["region"]?.takeUnless { it is kotlinx.serialization.json.JsonNull }?.jsonObject?.let { region ->
                if (region.keys != setOf("x", "y", "width", "height")) {
                    throw AndroidExecutionException("INVALID_ARGUMENT")
                }
                val x = region.positiveOrZero("x")
                val y = region.positiveOrZero("y")
                val width = region.positiveInt("width")
                val height = region.positiveInt("height")
                if (x.toLong() + width > decoded.width || y.toLong() + height > decoded.height) {
                    throw AndroidExecutionException("INVALID_ARGUMENT")
                }
                Bitmap.createBitmap(decoded, x, y, width, height)
            } ?: decoded
        } catch (error: Throwable) {
            decoded.recycle()
            throw error
        }
        return try {
            encoder.encodeAuto(transformed).transfer()
        } finally {
            if (transformed !== decoded) transformed.recycle()
            decoded.recycle()
        }
    }

    private fun validateVisualRequest(request: AndroidExecutionRequest) {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.payload.size > 1_048_576) throw AndroidExecutionException("RESOURCE_LIMIT")
    }
}

private fun ParcelFileDescriptor.imageDecoderSource(): ImageDecoder.Source =
    ImageDecoder.createSource(
        Callable {
            AssetFileDescriptor(
                ParcelFileDescriptor.dup(fileDescriptor),
                0,
                AssetFileDescriptor.UNKNOWN_LENGTH,
            )
        },
    )

private fun java.io.InputStream.readExact(size: Int): ByteArray {
    val output = ByteArray(size)
    var offset = 0
    while (offset < size) {
        val read = read(output, offset, size - offset)
        if (read < 0) throw AndroidExecutionException("IO_ERROR")
        offset += read
    }
    return output
}

internal fun visualRotationDegrees(rotation: Int): Int = when (rotation) {
    Surface.ROTATION_0 -> 0
    Surface.ROTATION_90 -> 90
    Surface.ROTATION_180 -> 180
    Surface.ROTATION_270 -> 270
    else -> throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
}

private fun AndroidExecutionRequest.objectPayload(allowed: Set<String>): JsonObject {
    val value = runCatching {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    }.getOrElse { throw AndroidExecutionException("INVALID_ARGUMENT") }
    if (value.keys.any { it !in allowed }) throw AndroidExecutionException("INVALID_ARGUMENT")
    return value
}

private fun requireNoDescriptors(request: AndroidExecutionRequest) {
    if (request.descriptors.isNotEmpty()) throw AndroidExecutionException("INVALID_ARGUMENT")
}

private fun JsonObject.positiveInt(key: String): Int =
    get(key)?.jsonPrimitive?.intOrNull?.takeIf { it > 0 }
        ?: throw AndroidExecutionException("INVALID_ARGUMENT")

private fun JsonObject.positiveOrZero(key: String): Int =
    get(key)?.jsonPrimitive?.intOrNull?.takeIf { it >= 0 }
        ?: throw AndroidExecutionException("INVALID_ARGUMENT")
