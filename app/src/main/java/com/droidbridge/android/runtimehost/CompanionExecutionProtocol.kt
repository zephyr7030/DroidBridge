package com.droidbridge.android.runtimehost

import com.droidbridge.android.execution.android.AndroidPrimitive
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * One S-IPC-DAEMON-004 `CompanionExecute` call: exactly one already-admitted
 * S-ANDROID-001 primitive request plus its execution id, per S-HANDOFF-014.
 */
internal class CompanionExecution(
    val primitive: AndroidPrimitive,
    val payload: ByteArray,
    val executionId: String,
)

private val COMPANION_EXECUTION_KEYS = setOf("primitive", "payload", "execution_id")

internal fun decodeCompanionExecution(body: JsonElement): CompanionExecution? {
    val value = body as? JsonObject ?: return null
    if (value.keys != COMPANION_EXECUTION_KEYS) return null
    val primitiveName = value["primitive"]?.primitiveContentOrNull() ?: return null
    val primitive = AndroidPrimitive.entries.firstOrNull { it.name == primitiveName } ?: return null
    val executionId = value["execution_id"]?.primitiveContentOrNull() ?: return null
    if (!DaemonProtocol.isUuid(executionId)) return null
    val payload = value["payload"] ?: return null
    return CompanionExecution(primitive, payload.toString().encodeToByteArray(), executionId)
}

/**
 * One S-IPC-DAEMON-004 `CompanionCancel` call: the identity's own cancel primitive plus
 * the execution it has to reach, per S-HANDOFF-014.
 */
internal class CompanionCancellation(
    val primitive: AndroidPrimitive,
    val executionId: String,
)

private val COMPANION_CANCEL_KEYS = setOf("primitive", "execution_id")

/**
 * The only primitives a cancellation may name. Each belongs to exactly one identity, so a
 * cancellation can never be turned into a signal another identity's runner would receive.
 */
private val COMPANION_CANCEL_PRIMITIVES = setOf(
    AndroidPrimitive.AppProcessCancel,
    AndroidPrimitive.ShizukuProcessCancel,
)

internal fun decodeCompanionCancellation(body: JsonElement): CompanionCancellation? {
    val value = body as? JsonObject ?: return null
    if (value.keys != COMPANION_CANCEL_KEYS) return null
    val primitiveName = value["primitive"]?.primitiveContentOrNull() ?: return null
    val primitive = AndroidPrimitive.entries.firstOrNull { it.name == primitiveName } ?: return null
    if (primitive !in COMPANION_CANCEL_PRIMITIVES) return null
    val executionId = value["execution_id"]?.primitiveContentOrNull() ?: return null
    if (!DaemonProtocol.isUuid(executionId)) return null
    return CompanionCancellation(primitive, executionId)
}

/**
 * The typed success result of a companion execution, or `null` when the executor's
 * payload is not a JSON document and cannot be carried on the wire.
 */
internal fun companionResultPayload(payload: ByteArray): JsonElement? {
    val decoded = runCatching {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true))
    }.getOrNull() ?: return null
    return buildJsonObject { put("payload", decoded) }
}

internal fun companionFailurePayload(code: String): JsonElement = buildJsonObject {
    put(
        "error",
        buildJsonObject {
            put(
                "code",
                (DaemonErrorToken.entries.firstOrNull { it.wire == code }
                    ?: DaemonErrorToken.InternalError).wire,
            )
            put("retryable", false)
        },
    )
}

/**
 * The instance the companion reply must repeat. An instance-fenced operation is only
 * answerable by the live Runtime instance that received it.
 */
internal fun companionResponseInstance(
    request: DaemonWireEnvelope,
    owner: DaemonOwnerFence,
): String? {
    if (request.operation.instanceFenced) {
        require(
            owner.runtimeInstanceId != null &&
                owner.runtimeInstanceId == request.runtimeInstanceId,
        ) { "companion request does not carry the live Runtime instance" }
    }
    return owner.runtimeInstanceId
}

/**
 * The role labels of a result's descriptors, or `null` when the executor labelled one
 * with a role the daemon wire contract cannot carry (S-ANDROID-002).
 */
internal fun companionResultRoles(roles: List<String>): List<String>? =
    roles.takeIf { labels -> labels.all(DaemonProtocol::isCanonicalDescriptorRole) }

private fun JsonElement.primitiveContentOrNull(): String? =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content
