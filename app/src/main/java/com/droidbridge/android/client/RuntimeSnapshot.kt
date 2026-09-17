package com.droidbridge.android.client

import org.json.JSONObject

enum class AvailabilityState {
    Available,
    Unavailable,
    Unknown,
}

data class AvailabilityFact(
    val state: AvailabilityState,
    val reason: String? = null,
)

enum class RuntimeReadiness {
    Initializing,
    Ready,
    Unavailable,
}

data class RuntimeSnapshot(
    val sdkInt: Int,
    val host: String,
    val hostGeneration: Long,
    val readiness: RuntimeReadiness,
    val runtimeReason: String?,
    val grants: Map<String, AvailabilityFact>,
    val capabilities: Map<String, AvailabilityFact>,
    /** Full-status `compatibility` values; empty when the refresh returned no such object. */
    val compatibility: Map<String, String> = emptyMap(),
) {
    companion object {
        fun failureReason(envelope: ByteArray): String? = runCatching {
            when (JSONObject(envelope.toString(Charsets.UTF_8)).getJSONObject("error").getString("code")) {
                "IO_ERROR" -> "STORE_UNAVAILABLE"
                "PROTOCOL_INCOMPATIBLE" -> "PROTOCOL_MISMATCH"
                else -> "RUNTIME_UNAVAILABLE"
            }
        }.getOrNull()

        fun fromResponse(envelope: ByteArray): RuntimeSnapshot {
            val root = JSONObject(envelope.toString(Charsets.UTF_8))
            require(root.getString("outcome") == "success")
            val result = root.getJSONObject("result")
            val runtime = result.getJSONObject("runtime")
            return RuntimeSnapshot(
                sdkInt = result.getJSONObject("device").getInt("sdk_int"),
                host = runtime.getString("host"),
                hostGeneration = runtime.getLong("host_generation"),
                readiness = when (runtime.getString("readiness")) {
                    "ready" -> RuntimeReadiness.Ready
                    "unavailable" -> RuntimeReadiness.Unavailable
                    else -> RuntimeReadiness.Initializing
                },
                runtimeReason = runtime.optString("reason").takeIf(String::isNotEmpty),
                grants = facts(result.getJSONObject("grants")),
                capabilities = facts(result.getJSONObject("capabilities")),
                compatibility = result.optJSONObject("compatibility")?.let { value ->
                    buildMap { value.keys().forEach { key -> put(key, value.getString(key)) } }
                }.orEmpty(),
            )
        }

        private fun facts(value: JSONObject): Map<String, AvailabilityFact> = buildMap {
            value.keys().forEach { key ->
                val fact = value.getJSONObject(key)
                put(
                    key,
                    AvailabilityFact(
                        state = when (fact.getString("state")) {
                            "available" -> AvailabilityState.Available
                            "unavailable" -> AvailabilityState.Unavailable
                            else -> AvailabilityState.Unknown
                        },
                        reason = fact.optString("reason").takeIf(String::isNotEmpty),
                    ),
                )
            }
        }
    }
}

sealed interface ClientState {
    data object Disconnected : ClientState
    data object Connecting : ClientState
    data class Available(
        val snapshot: RuntimeSnapshot,
        val refreshing: Boolean = false,
    ) : ClientState
    data class Unavailable(val reason: String) : ClientState
}
