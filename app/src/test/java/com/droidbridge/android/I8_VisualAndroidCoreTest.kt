package com.droidbridge.android

import com.droidbridge.android.execution.android.AccessibilitySceneProof
import com.droidbridge.android.execution.android.AccessibilityServiceStartupFact
import com.droidbridge.android.execution.android.AccessibilitySceneStore
import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.execution.android.VisualCodecFact
import com.droidbridge.android.execution.android.VisualCodecHealth
import com.droidbridge.android.execution.android.VisualDisplaySnapshot
import com.droidbridge.android.execution.android.ProjectionSessionSlot
import com.droidbridge.android.execution.android.decodeVisualRawRow
import com.droidbridge.android.execution.shizuku.ShizukuGuardedPlanCodec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class I8_VisualAndroidCoreTest {
    @Test
    fun I8_VIS_G07_sceneLookupKeepsTheExactRetainedHandleAndNeverRetargetsAReusedRef() {
        var now = 1_000L
        val released = mutableListOf<String>()
        val store = AccessibilitySceneStore<String>(
            capacity = 2,
            ttlMillis = 300_000,
            clockMillis = { now },
            release = released::add,
        )
        val first = proof(revision = 4, hash = "first")
        val replacement = proof(revision = 5, hash = "replacement")
        store.put("observation-a", first, mapOf("node-1" to "old-handle"))
        store.put("observation-b", replacement, mapOf("node-1" to "new-handle"))

        assertEquals("old-handle", store.exact("observation-a", "node-1", first))
        assertNull(store.exact("observation-a", "node-1", replacement))
        assertEquals("new-handle", store.exact("observation-b", "node-1", replacement))
        assertEquals(emptyList<String>(), released)
    }

    @Test
    fun I8_VIS_G09_sceneExpiryAndEvictionReleaseEveryOwnedHandle() {
        var now = 5_000L
        val first = Any()
        val second = Any()
        val third = Any()
        val released = mutableListOf<Any>()
        val store = AccessibilitySceneStore<Any>(
            capacity = 1,
            ttlMillis = 100,
            clockMillis = { now },
            release = released::add,
        )
        store.put("observation-a", proof(1, "a"), mapOf("a" to first))
        store.put("observation-b", proof(2, "b"), mapOf("b" to second))
        assertEquals(listOf(first), released)
        assertSame(second, store.exact("observation-b", "b", proof(2, "b")))

        now += 101
        assertNull(store.exact("observation-b", "b", proof(2, "b")))
        assertEquals(listOf(first, second), released)
        store.put("observation-c", proof(3, "c"), mapOf("c" to third))
        store.clear()
        assertEquals(listOf(first, second, third), released)
    }

    @Test
    fun I8_VIS_G09_sceneStoreReportsTheNextMandatoryExpiry() {
        var now = 2_000L
        val released = mutableListOf<String>()
        val store = AccessibilitySceneStore<String>(
            capacity = 2,
            ttlMillis = 100,
            clockMillis = { now },
            release = released::add,
        )
        store.put("observation-a", proof(1, "a"), mapOf("a" to "first"))

        assertEquals(100L, store.expireAndNextDelayMillis())
        now += 100
        assertNull(store.expireAndNextDelayMillis())
        assertEquals(listOf("first"), released)
    }

    @Test
    fun I8_VIS_G03_rawRowsPreserveTheFiveContractPixelFormats() {
        assertArrayEquals(
            intArrayOf(unchecked(0x7f112233), unchecked(0x00a0b0c0)),
            decodeVisualRawRow(1, byteArrayOf(0x11, 0x22, 0x33, 0x7f, 0xa0.toByte(), 0xb0.toByte(), 0xc0.toByte(), 0)),
        )
        assertArrayEquals(
            intArrayOf(unchecked(0xff112233)),
            decodeVisualRawRow(2, byteArrayOf(0x11, 0x22, 0x33, 0)),
        )
        assertArrayEquals(
            intArrayOf(unchecked(0xff112233)),
            decodeVisualRawRow(3, byteArrayOf(0x11, 0x22, 0x33)),
        )
        assertArrayEquals(
            intArrayOf(unchecked(0xffff0000), unchecked(0xff00ff00), unchecked(0xff0000ff)),
            decodeVisualRawRow(4, byteArrayOf(0x00, 0xf8.toByte(), 0xe0.toByte(), 0x07, 0x1f, 0x00)),
        )
        assertArrayEquals(
            intArrayOf(unchecked(0x7f112233)),
            decodeVisualRawRow(5, byteArrayOf(0x33, 0x22, 0x11, 0x7f)),
        )
    }

    @Test
    fun I8_VIS_G03_shizukuVisualPlansKeepTheExactProgramsDeadlinesAndOutputBounds() {
        val raw = ShizukuGuardedPlanCodec.decode(
            "screen_capture_raw",
            "{}".encodeToByteArray(),
            "/data/user/0/com.droidbridge.android/files/guard",
        )
        assertEquals("/system/bin/screencap", raw.program)
        assertEquals(emptyList<String>(), raw.arguments)
        assertEquals(5_000L, raw.deadlineMs)
        assertEquals(64 * 1_024 * 1_024 + 16, raw.stdoutLimit)

        val longPress = ShizukuGuardedPlanCodec.decode(
            "input_long_press",
            "{\"x\":12,\"y\":34}".encodeToByteArray(),
            "/data/user/0/com.droidbridge.android/files/guard",
        )
        assertEquals("/system/bin/input", longPress.program)
        assertEquals(listOf("swipe", "12", "34", "12", "34", "500"), longPress.arguments)
        assertEquals(5_000L, longPress.deadlineMs)

        // Ctrl+A: the modifier keycodes held while the final key is pressed.
        val combination = ShizukuGuardedPlanCodec.decode(
            "input_key_combination",
            "{\"key_codes\":[113,29]}".encodeToByteArray(),
            "/data/user/0/com.droidbridge.android/files/guard",
        )
        assertEquals("/system/bin/input", combination.program)
        assertEquals(listOf("keycombination", "113", "29"), combination.arguments)
        for (payload in listOf("{\"key_codes\":[29]}", "{\"key_codes\":[113,-1]}", "{\"key_codes\":\"113\"}")) {
            assertThrows(IllegalArgumentException::class.java) {
                ShizukuGuardedPlanCodec.decode(
                    "input_key_combination",
                    payload.encodeToByteArray(),
                    "/data/user/0/com.droidbridge.android/files/guard",
                )
            }
        }
    }

    @Test
    fun I8_VIS_G05_failedHeicGenerationCannotAuthorizeAnotherEncode() {
        var probes = 0
        val health = VisualCodecHealth { _, _ ->
            probes += 1
            VisualCodecFact(true, codecName = "test-codec")
        }
        val first = health.snapshot(1080, 2400)
        assertTrue(health.accepts(first.generation, 1080, 2400))

        health.invalidate(first.generation)

        assertFalse(health.accepts(first.generation, 1080, 2400))
        val replacement = health.snapshot(1080, 2400)
        assertTrue(replacement.generation > first.generation)
        assertEquals(2, probes)
    }

    @Test
    fun I8_VIS_G02_projectionSlotDetachesTheSessionBeforeIdempotentCleanup() {
        val released = mutableListOf<String>()
        val slot = ProjectionSessionSlot<String>()
        val generation = slot.publish("session-one")

        assertSame("session-one", slot.current(generation))
        assertThrows(IllegalStateException::class.java) { slot.publish("session-two") }
        val removed = slot.take()
        assertEquals("session-one", removed)
        assertNull(slot.current(generation))
        released.add(requireNotNull(removed))
        assertEquals(listOf("session-one"), released)
        assertNull(slot.take())
        assertTrue(slot.publish("session-three") > generation)
    }

    @Test
    fun I8_VIS_G04_sharedPrimitiveNameRemainsAddressableAtEachCapabilityGeneration() {
        val registry = AndroidExecutionRegistry { _, _, _, _, _ -> true }
        val framework = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(1)) }
        val accessibility = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(2)) }
        registry.register(
            CapabilityRegistration(
                "android.framework",
                RegisteredCapabilityState.Available,
                null,
                7,
                framework,
                setOf(AndroidPrimitive.AccessibilityObserve),
            ),
        )
        registry.register(
            CapabilityRegistration(
                "visual.accessibility",
                RegisteredCapabilityState.Available,
                null,
                11,
                accessibility,
                setOf(AndroidPrimitive.AccessibilityObserve),
            ),
        )

        assertSame(framework, registry.executor(AndroidPrimitive.AccessibilityObserve, 7))
        assertSame(accessibility, registry.executor(AndroidPrimitive.AccessibilityObserve, 11))
    }

    @Test
    fun I8_VIS_G04_disabledAccessibilityIsUnavailableUntilItsServiceConnects() {
        val sunk = mutableListOf<Triple<String, String, Long>>()
        val registry = AndroidExecutionRegistry { _, state, reason, generation, _ ->
            sunk += Triple(state, reason, generation)
            true
        }
        assertNull(AccessibilityServiceStartupFact.registration(enabled = true))
        val disabled = requireNotNull(AccessibilityServiceStartupFact.registration(enabled = false))
        assertTrue(registry.register(disabled))

        val service = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(1)) }
        assertTrue(
            registry.register(
                CapabilityRegistration(
                    "visual.accessibility",
                    RegisteredCapabilityState.Available,
                    null,
                    99,
                    service,
                    setOf(AndroidPrimitive.AccessibilityObserve),
                ),
            ),
        )
        // A later startup fact never overrides the connected component.
        assertFalse(registry.register(disabled))

        assertSame(service, registry.executor("visual.accessibility", 99))
        assertEquals(
            listOf(
                Triple("unavailable", AccessibilityServiceStartupFact.DISABLED_REASON, 1L),
                Triple("available", "", 99L),
            ),
            sunk,
        )
    }

    @Test
    fun I8_VIS_G04_conflictingPrimitiveRegistrationIsRejectedWithoutPartialMutation() {
        var sinkCalls = 0
        val registry = AndroidExecutionRegistry { _, _, _, _, _ ->
            sinkCalls += 1
            true
        }
        val first = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(1)) }
        val conflicting = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(2)) }
        registry.register(
            CapabilityRegistration(
                "first",
                RegisteredCapabilityState.Available,
                null,
                7,
                first,
                setOf(AndroidPrimitive.AccessibilityObserve),
            ),
        )

        assertThrows(IllegalArgumentException::class.java) {
            registry.register(
                CapabilityRegistration(
                    "conflicting",
                    RegisteredCapabilityState.Available,
                    null,
                    7,
                    conflicting,
                    setOf(AndroidPrimitive.AccessibilityObserve),
                ),
            )
        }

        assertEquals(1, sinkCalls)
        assertSame(first, registry.executor(AndroidPrimitive.AccessibilityObserve, 7))
        assertNull(registry.executor("conflicting", 7))
    }

    private fun proof(revision: Long, hash: String) = AccessibilitySceneProof(
        componentGeneration = 7,
        windowId = 3,
        sceneRevision = revision,
        display = VisualDisplaySnapshot(1080, 2400, 0, 420, 11),
        hierarchySha256 = hash,
    )

    private fun unchecked(value: Long): Int = value.toInt()
}
