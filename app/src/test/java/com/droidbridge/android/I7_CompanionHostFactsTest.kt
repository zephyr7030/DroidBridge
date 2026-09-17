package com.droidbridge.android

import com.droidbridge.android.runtimehost.CompanionCapabilityFacts
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class I7_CompanionHostFactsTest {
    @Test
    fun I7_G05_appFactsRecordedUnderMagiskReplayIntoTheNextAppHostWithoutTheGuard() {
        val facts = CompanionCapabilityFacts()
        assertTrue(facts.register("shizuku.shell", "available", "", 3, true))
        assertTrue(facts.register("android.notification_listener", "unavailable", "LISTENER_DISCONNECTED", 5, false))
        assertTrue(facts.register("execution.app_guard", "available", "", 1, true))

        val replay = facts.apkHostReplay()
        assertEquals(listOf("android.notification_listener", "shizuku.shell"), replay.map { it.key })
        assertEquals(listOf(5L, 3L), replay.map { it.sourceGeneration })
        assertEquals(listOf("LISTENER_DISCONNECTED", null), replay.map { it.reason })
        assertEquals(listOf(false, true), replay.map { it.hasExecutor })
        assertEquals(
            listOf("android.notification_listener", "execution.app_guard", "shizuku.shell"),
            facts.snapshot().map { it.key },
        )
    }
}
