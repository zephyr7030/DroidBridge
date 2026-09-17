package com.droidbridge.android.product.maintenance

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject

enum class MaintenanceBlocker { None, OwnerCorrupt, StoreCorrupt }

/** One validated S-UI-017 `getMaintenanceState` reply. */
data class MaintenanceState(val blocker: MaintenanceBlocker, val cleanupVerified: Boolean) {
    /** MaintenanceRecovery is the bootstrap root exactly while a blocker exists. */
    val recoveryRequired: Boolean get() = blocker != MaintenanceBlocker.None

    /** A reset action is offered only for a blocker whose live-resource cleanup is verified. */
    val resetAvailable: Boolean get() = recoveryRequired && cleanupVerified
}

object MaintenanceReplies {
    fun state(reply: String): MaintenanceState? = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        require(value.keys == setOf("schema_version", "blocker", "cleanup") && value.version())
        MaintenanceState(
            blocker = when (value.string("blocker")) {
                "none" -> MaintenanceBlocker.None
                "owner_corrupt" -> MaintenanceBlocker.OwnerCorrupt
                "store_corrupt" -> MaintenanceBlocker.StoreCorrupt
                else -> error("blocker")
            },
            cleanupVerified = when (value.string("cleanup")) {
                "verified" -> true
                "unverified" -> false
                else -> error("cleanup")
            },
        )
    }.getOrNull()

    /** True exactly for the S-UI-017 `{schema_version:1,reset:true}` success reply. */
    fun resetSucceeded(reply: String): Boolean = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        value.keys == setOf("schema_version", "reset") && value.version() &&
            (value.getValue("reset") as JsonPrimitive).let { !it.isString && it.booleanOrNull == true }
    }.getOrDefault(false)

    private fun JsonObject.version(): Boolean =
        (get("schema_version") as? JsonPrimitive)?.let { !it.isString && it.content == "1" } == true

    private fun JsonObject.string(name: String): String =
        (getValue(name) as JsonPrimitive).also { require(it.isString) }.content
}
