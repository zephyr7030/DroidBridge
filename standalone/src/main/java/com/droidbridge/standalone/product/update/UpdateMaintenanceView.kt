package com.droidbridge.standalone.product.update

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull

/** The presentation subset of one Runtime `update-maintenance.json` record. */
data class MaintenanceRecordView(
    val updateId: String,
    val targetVersion: String,
    val phase: String,
    val installFailure: MaintenanceReply.Refused? = null,
    val awaitingConfirmation: Boolean = false,
)

/** One `getUpdateMaintenance` reply. */
data class UpdateMaintenanceView(
    val configured: Boolean,
    val installedVersionCode: Long,
    val record: MaintenanceRecordView?,
)

sealed interface MaintenanceReply {
    data class Recorded(val record: MaintenanceRecordView?) : MaintenanceReply
    data class Refused(val code: String, val stage: String? = null) : MaintenanceReply
}

object UpdateMaintenanceReplies {
    fun state(reply: String): UpdateMaintenanceView? = runCatching {
        require(reply.length <= 16 * 1024)
        val value = Json.parseToJsonElement(reply).jsonObject
        require(value.keys == setOf("schema_version", "configured", "installed_version_code", "record"))
        require(value["schema_version"] == JsonPrimitive(1))
        UpdateMaintenanceView(
            configured = value.boolean("configured"),
            installedVersionCode = requireNotNull((value.getValue("installed_version_code") as JsonPrimitive).longOrNull),
            record = record(value.getValue("record")),
        )
    }.getOrNull()

    /** A begin/install/cancel reply: the resulting record, or the Runtime's refusal code. */
    fun mutation(reply: String): MaintenanceReply = runCatching {
        require(reply.length <= 16 * 1024)
        val value = Json.parseToJsonElement(reply).jsonObject
        require(value["schema_version"] == JsonPrimitive(1))
        value["error"]?.let {
            require(value.keys in setOf(setOf("schema_version", "error"), setOf("schema_version", "error", "stage")))
            val code = (it as JsonPrimitive).also { require(it.isString) }.content
            require(validUpdateCode(code))
            val stage = value["stage"]?.let { field ->
                (field as JsonPrimitive).also { require(it.isString) }.content.also { require(validUpdateStage(it)) }
            }
            MaintenanceReply.Refused(code, stage)
        }
            ?: run {
                require(value.keys == setOf("schema_version", "record"))
                MaintenanceReply.Recorded(record(value.getValue("record")))
            }
    }.getOrElse { MaintenanceReply.Refused("RESPONSE_INVALID", "response_decode") }

    private fun record(element: kotlinx.serialization.json.JsonElement): MaintenanceRecordView? {
        if (element == JsonNull) return null
        val value = element.jsonObject
        require(value["schema_version"] in setOf(JsonPrimitive(1), JsonPrimitive(2)))
        require(value.string("update_id").matches(Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")))
        require(value.string("target_version").matches(Regex("[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}")))
        require(value.string("phase") in setOf("prepared", "apk_installing"))
        val attempt = value["last_attempt"]?.takeUnless { it == JsonNull }?.jsonObject
        return MaintenanceRecordView(
            updateId = value.string("update_id"),
            targetVersion = value.string("target_version"),
            phase = value.string("phase"),
            awaitingConfirmation = value.string("phase") == "apk_installing" &&
                attempt?.get("confirmation_handled") == JsonPrimitive(true) &&
                attempt.get("terminal_callback_seen") == JsonPrimitive(false),
            installFailure = attempt?.get("failure")?.takeUnless { it == JsonNull }?.jsonObject?.let {
                    val code = it.string("code")
                    val stage = it.string("stage")
                    require(validUpdateCode(code) && validUpdateStage(stage))
                    MaintenanceReply.Refused(code, stage)
                },
        )
    }

    private fun JsonObject.string(key: String): String = (getValue(key) as JsonPrimitive).also { require(it.isString) }.content

    private fun JsonObject.boolean(key: String): Boolean =
        requireNotNull((getValue(key) as JsonPrimitive).takeUnless { it.isString }?.booleanOrNull)
}

internal fun validUpdateCode(value: String): Boolean = value.matches(Regex("[A-Z][A-Z0-9_]{0,63}"))

internal fun validUpdateStage(value: String): Boolean = value.matches(Regex("[a-z][a-z0-9_]{0,47}"))
