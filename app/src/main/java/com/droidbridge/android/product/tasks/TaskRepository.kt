package com.droidbridge.android.product.tasks

import com.droidbridge.android.product.runtime.PublicCalls
import com.droidbridge.android.product.runtime.PublicError
import com.droidbridge.android.product.runtime.PublicResult
import java.util.UUID
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


/** The UI's only Task access: ordinary public `task_control.*` requests over the client. */
class TaskRepository(
    submit: suspend (ByteArray) -> ByteArray,
    requestIds: () -> String = { UUID.randomUUID().toString() },
) {
    private val calls = PublicCalls(submit, requestIds)

    suspend fun list(filter: TaskFilter, limit: Int = DEFAULT_LIMIT): PublicResult<List<TaskSummary>> {
        val input = buildJsonObject {
            put("limit", limit)
            put("states", buildJsonArray { filter.states.forEach { add(JsonPrimitive(it)) } })
        }
        return when (val listed = call("list", input)) {
            is PublicResult.Failure -> listed
            is PublicResult.Success -> parse {
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

    suspend fun get(taskId: String): PublicResult<TaskSnapshot> = snapshot("get", taskId)

    suspend fun cancel(taskId: String): PublicResult<TaskSnapshot> = snapshot("cancel", taskId)

    private suspend fun snapshot(action: String, taskId: String): PublicResult<TaskSnapshot> =
        when (val fetched = call(action, buildJsonObject { put("task_id", taskId) })) {
            is PublicResult.Failure -> fetched
            is PublicResult.Success -> parse {
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

    private suspend fun call(action: String, input: JsonObject): PublicResult<JsonObject> =
        calls.call("task_control", action, input)

    private inline fun <T> parse(block: () -> T): PublicResult<T> =
        try {
            PublicResult.Success(block())
        } catch (_: RuntimeException) {
            PublicResult.Failure(PublicError(PublicCalls.PROTOCOL_MISMATCH))
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
