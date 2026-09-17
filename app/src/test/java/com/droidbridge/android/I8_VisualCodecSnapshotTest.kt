package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.VisualCodecFact
import com.droidbridge.android.execution.android.VisualCodecSnapshotAdapter
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class I8_VisualCodecSnapshotTest {
    @Test
    fun I8_VIS_G04_snapshotReturnsGenerationFencedCodecFactWithoutJni() = runBlocking {
        val seen = mutableListOf<Pair<Int, Int>>()
        val adapter = VisualCodecSnapshotAdapter(
            probe = { width, height ->
                seen += width to height
                VisualCodecFact(true, codecName = "test-codec")
            },
            validatesFence = { epoch, generation, instance ->
                epoch == EPOCH && generation == 7L && instance == INSTANCE
            },
        )
        val result = Json.parseToJsonElement(adapter.execute(request()).payload.decodeToString()).jsonObject
        assertEquals("available", result.getValue("hardware_heic").jsonPrimitive.content)
        assertTrue(result.getValue("codec_generation").jsonPrimitive.content.toLong() > 0)
        assertEquals(setOf("hardware_heic", "codec_generation"), result.keys)
        assertEquals(listOf(1080 to 2400), seen)
    }

    @Test
    fun I8_VIS_G04_unavailableIsAPlatformFactWhileBadFenceAndShapeAreErrors() {
        val adapter = VisualCodecSnapshotAdapter(
            probe = { _, _ -> VisualCodecFact(false, "NO_HARDWARE_HEIC_ENCODER") },
            validatesFence = { _, generation, _ -> generation == 7L },
        )
        val unavailable = runBlocking {
            Json.parseToJsonElement(adapter.execute(request()).payload.decodeToString()).jsonObject
        }
        assertEquals("unavailable", unavailable.getValue("hardware_heic").jsonPrimitive.content)
        assertEquals("NO_HARDWARE_HEIC_ENCODER", unavailable.getValue("reason").jsonPrimitive.content)
        assertEquals("STALE_AUTHORITY", failure { adapter.execute(request(hostGeneration = 8)) })
        assertEquals(
            "INVALID_ARGUMENT",
            failure { adapter.execute(request(payload = "{\"width\":0,\"height\":1,\"source\":\"privileged_raw\"}")) },
        )
    }

    private fun request(
        hostGeneration: Long = 7,
        payload: String = "{\"width\":1080,\"height\":2400,\"source\":\"privileged_raw\"}",
    ) = AndroidExecutionRequest(
        AndroidPrimitive.VisualCodecSnapshot,
        payload.encodeToByteArray(),
        EXECUTION,
        EPOCH,
        hostGeneration,
        INSTANCE,
    )

    private fun failure(block: suspend () -> Unit): String? = try {
        runBlocking { block() }
        null
    } catch (error: AndroidExecutionException) {
        error.code
    }

    private companion object {
        const val EPOCH = "20000000-0000-4000-8000-000000000001"
        const val INSTANCE = "20000000-0000-4000-8000-000000000002"
        const val EXECUTION = "20000000-0000-4000-8000-000000000003"
    }
}
