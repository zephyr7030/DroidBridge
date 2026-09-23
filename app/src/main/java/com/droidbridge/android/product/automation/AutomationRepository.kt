package com.droidbridge.android.product.automation

import com.droidbridge.android.product.runtime.PublicCalls
import com.droidbridge.android.product.runtime.PublicError
import com.droidbridge.android.product.runtime.PublicResult
import java.util.UUID
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put


data class AutomationExecutionRow(
    val taskId: String,
    val state: String,
    val triggeredAt: String,
    val errorCode: String? = null,
)

/** One `automation.list` summary; the trigger is filled from that row's `automation.get`. */
data class AutomationRow(
    val automationId: String,
    val name: String,
    val enabled: Boolean,
    val revision: Long,
    val updatedAt: String,
    val lastExecution: AutomationExecutionRow?,
    val trigger: JsonObject? = null,
)

/** One saved definition with its newest executions first. */
data class AutomationDetail(val automation: JsonObject, val history: List<AutomationExecutionRow>) {
    val automationId: String get() = automation.getValue("automation_id").jsonPrimitive.content
    val revision: Long get() = requireNotNull(automation.getValue("revision").jsonPrimitive.longOrNull)
    val enabled: Boolean get() = automation.getValue("enabled").jsonPrimitive.content.toBooleanStrict()
}

data class BulkDeleteOutcome(val deleted: Int, val failed: Int)

/** The UI's only Automation access: ordinary public `automation.*` requests over the client. */
class AutomationRepository(
    submit: suspend (ByteArray) -> ByteArray,
    requestIds: () -> String = { UUID.randomUUID().toString() },
) {
    private val calls = PublicCalls(submit, requestIds)

    suspend fun list(): PublicResult<List<AutomationRow>> =
        when (val listed = call("list", buildJsonObject { put("limit", LIST_LIMIT) })) {
            is PublicResult.Failure -> listed
            is PublicResult.Success -> PublicResult.Success(
                listed.value.getValue("automations").jsonArray.map { element ->
                    val row = element.jsonObject
                    AutomationRow(
                        automationId = row.string("automation_id"),
                        name = row.string("name"),
                        enabled = row.getValue("enabled").jsonPrimitive.content.toBooleanStrict(),
                        revision = requireNotNull(row.getValue("revision").jsonPrimitive.longOrNull),
                        updatedAt = row.string("updated_at"),
                        lastExecution = row["last_execution"]?.jsonObject?.let(::execution),
                    )
                },
            )
        }

    /** The saved definition of one Automation and its recent runs, or its canonical NOT_FOUND. */
    suspend fun get(automationId: String, historyLimit: Int = 1): PublicResult<AutomationDetail> =
        when (val fetched = call("get", buildJsonObject {
            put("automation_id", automationId)
            put("history_limit", historyLimit)
        })) {
            is PublicResult.Failure -> fetched
            is PublicResult.Success -> PublicResult.Success(
                AutomationDetail(
                    automation = fetched.value.getValue("automation").jsonObject,
                    history = fetched.value.getValue("history").jsonArray.map { execution(it.jsonObject) },
                ),
            )
        }

    /** Asks for one run now, outside the trigger; it appears in the history once admitted. */
    suspend fun run(automationId: String): PublicResult<JsonObject> =
        call("run", buildJsonObject { put("automation_id", automationId) })

    suspend fun save(input: JsonObject): PublicResult<JsonObject> = call("save", input)

    suspend fun setEnabled(automationId: String, revision: Long, enabled: Boolean): PublicResult<JsonObject> =
        call("set_enabled", buildJsonObject {
            put("automation_id", automationId)
            put("enabled", enabled)
            put("expected_revision", revision)
        })

    suspend fun delete(automationId: String, expectedRevision: Long): PublicResult<JsonObject> =
        call("delete", buildJsonObject {
            put("automation_id", automationId)
            put("expected_revision", expectedRevision)
        })

    /**
     * S-UI-007 delete-all: reload the complete bounded list, then one ordinary retained delete per
     * item in `(updated_at desc, automation_id desc)` order at that snapshot's revision. A revision
     * conflict is counted as a failure and never retried; nothing is claimed to be atomic.
     */
    suspend fun deleteAll(): PublicResult<BulkDeleteOutcome> =
        when (val snapshot = list()) {
            is PublicResult.Failure -> snapshot
            is PublicResult.Success -> {
                var deleted = 0
                var failed = 0
                snapshot.value
                    .sortedWith(compareByDescending<AutomationRow> { it.updatedAt }.thenByDescending { it.automationId })
                    .forEach { row ->
                        when (delete(row.automationId, row.revision)) {
                            is PublicResult.Success -> deleted += 1
                            is PublicResult.Failure -> failed += 1
                        }
                    }
                PublicResult.Success(BulkDeleteOutcome(deleted, failed))
            }
        }

    private suspend fun call(action: String, input: JsonObject): PublicResult<JsonObject> =
        calls.call("automation", action, input)

    private fun JsonObject.string(key: String): String = getValue(key).jsonPrimitive.content

    private fun execution(execution: JsonObject) = AutomationExecutionRow(
        taskId = execution.string("task_id"),
        state = execution.string("state"),
        triggeredAt = execution.string("triggered_at"),
        errorCode = execution["error_code"]?.jsonPrimitive?.contentOrNull,
    )

    companion object {
        /** The Contract maximum, so delete-all reloads the complete bounded list. */
        const val LIST_LIMIT = 500
    }
}
