package com.droidbridge.android.execution.android

import android.content.Context
import android.content.res.AssetFileDescriptor
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.MediaMuxer
import android.media.Image
import android.os.ParcelFileDescriptor
import android.util.Size
import android.view.Display
import android.view.Surface
import android.view.WindowManager
import java.io.BufferedInputStream
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.Callable
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
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

internal class VisualImageEncoder(
    private val context: Context,
    private val health: VisualCodecHealth,
) {
    init {
        val leftovers = context.cacheDir.listFiles { file ->
            file.isFile && file.name.startsWith(OUTPUT_PREFIX)
        } ?: throw AndroidExecutionException("IO_ERROR")
        if (leftovers.any { file -> !file.delete() }) {
            throw AndroidExecutionException("IO_ERROR")
        }
    }

    suspend fun encodeAuto(bitmap: Bitmap): EncodedVisualImage {
        val snapshot = try {
            withTimeout(PHASE_TIMEOUT_MS) {
                withContext(Dispatchers.IO) { health.snapshot(bitmap.width, bitmap.height) }
            }
        } catch (error: TimeoutCancellationException) {
            health.invalidateCurrent()
            throw error
        }
        if (snapshot.available) {
            try {
                return encodeHeic(bitmap, snapshot.generation)
            } catch (_: TimeoutCancellationException) {
                health.invalidate(snapshot.generation)
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                health.invalidate(snapshot.generation)
            }
        }
        return encodePng(bitmap)
    }

    suspend fun encodeRequested(
        bitmap: Bitmap,
        requested: String,
        codecGeneration: Long?,
    ): EncodedVisualImage = when (requested) {
        "png" -> {
            if (codecGeneration != null) throw AndroidExecutionException("INVALID_ARGUMENT")
            encodePng(bitmap)
        }
        "heic" -> {
            val generation = codecGeneration ?: throw AndroidExecutionException("INVALID_ARGUMENT")
            if (!health.accepts(generation, bitmap.width, bitmap.height)) {
                throw AndroidExecutionException("STALE_AUTHORITY")
            }
            try {
                encodeHeic(bitmap, generation)
            } catch (_: TimeoutCancellationException) {
                health.invalidate(generation)
                throw AndroidExecutionException("TIMEOUT")
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                health.invalidate(generation)
                throw if (error is AndroidExecutionException) error else AndroidExecutionException("IO_ERROR")
            }
        }
        else -> throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    private suspend fun encodePng(bitmap: Bitmap): EncodedVisualImage {
        var ownedFile: File? = null
        return try {
            withTimeout(PHASE_TIMEOUT_MS) {
                withContext(Dispatchers.IO) {
                    val file = outputFile("png")
                    ownedFile = file
                    try {
                        FileOutputStream(file).use { output ->
                            if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)) {
                                throw AndroidExecutionException("IO_ERROR")
                            }
                            output.fd.sync()
                        }
                        EncodedVisualImage(file, "png", "image/png", bitmap.width, bitmap.height)
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

    private suspend fun encodeHeic(bitmap: Bitmap, generation: Long): EncodedVisualImage {
        var ownedFile: File? = null
        return try {
            withTimeout(PHASE_TIMEOUT_MS) {
                withContext(Dispatchers.IO) {
                    val codecName = health.candidate(generation, bitmap.width, bitmap.height)
                        ?: throw AndroidExecutionException("STALE_AUTHORITY")
                    val file = outputFile("heic")
                    ownedFile = file
                    try {
                        encodeHeicFile(bitmap, codecName, file)
                        EncodedVisualImage(file, "heic", "image/heic", bitmap.width, bitmap.height)
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

    private suspend fun encodeHeicFile(bitmap: Bitmap, codecName: String, output: File) {
        val yuv = bitmapToI420(bitmap)
        val codec = MediaCodec.createByCodecName(codecName)
        var codecStarted = false
        var muxer: MediaMuxer? = null
        var muxerStarted = false
        try {
            codec.configure(
                visualHeicFormat(
                    bitmap.width,
                    bitmap.height,
                    MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible,
                    codec.codecInfo.getCapabilitiesForType(MediaFormat.MIMETYPE_IMAGE_ANDROID_HEIC),
                ),
                null,
                null,
                MediaCodec.CONFIGURE_FLAG_ENCODE,
            )
            codec.start()
            codecStarted = true
            val deadline = android.os.SystemClock.elapsedRealtime() + PHASE_TIMEOUT_MS
            val inputIndex = awaitInput(codec, deadline)
            val input = codec.getInputBuffer(inputIndex)
                ?: throw AndroidExecutionException("IO_ERROR")
            if (input.capacity() < yuv.size) throw AndroidExecutionException("RESOURCE_LIMIT")
            val inputImage = codec.getInputImage(inputIndex)
                ?: throw AndroidExecutionException("IO_ERROR")
            copyI420ToImage(yuv, bitmap.width, bitmap.height, inputImage)
            codec.queueInputBuffer(
                inputIndex,
                0,
                input.capacity(),
                132,
                0,
            )
            val eosIndex = awaitInput(codec, deadline)
            codec.queueInputBuffer(
                eosIndex,
                0,
                0,
                1_000_132,
                MediaCodec.BUFFER_FLAG_END_OF_STREAM,
            )
            val info = MediaCodec.BufferInfo()
            var track = -1
            var complete = false
            while (!complete) {
                kotlinx.coroutines.currentCoroutineContext().ensureActive()
                val remaining = deadline - android.os.SystemClock.elapsedRealtime()
                if (remaining <= 0) throw AndroidExecutionException("TIMEOUT")
                when (val index = codec.dequeueOutputBuffer(info, minOf(remaining * 1_000, 100_000))) {
                    MediaCodec.INFO_TRY_AGAIN_LATER -> Unit
                    MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                        if (muxer != null) throw AndroidExecutionException("IO_ERROR")
                        muxer = MediaMuxer(output.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_HEIF)
                        track = muxer.addTrack(codec.outputFormat)
                        muxer.start()
                        muxerStarted = true
                    }
                    else -> if (index >= 0) {
                        val data = codec.getOutputBuffer(index)
                            ?: throw AndroidExecutionException("IO_ERROR")
                        if (info.size > 0 && info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                            if (!muxerStarted || track < 0) throw AndroidExecutionException("IO_ERROR")
                            data.position(info.offset)
                            data.limit(info.offset + info.size)
                            muxer?.writeSampleData(track, data, info)
                        }
                        complete = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                        codec.releaseOutputBuffer(index, false)
                    }
                }
            }
            if (!muxerStarted) throw AndroidExecutionException("IO_ERROR")
            muxer?.stop()
            muxerStarted = false
        } finally {
            if (muxerStarted) runCatching { muxer?.stop() }
            runCatching { muxer?.release() }
            if (codecStarted) runCatching { codec.stop() }
            codec.release()
        }
    }

    private suspend fun awaitInput(codec: MediaCodec, deadline: Long): Int {
        while (true) {
            kotlinx.coroutines.currentCoroutineContext().ensureActive()
            val remaining = deadline - android.os.SystemClock.elapsedRealtime()
            if (remaining <= 0) throw AndroidExecutionException("TIMEOUT")
            val index = codec.dequeueInputBuffer(minOf(remaining * 1_000, 100_000))
            if (index >= 0) return index
        }
    }

    private suspend fun bitmapToI420(bitmap: Bitmap): ByteArray {
        val width = bitmap.width
        val height = bitmap.height
        if (width <= 0 || height <= 0 || width > 16_384 || height > 16_384 ||
            width % 2 != 0 || height % 2 != 0
        ) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val pixels = width.toLong() * height
        if (pixels * 4 > MAX_RAW_BYTES) throw AndroidExecutionException("RESOURCE_LIMIT")
        val output = ByteArray((pixels + pixels / 2).toInt())
        val row = IntArray(width)
        var yOffset = 0
        val uOffset = pixels.toInt()
        val vOffset = uOffset + (pixels / 4).toInt()
        for (y in 0 until height) {
            kotlinx.coroutines.currentCoroutineContext().ensureActive()
            bitmap.getPixels(row, 0, width, 0, y, width, 1)
            for (x in 0 until width) {
                val color = row[x]
                val red = color ushr 16 and 0xff
                val green = color ushr 8 and 0xff
                val blue = color and 0xff
                output[yOffset++] = clamp(((66 * red + 129 * green + 25 * blue + 128) shr 8) + 16).toByte()
                if (y % 2 == 0 && x % 2 == 0) {
                    val chroma = (y / 2) * (width / 2) + x / 2
                    output[uOffset + chroma] = clamp(((-38 * red - 74 * green + 112 * blue + 128) shr 8) + 128).toByte()
                    output[vOffset + chroma] = clamp(((112 * red - 94 * green - 18 * blue + 128) shr 8) + 128).toByte()
                }
            }
        }
        return output
    }

    private fun outputFile(extension: String): File =
        File.createTempFile(OUTPUT_PREFIX, ".$extension", context.cacheDir)

    private fun clamp(value: Int): Int = value.coerceIn(0, 255)

    private companion object {
        const val PHASE_TIMEOUT_MS = 10_000L
        const val MAX_RAW_BYTES = 67_108_864L
        const val OUTPUT_PREFIX = "droidbridge-visual-"
    }
}

private fun copyI420ToImage(source: ByteArray, width: Int, height: Int, image: Image) {
    val expected = width.toLong() * height * 3 / 2
    if (source.size.toLong() != expected || image.planes.size != 3) {
        throw AndroidExecutionException("IO_ERROR")
    }
    val ySize = width * height
    val planeOffsets = intArrayOf(0, ySize, ySize + ySize / 4)
    image.planes.forEachIndexed { planeIndex, plane ->
        val divisor = if (planeIndex == 0) 1 else 2
        val planeWidth = width / divisor
        val planeHeight = height / divisor
        val rowStride = plane.rowStride
        val pixelStride = plane.pixelStride
        val destination = plane.buffer
        val base = destination.position()
        if (rowStride < planeWidth * pixelStride || pixelStride <= 0) {
            throw AndroidExecutionException("IO_ERROR")
        }
        for (row in 0 until planeHeight) {
            val sourceRow = planeOffsets[planeIndex] + row * planeWidth
            val destinationRow = base + row * rowStride
            val last = destinationRow + (planeWidth - 1) * pixelStride
            if (destinationRow < 0 || last >= destination.limit()) {
                throw AndroidExecutionException("IO_ERROR")
            }
            for (column in 0 until planeWidth) {
                destination.put(
                    destinationRow + column * pixelStride,
                    source[sourceRow + column],
                )
            }
        }
    }
}

internal class VisualFrameworkAdapter(
    private val display: VisualDisplayTracker,
    private val encoder: VisualImageEncoder,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        validateVisualRequest(request)
        return when (request.primitive) {
            AndroidPrimitive.AccessibilityObserve -> display(request)
            AndroidPrimitive.VisualFrameEncode -> frame(request)
            AndroidPrimitive.VisualImageTransform -> transform(request)
            else -> throw AndroidExecutionException("UNSUPPORTED")
        }
    }

    private fun display(request: AndroidExecutionRequest): AndroidExecutionResult {
        requireNoDescriptors(request)
        val input = request.objectPayload(setOf("operation"))
        if (input["operation"]?.jsonPrimitive?.contentOrNull != "display") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val snapshot = display.snapshot()
        return AndroidExecutionResult(
            buildJsonObject {
                put("display", snapshot.geometryJson())
                put("display_generation", snapshot.generation)
            }.toString().encodeToByteArray(),
        )
    }

    private suspend fun frame(request: AndroidExecutionRequest): AndroidExecutionResult {
        val input = request.objectPayload(
            setOf("width", "height", "pixel_format", "colorspace", "requested", "codec_generation"),
        )
        if (request.descriptors.size != 1 || request.descriptors.single().role != "visual_raw_frame") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val width = input.positiveInt("width")
        val height = input.positiveInt("height")
        val pixelFormat = input["pixel_format"]?.jsonPrimitive?.intOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        val colorspace = input["colorspace"]?.jsonPrimitive?.intOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        if (width > 16_384 || height > 16_384 || pixelFormat !in 1..5 || colorspace !in 0..2) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val requested = input["requested"]?.jsonPrimitive?.contentOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        val generation = input["codec_generation"]?.jsonPrimitive?.longOrNull
        if ((requested == "heic") != (generation != null) || requested !in setOf("heic", "png")) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        var ownedBitmap: Bitmap? = null
        val bitmap = try {
            withTimeout(10_000) {
                withContext(Dispatchers.IO) {
                    decodeRawBitmap(
                        request.descriptors.single().descriptor,
                        width,
                        height,
                        pixelFormat,
                        colorspace,
                    ).also { ownedBitmap = it }
                }
            }
        } catch (error: Throwable) {
            ownedBitmap?.recycle()
            throw error
        }
        ownedBitmap = null
        return try {
            encoder.encodeRequested(bitmap, requested, generation).transfer()
        } finally {
            bitmap.recycle()
        }
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

private fun decodeRawBitmap(
    descriptor: ParcelFileDescriptor,
    width: Int,
    height: Int,
    pixelFormat: Int,
    colorspace: Int,
): Bitmap {
    if (colorspace !in 0..2) throw AndroidExecutionException("INVALID_ARGUMENT")
    val bytesPerPixel = when (pixelFormat) {
        1, 2, 5 -> 4
        3 -> 3
        4 -> 2
        else -> throw AndroidExecutionException("INVALID_ARGUMENT")
    }
    val payloadSize = width.toLong() * height * bytesPerPixel
    if (payloadSize > 67_108_864L) throw AndroidExecutionException("RESOURCE_LIMIT")
    val input = BufferedInputStream(descriptor.duplicateInput())
    input.use {
        val header = it.readExact(16)
        val observed = IntArray(4) { index ->
            val offset = index * 4
            (header[offset].toInt() and 0xff) or
                ((header[offset + 1].toInt() and 0xff) shl 8) or
                ((header[offset + 2].toInt() and 0xff) shl 16) or
                ((header[offset + 3].toInt() and 0xff) shl 24)
        }
        if (!observed.contentEquals(intArrayOf(width, height, pixelFormat, colorspace))) {
            throw AndroidExecutionException("IO_ERROR")
        }
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val rowBytes = width * bytesPerPixel
        try {
            repeat(height) { y ->
                val row = decodeVisualRawRow(pixelFormat, it.readExact(rowBytes))
                bitmap.setPixels(row, 0, width, 0, y, width, 1)
            }
            if (it.read() != -1) throw AndroidExecutionException("IO_ERROR")
            return bitmap
        } catch (error: Throwable) {
            bitmap.recycle()
            throw error
        }
    }
}

private fun ParcelFileDescriptor.duplicateInput(): java.io.InputStream =
    ParcelFileDescriptor.AutoCloseInputStream(ParcelFileDescriptor.dup(fileDescriptor))

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
