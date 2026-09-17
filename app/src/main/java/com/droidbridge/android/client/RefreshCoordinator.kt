package com.droidbridge.android.client

internal class RefreshCoordinator(
    private val minimumIntervalMs: Long = 100,
) {
    private var active = false
    private var dirty = false
    private var lastStartMs: Long? = null

    @Synchronized
    fun hint(nowMs: Long): Long? {
        if (active) {
            dirty = true
            return null
        }
        active = true
        return delayUntilNext(nowMs)
    }

    @Synchronized
    fun started(nowMs: Long) {
        lastStartMs = nowMs
    }

    @Synchronized
    fun finished(nowMs: Long): Long? {
        if (!dirty) {
            active = false
            return null
        }
        dirty = false
        return delayUntilNext(nowMs)
    }

    @Synchronized
    fun disconnected() {
        active = false
        dirty = false
    }

    private fun delayUntilNext(nowMs: Long): Long {
        val earliest = (lastStartMs ?: Long.MIN_VALUE).let { previous ->
            if (previous == Long.MIN_VALUE) nowMs else previous + minimumIntervalMs
        }
        return (earliest - nowMs).coerceAtLeast(0)
    }
}
