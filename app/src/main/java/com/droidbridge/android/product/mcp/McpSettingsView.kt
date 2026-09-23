package com.droidbridge.android.product.mcp

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject

/** The MCP revision both agent connections speak; About shows it. */
const val MCP_PROTOCOL_VERSION = "2026-07-28"

enum class McpListenerState { Stopped, Running, Failed }

/** One validated S-MCP-003 settings reply; the token is never part of it. */
data class McpSettingsView(
    val enabled: Boolean,
    val listener: McpListenerState,
    val reason: String?,
    val endpoint: String,
    val protocolVersion: String,
)

object McpSettingsReplies {
    /** Returns null for `{error}` or any reply outside the exact S-MCP-003 shape. */
    fun settings(reply: String): McpSettingsView? = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        val listener = when (value.string("listener")) {
            "stopped" -> McpListenerState.Stopped
            "running" -> McpListenerState.Running
            "failed" -> McpListenerState.Failed
            else -> error("listener")
        }
        val expected = setOf("schema_version", "enabled", "listener", "endpoint", "protocol_version") +
            if (listener == McpListenerState.Failed) setOf("reason") else emptySet()
        require(value.keys == expected && value.version())
        McpSettingsView(
            enabled = requireNotNull((value.getValue("enabled") as JsonPrimitive).takeUnless { it.isString }?.booleanOrNull),
            listener = listener,
            reason = if (listener == McpListenerState.Failed) value.string("reason") else null,
            endpoint = value.string("endpoint"),
            protocolVersion = value.string("protocol_version"),
        )
    }.getOrNull()

    /** Returns the revealed token, or null for `{error}` or a malformed reply. */
    fun token(reply: String): String? = runCatching {
        val value = Json.parseToJsonElement(reply).jsonObject
        require(value.keys == setOf("schema_version", "token") && value.version())
        value.string("token").takeIf(String::isNotEmpty)
    }.getOrNull()

    private fun JsonObject.version(): Boolean =
        (get("schema_version") as? JsonPrimitive)?.let { !it.isString && it.content == "1" } == true

    private fun JsonObject.string(name: String): String =
        (getValue(name) as JsonPrimitive).also { require(it.isString) }.content
}
