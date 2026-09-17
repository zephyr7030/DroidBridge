package com.droidbridge.android.product.update

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
    val productUpdate: Boolean,
    val targetVersion: String,
    val phase: String,
    val requiresModule: Boolean,
    val packageInstaller: Boolean,
    /** A daemon-owned install attempt is recorded and not yet settled. */
    val nativeAttemptActive: Boolean,
) {
    val moduleFile: String get() = "droidbridge-magisk-$targetVersion.zip"
}

/** One `getUpdateMaintenance` reply. */
data class UpdateMaintenanceView(
    val configured: Boolean,
    val module: ModulePresence,
    val privilegedInstall: Boolean,
    val installedVersionCode: Long,
    val record: MaintenanceRecordView?,
)

sealed interface MaintenanceReply {
    data class Recorded(val record: MaintenanceRecordView?) : MaintenanceReply
    data class Refused(val code: String) : MaintenanceReply
}

object UpdateMaintenanceReplies {
    fun state(reply: String): UpdateMaintenanceView? = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        require(value.keys == setOf("schema_version", "configured", "module", "privileged_install", "installed_version_code", "record"))
        UpdateMaintenanceView(
            configured = value.boolean("configured"),
            module = when (value.string("module")) {
                "compatible" -> ModulePresence.Compatible
                "absent" -> ModulePresence.Absent
                "mismatched" -> ModulePresence.Mismatched
                "excluded" -> ModulePresence.Excluded
                else -> error("module")
            },
            privilegedInstall = value.boolean("privileged_install"),
            installedVersionCode = requireNotNull((value.getValue("installed_version_code") as JsonPrimitive).longOrNull),
            record = record(value.getValue("record")),
        )
    }.getOrNull()

    /** A begin/install/cancel/exit reply: the resulting record, or the Runtime's refusal code. */
    fun mutation(reply: String): MaintenanceReply = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        value["error"]?.let { MaintenanceReply.Refused((it as JsonPrimitive).content) }
            ?: MaintenanceReply.Recorded(record(value.getValue("record")))
    }.getOrElse { MaintenanceReply.Refused("INTERNAL_ERROR") }

    private fun record(element: kotlinx.serialization.json.JsonElement): MaintenanceRecordView? {
        if (element == JsonNull) return null
        val value = element.jsonObject
        return MaintenanceRecordView(
            updateId = value.string("update_id"),
            productUpdate = value.string("kind") == "product_update",
            targetVersion = value.string("target_version"),
            phase = value.string("phase"),
            requiresModule = value.boolean("requires_module"),
            packageInstaller = (value["apk_install_provider"] as? JsonPrimitive)?.takeIf { it.isString }?.content == "package_installer",
            nativeAttemptActive = value["maintenance_execution_id"].let { it != null && it != JsonNull },
        )
    }

    private fun JsonObject.string(key: String): String = (getValue(key) as JsonPrimitive).also { require(it.isString) }.content

    private fun JsonObject.boolean(key: String): Boolean =
        requireNotNull((getValue(key) as JsonPrimitive).takeUnless { it.isString }?.booleanOrNull)
}
