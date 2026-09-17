package com.droidbridge.android.product.automation

import java.util.UUID
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/** A public error exactly as the Runtime returned it; the UI adds no message of its own. */
data class AutomationError(
    val code: String,
    val message: String? = null,
    val details: JsonObject? = null,
)

sealed interface AutomationResult<out T> {
    data class Success<T>(val value: T) : AutomationResult<T>
    data class Failure(val error: AutomationError) : AutomationResult<Nothing>
}

data class AutomationExecutionRow(val taskId: String, val state: String)

/** One `automation.list` summary; the trigger type is filled from that row's `automation.get`. */
data class AutomationRow(
    val automationId: String,
    val name: String,
    val enabled: Boolean,
    val revision: Long,
    val updatedAt: String,
    val lastExecution: AutomationExecutionRow?,
    val triggerType: String? = null,
)

data class BulkDeleteOutcome(val deleted: Int, val failed: Int)

/** The UI's only Automation access: ordinary public `automation.*` requests over the client. */
class AutomationRepository(
    private val submit: suspend (ByteArray) -> ByteArray,
    private val requestIds: () -> String = { UUID.randomUUID().toString() },
) {
    suspend fun list(): AutomationResult<List<AutomationRow>> =
        when (val listed = call("list", buildJsonObject { put("limit", LIST_LIMIT) })) {
            is AutomationResult.Failure -> listed
            is AutomationResult.Success -> AutomationResult.Success(
                listed.value.getValue("automations").jsonArray.map { element ->
                    val row = element.jsonObject
                    AutomationRow(
                        automationId = row.string("automation_id"),
                        name = row.string("name"),
                        enabled = row.getValue("enabled").jsonPrimitive.content.toBooleanStrict(),
                        revision = requireNotNull(row.getValue("revision").jsonPrimitive.longOrNull),
                        updatedAt = row.string("updated_at"),
                        lastExecution = row["last_execution"]?.jsonObject?.let { execution ->
                            AutomationExecutionRow(execution.string("task_id"), execution.string("state"))
                        },
                    )
                },
            )
        }

    /** The saved definition of one Automation, or its canonical NOT_FOUND. */
    suspend fun get(automationId: String): AutomationResult<JsonObject> =
        when (val fetched = call("get", buildJsonObject {
            put("automation_id", automationId)
            put("history_limit", 1)
        })) {
            is AutomationResult.Failure -> fetched
            is AutomationResult.Success -> AutomationResult.Success(fetched.value.getValue("automation").jsonObject)
        }

    suspend fun save(input: JsonObject): AutomationResult<JsonObject> = call("save", input)

    suspend fun setEnabled(row: AutomationRow, enabled: Boolean): AutomationResult<JsonObject> =
        call("set_enabled", buildJsonObject {
            put("automation_id", row.automationId)
            put("enabled", enabled)
            put("expected_revision", row.revision)
        })

    suspend fun delete(automationId: String, expectedRevision: Long): AutomationResult<JsonObject> =
        call("delete", buildJsonObject {
            put("automation_id", automationId)
            put("expected_revision", expectedRevision)
        })

    /**
     * S-UI-007 delete-all: reload the complete bounded list, then one ordinary retained delete per
     * item in `(updated_at desc, automation_id desc)` order at that snapshot's revision. A revision
     * conflict is counted as a failure and never retried; nothing is claimed to be atomic.
     */
    suspend fun deleteAll(): AutomationResult<BulkDeleteOutcome> =
        when (val snapshot = list()) {
            is AutomationResult.Failure -> snapshot
            is AutomationResult.Success -> {
                var deleted = 0
                var failed = 0
                snapshot.value
                    .sortedWith(compareByDescending<AutomationRow> { it.updatedAt }.thenByDescending { it.automationId })
                    .forEach { row ->
                        when (delete(row.automationId, row.revision)) {
                            is AutomationResult.Success -> deleted += 1
                            is AutomationResult.Failure -> failed += 1
                        }
                    }
                AutomationResult.Success(BulkDeleteOutcome(deleted, failed))
            }
        }

    private suspend fun call(action: String, input: JsonObject): AutomationResult<JsonObject> {
        val envelope = buildJsonObject {
            put("protocol_version", 1)
            put("request_id", requestIds())
            put("payload", buildJsonObject {
                put("tool", "automation")
                put("action", action)
                put("input", input)
            })
        }
        val response = try {
            submit(envelope.toString().encodeToByteArray())
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            return AutomationResult.Failure(AutomationError(RUNTIME_UNAVAILABLE))
        }
        val root = try {
            Json.parseToJsonElement(response.decodeToString()).jsonObject
        } catch (_: RuntimeException) {
            return AutomationResult.Failure(AutomationError(PROTOCOL_MISMATCH))
        }
        return when ((root["outcome"] as? JsonPrimitive)?.contentOrNull) {
            "success" -> (root["result"] as? JsonObject)?.let { AutomationResult.Success(it) }
                ?: AutomationResult.Failure(AutomationError(PROTOCOL_MISMATCH))
            "error" -> (root["error"] as? JsonObject)?.let { error ->
                AutomationResult.Failure(
                    AutomationError(
                        code = (error["code"] as? JsonPrimitive)?.contentOrNull ?: PROTOCOL_MISMATCH,
                        message = (error["message"] as? JsonPrimitive)?.contentOrNull,
                        details = error["details"] as? JsonObject,
                    ),
                )
            } ?: AutomationResult.Failure(AutomationError(PROTOCOL_MISMATCH))
            else -> AutomationResult.Failure(AutomationError(PROTOCOL_MISMATCH))
        }
    }

    private fun JsonObject.string(key: String): String = getValue(key).jsonPrimitive.content

    companion object {
        /** The Contract maximum, so delete-all reloads the complete bounded list. */
        const val LIST_LIMIT = 500
        const val RUNTIME_UNAVAILABLE = "RUNTIME_UNAVAILABLE"
        const val PROTOCOL_MISMATCH = "PROTOCOL_MISMATCH"
    }
}
