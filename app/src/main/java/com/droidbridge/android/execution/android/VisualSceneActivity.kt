package com.droidbridge.android.execution.android

import android.os.SystemClock
import kotlinx.coroutines.delay

/**
 * When the accessibility service last saw the screen change. An observation waits here until the
 * screen has held still, so its image and nodes describe a settled scene rather than a frame of an
 * animation or a keyboard sliding in, which would be stale by the time a caller acts on it. The
 * wait is bounded: a screen that never stops changing is observed as it is. Without an enabled
 * accessibility service nothing reports changes and an observation never waits.
 */
internal class VisualSceneActivity(
    private val clock: () -> Long = SystemClock::elapsedRealtime,
    private val sleep: suspend (Long) -> Unit = { delay(it) },
) {
    @Volatile private var lastChangeAt: Long? = null

    fun changed() {
        lastChangeAt = clock()
    }

    suspend fun awaitQuiet() {
        val started = clock()
        while (true) {
            val last = lastChangeAt ?: return
            val now = clock()
            val quietFor = now - last
            val waited = now - started
            if (quietFor >= QUIET_MS || waited >= MAX_SETTLE_MS) return
            sleep(minOf(QUIET_MS - quietFor, MAX_SETTLE_MS - waited))
        }
    }

    companion object {
        /** Content events arrive at most every 100 ms, so this is two or more missed updates. */
        const val QUIET_MS = 250L
        const val MAX_SETTLE_MS = 1_000L
    }
}
