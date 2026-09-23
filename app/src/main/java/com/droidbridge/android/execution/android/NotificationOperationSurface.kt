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

internal const val MAX_ACTIVE_NOTIFICATIONS = 256
internal const val MAX_NOTIFICATION_KEY_BYTES = 2_048
internal const val MAX_NOTIFICATION_TITLE_BYTES = 256
internal const val MAX_NOTIFICATION_TEXT_BYTES = 512
internal const val MAX_NOTIFICATION_ACTIONS = 32
internal const val MAX_NOTIFICATION_ACTION_TITLE_BYTES = 512

internal fun boundedUtf8(value: String?, maxBytes: Int): String? {
    value ?: return null
    if (value.encodeToByteArray().size <= maxBytes) return value
    var index = 0
    var bytes = 0
    while (index < value.length) {
        val codePoint = value.codePointAt(index)
        val encoded = when {
            codePoint <= 0x7f -> 1
            codePoint <= 0x7ff -> 2
            codePoint <= 0xffff -> 3
            else -> 4
        }
        if (bytes + encoded > maxBytes) break
        bytes += encoded
        index += Character.charCount(codePoint)
    }
    return value.substring(0, index)
}

/** One platform notification exactly as a listener callback delivered it. */
internal class ObservedNotification<Handle>(
    val key: String,
    val packageName: String,
    val postedAtMillis: Long,
    val title: String?,
    val text: String?,
    val actionCount: Int,
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
    private var nextGeneration = 1L

    @Synchronized
    fun connected(active: List<ObservedNotification<Handle>>) {
        entries.clear()
        active.asSequence()
            .mapNotNull(::normalized)
            .sortedWith(compareByDescending<ObservedNotification<Handle>> { it.postedAtMillis }.thenBy { it.key })
            .take(MAX_ACTIVE_NOTIFICATIONS)
            .forEach { entries[it.key] = Entry(allocateGeneration(), it) }
    }

    @Synchronized
    fun posted(notification: ObservedNotification<Handle>) {
        val admitted = normalized(notification) ?: return
        entries[admitted.key] = Entry(allocateGeneration(), admitted)
        if (entries.size > MAX_ACTIVE_NOTIFICATIONS) {
            val oldest = entries.values.minWithOrNull(
                compareBy<Entry<Handle>> { it.notification.postedAtMillis }
                    .thenByDescending { it.notification.key },
            )
            oldest?.let { entries.remove(it.notification.key) }
        }
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
                entries.values
                    .sortedWith(compareByDescending<Entry<Handle>> { it.notification.postedAtMillis }.thenBy { it.notification.key })
                    .forEach { entry ->
                    val notification = entry.notification
                    add(buildJsonObject {
                        put("key", notification.key)
                        put("generation", entry.generation)
                        put("package_name", notification.packageName)
                        if (notification.postedAtMillis > 0) put("posted_at_ms", notification.postedAtMillis)
                        notification.title?.let { put("title", it) }
                        notification.text?.let { put("text", it) }
                        put("action_count", notification.actionCount)
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

    private fun normalized(notification: ObservedNotification<Handle>): ObservedNotification<Handle>? {
        if (notification.key.isEmpty() ||
            notification.key.encodeToByteArray().size > MAX_NOTIFICATION_KEY_BYTES ||
            notification.packageName.isEmpty() ||
            notification.packageName.encodeToByteArray().size > 255
        ) {
            return null
        }
        return ObservedNotification(
            key = notification.key,
            packageName = notification.packageName,
            postedAtMillis = notification.postedAtMillis,
            title = boundedUtf8(notification.title, MAX_NOTIFICATION_TITLE_BYTES),
            text = boundedUtf8(notification.text, MAX_NOTIFICATION_TEXT_BYTES),
            actionCount = notification.actionCount.coerceAtLeast(notification.actions.size),
            actions = notification.actions.take(MAX_NOTIFICATION_ACTIONS + 1).map { action ->
                NotificationActionFact(
                    title = boundedUtf8(action.title, MAX_NOTIFICATION_ACTION_TITLE_BYTES),
                    requiresRemoteInput = action.requiresRemoteInput,
                )
            },
            handle = notification.handle,
        )
    }

    private fun allocateGeneration(): Long {
        if (nextGeneration == Long.MAX_VALUE) {
            entries.clear()
            throw AndroidExecutionException("RESOURCE_LIMIT")
        }
        return nextGeneration++
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
