package com.droidbridge.android.execution.android

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

internal data class NotificationActionFact(
    val title: String?,
    val requiresRemoteInput: Boolean,
)

/** One platform notification exactly as a listener callback delivered it. */
internal class ObservedNotification<Handle>(
    val key: String,
    val packageName: String,
    val postedAtMillis: Long,
    val title: String?,
    val text: String?,
    val actions: List<NotificationActionFact>,
    val handle: Handle,
)

internal interface NotificationPlatform<Handle> {
    fun cancel(key: String)

    fun sendAction(handle: Handle, index: Int)
}

/**
 * S-ANDROID-005 typed notification operations. Each active key carries the callback
 * generation of the exact notification object last delivered for it; dismiss/action
 * revalidate that generation under the same lock that callbacks use, so a replacement can
 * never be targeted through an older reference.
 */
internal class NotificationOperationSurface<Handle>(
    private val platform: NotificationPlatform<Handle>,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    private class Entry<Handle>(val generation: Long, val notification: ObservedNotification<Handle>)

    private val entries = LinkedHashMap<String, Entry<Handle>>()

    @Synchronized
    fun connected(active: List<ObservedNotification<Handle>>) {
        entries.clear()
        active.forEach { entries[it.key] = Entry(1, it) }
    }

    @Synchronized
    fun posted(notification: ObservedNotification<Handle>) {
        val generation = (entries[notification.key]?.generation ?: 0L) + 1L
        entries[notification.key] = Entry(generation, notification)
    }

    @Synchronized
    fun removed(key: String) {
        entries.remove(key)
    }

    @Synchronized
    fun disconnected() {
        entries.clear()
    }

    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException(STALE_AUTHORITY)
        }
        if (request.descriptors.isNotEmpty() || request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        val input = parseObject(request.payload)
        return when (request.primitive) {
            AndroidPrimitive.NotificationSnapshot -> {
                requireKeys(input, emptySet())
                snapshot()
            }
            AndroidPrimitive.NotificationDismiss -> {
                requireKeys(input, setOf("key", "generation"))
                dismiss(input.key(), input.generation())
            }
            AndroidPrimitive.NotificationAction -> {
                requireKeys(input, setOf("key", "generation", "action_index"))
                val index = input["action_index"]?.jsonPrimitive?.intOrNull
                    ?.takeIf { it in 0..31 }
                    ?: throw AndroidExecutionException(INVALID_ARGUMENT)
                invoke(input.key(), input.generation(), index)
            }
            else -> throw AndroidExecutionException(UNSUPPORTED)
        }
    }

    @Synchronized
    private fun snapshot(): AndroidExecutionResult {
        val encoded = buildJsonObject {
            put("notifications", buildJsonArray {
                entries.values.forEach { entry ->
                    val notification = entry.notification
                    add(buildJsonObject {
                        put("key", notification.key)
                        put("generation", entry.generation)
                        put("package_name", notification.packageName)
                        if (notification.postedAtMillis > 0) put("posted_at_ms", notification.postedAtMillis)
                        notification.title?.let { put("title", it) }
                        notification.text?.let { put("text", it) }
                        put("actions", buildJsonArray {
                            notification.actions.forEach { action ->
                                add(buildJsonObject {
                                    action.title?.let { put("title", it) }
                                    put("requires_remote_input", action.requiresRemoteInput)
                                })
                            }
                        })
                    })
                }
            })
        }
        return AndroidExecutionResult(encoded.toString().encodeToByteArray())
    }

    @Synchronized
    private fun dismiss(key: String, generation: Long): AndroidExecutionResult {
        current(key, generation)
        platform.cancel(key)
        return completed()
    }

    @Synchronized
    private fun invoke(key: String, generation: Long, index: Int): AndroidExecutionResult {
        val notification = current(key, generation)
        val action = notification.actions.getOrNull(index)
            ?: throw AndroidExecutionException(INVALID_ARGUMENT)
        if (action.requiresRemoteInput) throw AndroidExecutionException(UNSUPPORTED)
        platform.sendAction(notification.handle, index)
        return completed()
    }

    private fun current(key: String, generation: Long): ObservedNotification<Handle> {
        val entry = entries[key]
        if (entry == null || entry.generation != generation) {
            throw AndroidExecutionException(STALE_REFERENCE)
        }
        return entry.notification
    }

    private fun completed() = AndroidExecutionResult(COMPLETED)

    private fun parseObject(payload: ByteArray): JsonObject = try {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    } catch (_: IllegalArgumentException) {
        throw AndroidExecutionException(INVALID_ARGUMENT)
    } catch (_: CharacterCodingException) {
        throw AndroidExecutionException(INVALID_ARGUMENT)
    }

    private fun requireKeys(input: JsonObject, allowed: Set<String>) {
        if (input.keys != allowed) throw AndroidExecutionException(INVALID_ARGUMENT)
    }

    private fun JsonObject.key(): String = this["key"]?.jsonPrimitive
        ?.takeIf { it.isString }
        ?.contentOrNull
        ?.takeIf(String::isNotEmpty)
        ?: throw AndroidExecutionException(INVALID_ARGUMENT)

    private fun JsonObject.generation(): Long = this["generation"]?.jsonPrimitive?.longOrNull
        ?.takeIf { it > 0 }
        ?: throw AndroidExecutionException(INVALID_ARGUMENT)

    private companion object {
        const val MAX_PAYLOAD_BYTES = 65_536
        const val STALE_AUTHORITY = "STALE_AUTHORITY"
        const val STALE_REFERENCE = "STALE_REFERENCE"
        const val INVALID_ARGUMENT = "INVALID_ARGUMENT"
        const val UNSUPPORTED = "UNSUPPORTED"
        val COMPLETED = "{\"completed\":true}".encodeToByteArray()
    }
}
