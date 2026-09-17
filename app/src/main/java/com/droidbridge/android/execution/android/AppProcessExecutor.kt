package com.droidbridge.android.execution.android

import com.droidbridge.android.runtimehost.DaemonProtocol
import com.droidbridge.android.runtimehost.NativeRuntime
import java.nio.charset.CharacterCodingException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * The App-identity process primitive of the APK execution surface.
 *
 * The App surface owns `run_as=app`, so the guarded run happens in this process's own
 * native guard runner and is reported through the one settlement both host Command
 * surfaces decode (S-AUTH-CMD-001). This adapter only carries an already admitted
 * request to that runner: it selects no identity of its own and never falls back to
 * another runner when the request cannot be served as asked.
 */
internal class AppProcessExecutor(
    private val validatesFence: (String, Long, String) -> Boolean,
    private val runCommand: (executionId: String, requestJson: String) -> String? =
        NativeRuntime::nativeRunAppCommand,
    private val cancelCommand: (executionId: String) -> Boolean =
        NativeRuntime::nativeCancelAppCommand,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException(STALE_AUTHORITY)
        }
        return when (request.primitive) {
            AndroidPrimitive.AppProcessStart -> start(request)
            AndroidPrimitive.AppProcessCancel -> cancel(request)
            else -> throw AndroidExecutionException(UNSUPPORTED)
        }
    }

    private fun start(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (request.descriptors.isNotEmpty()) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        if (request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException(RESOURCE_LIMIT)
        }
        val encoded = runCommand(request.executionId, strictUtf8(request.payload))
            ?: throw AndroidExecutionException(INTERNAL_ERROR)
        val value = decode(encoded)
        val error = value["error"]?.jsonObject
            ?: return AndroidExecutionResult(encoded.encodeToByteArray())
        val code = error["code"]?.jsonPrimitive?.content
        throw AndroidExecutionException(code ?: INTERNAL_ERROR)
    }

    private fun cancel(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (request.descriptors.isNotEmpty()) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        if (request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException(RESOURCE_LIMIT)
        }
        val payload = decode(strictUtf8(request.payload))
        if (payload.keys != CANCELLATION_KEYS) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        val target = payload["execution_id"]?.jsonPrimitive?.content
            ?: throw AndroidExecutionException(INVALID_ARGUMENT)
        if (!DaemonProtocol.isUuid(target)) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        val encoded = json.encodeToString(
            JsonObject.serializer(),
            buildJsonObject { put("cancelled", cancelCommand(target)) },
        )
        return AndroidExecutionResult(encoded.encodeToByteArray())
    }

    /**
     * A reported command result that does not decode is not evidence about the run, so
     * it is reported as the one code the command family reads as an unverified cleanup
     * rather than as a settled command.
     */
    private fun decode(encoded: String): JsonObject =
        runCatching { json.parseToJsonElement(encoded).jsonObject }.getOrNull()
            ?: throw AndroidExecutionException(INTERNAL_ERROR)

    private fun strictUtf8(payload: ByteArray): String = try {
        payload.decodeToString(throwOnInvalidSequence = true)
    } catch (_: CharacterCodingException) {
        throw AndroidExecutionException(INVALID_ARGUMENT)
    }

    private companion object {
        const val MAX_PAYLOAD_BYTES = 65_536
        const val INVALID_ARGUMENT = "INVALID_ARGUMENT"
        const val STALE_AUTHORITY = "STALE_AUTHORITY"
        const val RESOURCE_LIMIT = "RESOURCE_LIMIT"
        const val UNSUPPORTED = "UNSUPPORTED"
        const val INTERNAL_ERROR = "INTERNAL_ERROR"
        val json = Json { ignoreUnknownKeys = false }
        val CANCELLATION_KEYS = setOf("execution_id")
    }
}
