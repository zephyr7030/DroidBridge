package com.droidbridge.android.execution.android

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal data class VisualCodecFact(
    val available: Boolean,
    val reason: String? = null,
    val codecName: String? = null,
)

internal fun interface VisualCodecProbe {
    fun inspect(width: Int, height: Int): VisualCodecFact
}

internal data class VisualCodecHealthSnapshot(
    val available: Boolean,
    val reason: String?,
    val generation: Long,
)

internal class VisualCodecHealth(private val probe: VisualCodecProbe) {
    private var generation = 1L
    private val admittedDimensions = mutableMapOf<Pair<Int, Int>, String>()

    @Synchronized
    fun snapshot(width: Int, height: Int): VisualCodecHealthSnapshot {
        val fact = probe.inspect(width, height)
        if (fact.available) {
            val codecName = fact.codecName ?: throw AndroidExecutionException("IO_ERROR")
            admittedDimensions[width to height] = codecName
        } else {
            admittedDimensions.remove(width to height)
        }
        return VisualCodecHealthSnapshot(fact.available, fact.reason, generation)
    }

    @Synchronized
    fun accepts(candidateGeneration: Long, width: Int, height: Int): Boolean =
        candidateGeneration == generation && width to height in admittedDimensions

    @Synchronized
    fun candidate(candidateGeneration: Long, width: Int, height: Int): String? =
        admittedDimensions[width to height].takeIf { candidateGeneration == generation }

    @Synchronized
    fun invalidate(candidateGeneration: Long) {
        if (candidateGeneration != generation) return
        if (generation == Long.MAX_VALUE) throw AndroidExecutionException("RESOURCE_LIMIT")
        generation += 1
        admittedDimensions.clear()
    }

    @Synchronized
    fun invalidateCurrent() {
        if (generation == Long.MAX_VALUE) throw AndroidExecutionException("RESOURCE_LIMIT")
        generation += 1
        admittedDimensions.clear()
    }
}

internal class AndroidHeicCodecProbe : VisualCodecProbe {
    override fun inspect(width: Int, height: Int): VisualCodecFact {
        val candidates = MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos.filter { info ->
            info.isEncoder && info.isHardwareAccelerated && !info.isSoftwareOnly &&
                info.supportedTypes.any { it.equals(HEIC_MIME, ignoreCase = true) }
        }
        if (candidates.isEmpty()) return VisualCodecFact(false, "NO_HARDWARE_HEIC_ENCODER")
        if (width % 2 != 0 || height % 2 != 0) {
            return VisualCodecFact(false, "DIMENSIONS_UNSUPPORTED")
        }
        val colorFormat = MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible
        var sawCapabilities = false
        var sawDimensions = false
        var sawInputMode = false
        var sawQualityMode = false
        candidates.forEach { candidate ->
            val capabilities = runCatching { candidate.getCapabilitiesForType(HEIC_MIME) }
                .getOrNull() ?: return@forEach
            sawCapabilities = true
            if (capabilities.videoCapabilities?.isSizeSupported(width, height) != true) {
                return@forEach
            }
            sawDimensions = true
            if (!capabilities.colorFormats.contains(colorFormat)) return@forEach
            sawInputMode = true
            val encoder = capabilities.encoderCapabilities ?: return@forEach
            if (!encoder.isBitrateModeSupported(MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CQ) ||
                !encoder.qualityRange.contains(REQUIRED_QUALITY)
            ) {
                return@forEach
            }
            sawQualityMode = true
            val configured = runCatching {
                val codec = MediaCodec.createByCodecName(candidate.name)
                try {
                    val format = visualHeicFormat(width, height, colorFormat, capabilities)
                    codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                    codec.start()
                    codec.stop()
                } finally {
                    codec.release()
                }
            }.isSuccess
            if (configured) {
                return VisualCodecFact(true, codecName = candidate.name)
            }
        }
        return VisualCodecFact(
            false,
            when {
                !sawCapabilities -> "CAPABILITIES_UNAVAILABLE"
                !sawDimensions -> "DIMENSIONS_UNSUPPORTED"
                !sawInputMode -> "INPUT_MODE_UNSUPPORTED"
                !sawQualityMode -> "QUALITY_MODE_UNSUPPORTED"
                else -> "CONFIGURATION_FAILED"
            },
        )
    }

    private companion object {
        const val HEIC_MIME = "image/vnd.android.heic"
        const val REQUIRED_QUALITY = 90
    }
}

internal fun visualHeicFormat(
    width: Int,
    height: Int,
    colorFormat: Int,
    capabilities: MediaCodecInfo.CodecCapabilities,
): MediaFormat =
    MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_IMAGE_ANDROID_HEIC, width, height).apply {
        setInteger(MediaFormat.KEY_COLOR_FORMAT, colorFormat)
        setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 0)
        setInteger(MediaFormat.KEY_FRAME_RATE, 1)
        setInteger(MediaFormat.KEY_OPERATING_RATE, 30)
        val encoder = capabilities.encoderCapabilities
            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        if (!encoder.isBitrateModeSupported(MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CQ) ||
            !encoder.qualityRange.contains(REQUIRED_HEIC_QUALITY)
        ) {
            throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        }
        setInteger(
            MediaFormat.KEY_BITRATE_MODE,
            MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CQ,
        )
        setInteger(MediaFormat.KEY_QUALITY, REQUIRED_HEIC_QUALITY)
    }

internal class VisualCodecSnapshotAdapter(
    private val health: VisualCodecHealth,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    constructor(
        probe: VisualCodecProbe,
        validatesFence: (String, Long, String) -> Boolean,
    ) : this(VisualCodecHealth(probe), validatesFence)

    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (request.primitive != AndroidPrimitive.VisualCodecSnapshot) {
            throw AndroidExecutionException("UNSUPPORTED")
        }
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.descriptors.isNotEmpty()) throw AndroidExecutionException("INVALID_ARGUMENT")
        if (request.payload.size > 1_048_576) throw AndroidExecutionException("RESOURCE_LIMIT")
        val input = runCatching {
            Json.parseToJsonElement(
                request.payload.decodeToString(throwOnInvalidSequence = true),
            ).jsonObject
        }.getOrElse { throw AndroidExecutionException("INVALID_ARGUMENT") }
        if (input.keys != INPUT_KEYS || input["source"]?.jsonPrimitive?.contentOrNull != "privileged_raw") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val width = input["width"]?.jsonPrimitive?.intOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        val height = input["height"]?.jsonPrimitive?.intOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        if (width !in 1..16_384 || height !in 1..16_384) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val fact = try {
            withTimeout(10_000) { withContext(Dispatchers.IO) { health.snapshot(width, height) } }
        } catch (_: kotlinx.coroutines.TimeoutCancellationException) {
            health.invalidateCurrent()
            throw AndroidExecutionException("TIMEOUT")
        }
        val result = buildJsonObject {
            put("hardware_heic", if (fact.available) "available" else "unavailable")
            put("codec_generation", fact.generation)
            if (!fact.available) put("reason", fact.reason ?: "PROBE_UNAVAILABLE")
        }
        return AndroidExecutionResult(result.toString().encodeToByteArray())
    }

    private companion object {
        val INPUT_KEYS = setOf("width", "height", "source")
    }
}

private const val REQUIRED_HEIC_QUALITY = 90
