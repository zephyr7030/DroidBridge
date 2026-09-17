package com.droidbridge.android.runtimehost

internal data class CompanionCapabilityFact(
    val key: String,
    val state: String,
    val reason: String?,
    val sourceGeneration: Long,
    val hasExecutor: Boolean,
)

internal class CompanionCapabilityFacts {
    private val values = mutableMapOf<String, CompanionCapabilityFact>()

    @Synchronized
    fun register(
        key: String,
        state: String,
        reason: String,
        sourceGeneration: Long,
        hasExecutor: Boolean,
    ): Boolean {
        require(key in APP_CAPABILITY_KEYS)
        require(state in CAPABILITY_STATES)
        require(sourceGeneration > 0)
        require(state == "available" || !hasExecutor)
        require(if (state == "available") reason.isEmpty() else reason.isNotEmpty())
        val candidate = CompanionCapabilityFact(
            key = key,
            state = state,
            reason = reason.takeUnless(String::isEmpty),
            sourceGeneration = sourceGeneration,
            hasExecutor = hasExecutor,
        )
        val current = values[key]
        if (current != null && sourceGeneration < current.sourceGeneration) return false
        if (current != null && sourceGeneration == current.sourceGeneration) {
            return current == candidate
        }
        values[key] = candidate
        return true
    }

    @Synchronized
    fun snapshot(): List<CompanionCapabilityFact> = values.toSortedMap().values.toList()

    /**
     * Facts to replay into an App Runtime that became host after they were recorded. The App
     * guard is excluded because each executor instance proves its own guard (S-EXEC-001).
     */
    @Synchronized
    fun apkHostReplay(): List<CompanionCapabilityFact> =
        values.toSortedMap().values.filter { it.key != APP_GUARD_KEY }

    private companion object {
        const val APP_GUARD_KEY = "execution.app_guard"
        val CAPABILITY_STATES = setOf("available", "unavailable", "unknown")
        val APP_CAPABILITY_KEYS = setOf(
            "android.local_network",
            "android.notifications",
            "android.notification_listener",
            "automation.exact_alarm",
            "visual.accessibility",
            "visual.media_projection_session",
            "shizuku.shell",
            "execution.app_guard",
            "execution.shell_guard",
        )
    }
}
