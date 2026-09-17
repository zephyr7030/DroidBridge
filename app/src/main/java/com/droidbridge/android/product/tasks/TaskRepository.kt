package com.droidbridge.android.product.tasks

import java.util.UUID
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

/** A public error code exactly as the Runtime returned it; the UI adds no message of its own. */
data class TaskError(val code: String)

sealed interface TaskResult<out T> {
    data class Success<T>(val value: T) : TaskResult<T>
    data class Failure(val error: TaskError) : TaskResult<Nothing>
}

/** The UI's only Task access: ordinary public `task_control.*` requests over the client. */
class TaskRepository(
    private val submit: suspend (ByteArray) -> ByteArray,
    private val requestIds: () -> String = { UUID.randomUUID().toString() },
) {
    suspend fun list(filter: TaskFilter, limit: Int = DEFAULT_LIMIT): TaskResult<List<TaskSummary>> {
        val input = buildJsonObject {
            put("limit", limit)
            filter.states?.let { states -> put("states", buildJsonArray { states.forEach { add(JsonPrimitive(it)) } }) }
        }
        return when (val listed = call("list", input)) {
            is TaskResult.Failure -> listed
            is TaskResult.Success -> parse {
                listed.value.getValue("tasks").jsonArray.map { element ->
                    val row = element.jsonObject
                    TaskSummary(
                        taskId = row.string("task_id"),
                        state = row.string("state"),
                        tool = row.string("tool"),
                        action = row.string("action"),
                        createdAt = row.string("created_at"),
                        startedAt = row.optionalString("started_at"),
                        endedAt = row.optionalString("ended_at"),
                    )
                }
            }
        }
    }

    suspend fun get(taskId: String): TaskResult<TaskSnapshot> = snapshot("get", taskId)

    suspend fun cancel(taskId: String): TaskResult<TaskSnapshot> = snapshot("cancel", taskId)

    private suspend fun snapshot(action: String, taskId: String): TaskResult<TaskSnapshot> =
        when (val fetched = call(action, buildJsonObject { put("task_id", taskId) })) {
            is TaskResult.Failure -> fetched
            is TaskResult.Success -> parse {
                val row = fetched.value
                TaskSnapshot(
                    taskId = row.string("task_id"),
                    state = row.string("state"),
                    tool = row.string("tool"),
                    action = row.string("action"),
                    createdAt = row.string("created_at"),
                    startedAt = row.optionalString("started_at"),
                    endedAt = row.optionalString("ended_at"),
                    executionClass = row.optionalString("execution_class"),
                    cancelRequested = requireNotNull((row.getValue("cancel_requested") as JsonPrimitive).booleanOrNull),
                    result = row.optionalElement("result"),
                    error = row.optionalElement("error"),
                )
            }
        }

    private suspend fun call(action: String, input: JsonObject): TaskResult<JsonObject> {
        val envelope = buildJsonObject {
            put("protocol_version", 1)
            put("request_id", requestIds())
            put("payload", buildJsonObject {
                put("tool", "task_control")
                put("action", action)
                put("input", input)
            })
        }
        val response = try {
            submit(envelope.toString().encodeToByteArray())
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            return TaskResult.Failure(TaskError(RUNTIME_UNAVAILABLE))
        }
        val root = try {
            Json.parseToJsonElement(response.decodeToString()).jsonObject
        } catch (_: RuntimeException) {
            return TaskResult.Failure(TaskError(PROTOCOL_MISMATCH))
        }
        return when ((root["outcome"] as? JsonPrimitive)?.contentOrNull) {
            "success" -> (root["result"] as? JsonObject)?.let { TaskResult.Success(it) }
            "error" -> ((root["error"] as? JsonObject)?.get("code") as? JsonPrimitive)?.contentOrNull
                ?.let { TaskResult.Failure(TaskError(it)) }
            else -> null
        } ?: TaskResult.Failure(TaskError(PROTOCOL_MISMATCH))
    }

    private inline fun <T> parse(block: () -> T): TaskResult<T> =
        try {
            TaskResult.Success(block())
        } catch (_: RuntimeException) {
            TaskResult.Failure(TaskError(PROTOCOL_MISMATCH))
        }

    private fun JsonObject.string(key: String): String =
        (getValue(key) as JsonPrimitive).also { require(it.isString) }.content

    private fun JsonObject.optionalString(key: String): String? =
        (get(key) as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content

    private fun JsonObject.optionalElement(key: String): JsonElement? =
        get(key)?.takeUnless { it is JsonNull }

    companion object {
        const val DEFAULT_LIMIT = 100
        const val RUNTIME_UNAVAILABLE = "RUNTIME_UNAVAILABLE"
        const val PROTOCOL_MISMATCH = "PROTOCOL_MISMATCH"
    }
}
