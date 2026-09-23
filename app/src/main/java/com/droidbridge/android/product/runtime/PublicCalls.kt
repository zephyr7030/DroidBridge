package com.droidbridge.android.product.runtime

import java.util.UUID
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

/** A public error exactly as the Runtime returned it; the UI adds no message of its own. */
data class PublicError(
    val code: String,
    val message: String? = null,
    val details: JsonObject? = null,
)

sealed interface PublicResult<out T> {
    data class Success<T>(val value: T) : PublicResult<T>
    data class Failure(val error: PublicError) : PublicResult<Nothing>
}

/**
 * The one public request the App sends: `{protocol_version, request_id, payload{tool, action,
 * input}}` over the client, and the one way its answer is read. A transport that fails and a
 * response that is not a Contract envelope are the only errors this layer invents.
 */
class PublicCalls(
    private val submit: suspend (ByteArray) -> ByteArray,
    private val requestIds: () -> String = { UUID.randomUUID().toString() },
) {
    suspend fun call(tool: String, action: String, input: JsonObject): PublicResult<JsonObject> {
        val envelope = buildJsonObject {
            put("protocol_version", PROTOCOL_VERSION)
            put("request_id", requestIds())
            put("payload", buildJsonObject {
                put("tool", tool)
                put("action", action)
                put("input", input)
            })
        }
        val response = try {
            submit(envelope.toString().encodeToByteArray())
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            return failure(RUNTIME_UNAVAILABLE)
        }
        val root = try {
            Json.parseToJsonElement(response.decodeToString()).jsonObject
        } catch (_: RuntimeException) {
            return failure(PROTOCOL_MISMATCH)
        }
        return when (root.text("outcome")) {
            "success" -> (root["result"] as? JsonObject)?.let { PublicResult.Success(it) } ?: failure(PROTOCOL_MISMATCH)
            "error" -> (root["error"] as? JsonObject)?.let { error ->
                PublicResult.Failure(
                    PublicError(
                        code = error.text("code") ?: PROTOCOL_MISMATCH,
                        message = error.text("message"),
                        details = error["details"] as? JsonObject,
                    ),
                )
            } ?: failure(PROTOCOL_MISMATCH)
            else -> failure(PROTOCOL_MISMATCH)
        }
    }

    private fun failure(code: String) = PublicResult.Failure(PublicError(code))

    private fun JsonObject.text(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull

    companion object {
        const val PROTOCOL_VERSION = 1
        const val RUNTIME_UNAVAILABLE = "RUNTIME_UNAVAILABLE"
        const val PROTOCOL_MISMATCH = "PROTOCOL_MISMATCH"
    }
}
