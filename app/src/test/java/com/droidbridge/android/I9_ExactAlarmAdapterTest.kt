package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.ExactAlarmAccess
import com.droidbridge.android.execution.android.ExactAlarmAdapter
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class I9_ExactAlarmAdapterTest {
    @Test
    fun I9_G02_apkAlarmArmsOnlyTheCarriedCanonicalDue() = runBlocking {
        val access = RecordingAlarmAccess()
        val adapter = ExactAlarmAdapter(access, ::validatesFence)

        assertEquals(
            "{\"scheduled\":true}",
            adapter.execute(schedule(DUE)).payload.decodeToString(),
        )
        assertEquals(DUE, access.armedAt)
        // A re-arm replaces the single alarm rather than adding a second schedule.
        adapter.execute(schedule(DUE + 60_000))
        assertEquals(DUE + 60_000, access.armedAt)
        assertEquals(listOf(DUE, DUE + 60_000), access.arms)

        assertEquals(
            "{\"cancelled\":true}",
            adapter.execute(request(AndroidPrimitive.AlarmCancel, "{}")).payload.decodeToString(),
        )
        assertNull(access.armedAt)
    }

    @Test
    fun I9_G02_missingExactAlarmAccessIsExplicitAndArmsNothing() {
        val access = RecordingAlarmAccess(canSchedule = false)
        val adapter = ExactAlarmAdapter(access, ::validatesFence)

        assertEquals("CAPABILITY_UNAVAILABLE", failureOf { adapter.execute(schedule(DUE)) })
        assertNull(access.armedAt)

        access.canSchedule = true
        access.revoked = true
        assertEquals("CAPABILITY_UNAVAILABLE", failureOf { adapter.execute(schedule(DUE)) })
        assertNull(access.armedAt)
    }

    @Test
    fun I9_G02_staleFenceAndMalformedDueAreRejected() {
        val access = RecordingAlarmAccess()
        val adapter = ExactAlarmAdapter(access, ::validatesFence)

        assertEquals(
            "STALE_AUTHORITY",
            failureOf { adapter.execute(schedule(DUE).copy(hostGeneration = HOST_GENERATION + 1)) },
        )
        for (payload in listOf(
            "{}",
            "{\"due_unix_millis\":\"$DUE\"}",
            "{\"due_unix_millis\":0}",
            "{\"due_unix_millis\":$DUE,\"extra\":1}",
            "not json",
        )) {
            assertEquals(
                payload,
                "INVALID_ARGUMENT",
                failureOf { adapter.execute(request(AndroidPrimitive.AlarmSchedule, payload)) },
            )
        }
        assertEquals(
            "INVALID_ARGUMENT",
            failureOf { adapter.execute(request(AndroidPrimitive.AlarmCancel, "{\"due_unix_millis\":1}")) },
        )
        assertEquals(
            "UNSUPPORTED",
            failureOf { adapter.execute(request(AndroidPrimitive.NetworkDefaultSubscribe, "{}")) },
        )
        assertEquals(emptyList<Long>(), access.arms)
    }

    private class RecordingAlarmAccess(
        var canSchedule: Boolean = true,
    ) : ExactAlarmAccess {
        var revoked = false
        var armedAt: Long? = null
        val arms = mutableListOf<Long>()

        override fun canScheduleExactAlarms(): Boolean = canSchedule

        override fun setExactAndAllowWhileIdle(triggerAtMillis: Long) {
            if (revoked) throw SecurityException("exact alarm access revoked")
            arms += triggerAtMillis
            armedAt = triggerAtMillis
        }

        override fun cancel() {
            armedAt = null
        }
    }

    private fun schedule(due: Long) =
        request(AndroidPrimitive.AlarmSchedule, "{\"due_unix_millis\":$due}")

    private fun request(primitive: AndroidPrimitive, payload: String) = AndroidExecutionRequest(
        primitive = primitive,
        payload = payload.encodeToByteArray(),
        executionId = "99400000-0000-4000-8000-000000000009",
        runtimeEpoch = RUNTIME_EPOCH,
        hostGeneration = HOST_GENERATION,
        runtimeInstanceId = INSTANCE,
    )

    private fun validatesFence(epoch: String, generation: Long, instance: String): Boolean =
        epoch == RUNTIME_EPOCH && generation == HOST_GENERATION && instance == INSTANCE

    private fun failureOf(block: suspend () -> Unit): String? = try {
        runBlocking { block() }
        null
    } catch (error: AndroidExecutionException) {
        error.code
    }

    private companion object {
        const val RUNTIME_EPOCH = "99400000-0000-4000-8000-000000000001"
        const val INSTANCE = "99400000-0000-4000-8000-000000000002"
        const val HOST_GENERATION = 3L
        const val DUE = 1_789_372_800_000L
    }
}
