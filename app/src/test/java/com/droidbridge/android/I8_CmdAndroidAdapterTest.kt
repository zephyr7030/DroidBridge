package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.AppProcessExecutor
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.execution.shizuku.ShizukuGuardedPlanCodec
import com.droidbridge.android.runtimehost.decodeCompanionCancellation
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I8_CmdAndroidAdapterTest {
    @Test
    fun I8_CMD_G01_appSurfaceReportsTheSharedSettlementAndItsTypedFailureCode() = runBlocking {
        val executor = AppProcessExecutor(
            validatesFence = { _, _, _ -> true },
            runCommand = { _, _ -> """{"cause":"exited","exit_code":0,"stdout_base64":"","stderr_base64":""}""" },
        )
        val settled = executor.execute(request(AndroidPrimitive.AppProcessStart))
        assertEquals(
            """{"cause":"exited","exit_code":0,"stdout_base64":"","stderr_base64":""}""",
            settled.payload.decodeToString(),
        )

        val rejected = AppProcessExecutor(
            validatesFence = { _, _, _ -> true },
            runCommand = { _, _ -> """{"error":{"code":"NOT_FOUND","retryable":false}}""" },
        )
        val failure = runCatching {
            rejected.execute(request(AndroidPrimitive.AppProcessStart))
        }.exceptionOrNull()
        assertEquals("NOT_FOUND", (failure as AndroidExecutionException).code)
    }

    @Test
    fun I8_CMD_G01_anUndecodablePrimitiveReportIsAnUnverifiedCleanup() = runBlocking {
        for (report in listOf(null, "not a command report")) {
            val executor = AppProcessExecutor(
                validatesFence = { _, _, _ -> true },
                runCommand = { _, _ -> report },
            )
            val failure = runCatching {
                executor.execute(request(AndroidPrimitive.AppProcessStart))
            }.exceptionOrNull()
            assertEquals("INTERNAL_ERROR", (failure as AndroidExecutionException).code)
        }
    }

    @Test
    fun I8_CMD_G01_appAndShizukuRejectWidenedInternalProcessRequests() = runBlocking {
        var invoked = false
        val app = AppProcessExecutor(
            validatesFence = { _, _, _ -> true },
            runCommand = { _, _ ->
                invoked = true
                """{"cause":"exited","exit_code":0,"stdout_base64":"","stderr_base64":""}"""
            },
        )
        val malformedUtf8 = runCatching {
            app.execute(
                request(
                    AndroidPrimitive.AppProcessStart,
                    byteArrayOf(0xc3.toByte(), 0x28),
                ),
            )
        }.exceptionOrNull()
        assertEquals("INVALID_ARGUMENT", (malformedUtf8 as AndroidExecutionException).code)
        assertTrue(!invoked)

        val guard = "/data/app/libdroidbridge_exec_guard.so"
        val exact =
            """{"program":"/system/bin/sh","arguments":["-c","id"],"timeout_ms":1000,"cwd":"/","max_output_bytes":1024}"""
                .encodeToByteArray()
        assertEquals(
            listOf("-c", "id"),
            ShizukuGuardedPlanCodec.decode("process_start", exact, guard).arguments,
        )
        for (widened in listOf(
            """{"program":"/system/bin/id","arguments":[],"timeout_ms":1000,"cwd":"/","max_output_bytes":1024}""",
            """{"program":"/system/bin/sh","arguments":["-c","id"],"timeout_ms":999,"cwd":"/","max_output_bytes":1024}""",
            """{"program":"/system/bin/sh","arguments":["-c","id"],"timeout_ms":1000,"cwd":"/","max_output_bytes":1}""",
        )) {
            val failure = runCatching {
                ShizukuGuardedPlanCodec.decode(
                    "process_start",
                    widened.encodeToByteArray(),
                    guard,
                )
            }.exceptionOrNull()
            assertTrue(failure is IllegalArgumentException)
        }
    }

    @Test
    fun I8_CMD_G02_appSurfaceServesOnlyItsOwnPrimitivesAndNeverSubstitutesARunner() = runBlocking {
        var started: Pair<String, String>? = null
        var cancelled: String? = null
        val executor = AppProcessExecutor(
            validatesFence = { epoch, generation, instance ->
                epoch == RUNTIME_EPOCH && generation == HOST_GENERATION && instance == INSTANCE
            },
            runCommand = { executionId, requestJson ->
                started = executionId to requestJson
                """{"cause":"exited","exit_code":0,"stdout_base64":"","stderr_base64":""}"""
            },
            cancelCommand = { executionId ->
                cancelled = executionId
                true
            },
        )
        val payload = """{"operation":"process_start","program":"/system/bin/sh"}"""
        executor.execute(
            request(AndroidPrimitive.AppProcessStart, payload.encodeToByteArray()),
        )
        assertEquals(EXECUTION_ID to payload, started)

        val unsupported = runCatching {
            executor.execute(request(AndroidPrimitive.ContentInspect))
        }.exceptionOrNull()
        assertEquals("UNSUPPORTED", (unsupported as AndroidExecutionException).code)
        assertNull(cancelled)

        val stale = runCatching {
            executor.execute(
                AndroidExecutionRequest(
                    primitive = AndroidPrimitive.AppProcessStart,
                    payload = payload.encodeToByteArray(),
                    executionId = EXECUTION_ID,
                    runtimeEpoch = RUNTIME_EPOCH,
                    hostGeneration = HOST_GENERATION + 1,
                    runtimeInstanceId = INSTANCE,
                ),
            )
        }.exceptionOrNull()
        assertEquals("STALE_AUTHORITY", (stale as AndroidExecutionException).code)
        assertEquals(EXECUTION_ID to payload, started)
    }

    @Test
    fun I8_CMD_G03_shizukuProcessPrimitivesRouteThroughTheAppLocalGenerationRegistration() {
        val registry = AndroidExecutionRegistry { key, state, reason, generation, hasExecutor ->
            if (key == "android.framework") {
                state == RegisteredCapabilityState.Available.wireValue &&
                    reason.isEmpty() && hasExecutor && generation > 0
            } else {
                true
            }
        }
        val routed = AndroidExecutionBridge {
            AndroidExecutionResult("routed".encodeToByteArray())
        }
        assertTrue(
            registry.register(
                CapabilityRegistration(
                    key = "shizuku.shell",
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = SHIZUKU_SOURCE_GENERATION,
                    executor = routed,
                ),
            ),
        )
        assertNull(registry.executor(AndroidPrimitive.ShizukuProcessStart, HOST_GENERATION))
        assertTrue(
            registry.register(
                CapabilityRegistration(
                    key = "android.framework",
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = HOST_GENERATION,
                    executor = routed,
                    primitives = setOf(
                        AndroidPrimitive.AppProcessStart,
                        AndroidPrimitive.AppProcessCancel,
                        AndroidPrimitive.ShizukuProcessStart,
                        AndroidPrimitive.ShizukuProcessCancel,
                    ),
                ),
            ),
        )
        for (primitive in listOf(
            AndroidPrimitive.AppProcessStart,
            AndroidPrimitive.AppProcessCancel,
            AndroidPrimitive.ShizukuProcessStart,
            AndroidPrimitive.ShizukuProcessCancel,
        )) {
            assertEquals(routed, registry.executor(primitive, HOST_GENERATION))
            assertNull(registry.executor(primitive, HOST_GENERATION + 1))
        }
        assertNull(registry.executor(AndroidPrimitive.ContentInspect, HOST_GENERATION))
    }

    @Test
    fun I8_CMD_G04_appSurfaceCancelAddressesExactlyOneExecutionAndRefusesEverythingElse() =
        runBlocking {
            var cancelled: String? = null
            val executor = AppProcessExecutor(
                validatesFence = { _, _, _ -> true },
                runCommand = { _, _ -> null },
                cancelCommand = { executionId ->
                    cancelled = executionId
                    false
                },
            )
            val result = executor.execute(
                request(
                    AndroidPrimitive.AppProcessCancel,
                    """{"execution_id":"$EXECUTION_ID"}""".encodeToByteArray(),
                ),
            )
            assertEquals(EXECUTION_ID, cancelled)
            assertEquals(
                false,
                Json.parseToJsonElement(result.payload.decodeToString())
                    .jsonObject.getValue("cancelled").jsonPrimitive.content.toBoolean(),
            )

            for (payload in listOf(
                """{"execution_id":"$EXECUTION_ID","primitive":"AppProcessStart"}""",
                """{"execution_id":"not-a-uuid"}""",
                """{"execution_id":""}""",
            )) {
                val rejected = runCatching {
                    executor.execute(
                        request(AndroidPrimitive.AppProcessCancel, payload.encodeToByteArray()),
                    )
                }.exceptionOrNull()
                assertEquals("INVALID_ARGUMENT", (rejected as AndroidExecutionException).code)
            }
            assertEquals(EXECUTION_ID, cancelled)
        }

    @Test
    fun I8_CMD_G04_companionCancellationNamesOnlyAnIdentitysOwnCancelPrimitive() {
        val cancellation = decodeCompanionCancellation(
            Json.parseToJsonElement(
                """{"primitive":"AppProcessCancel","execution_id":"$EXECUTION_ID"}""",
            ),
        )
        assertEquals(AndroidPrimitive.AppProcessCancel, cancellation?.primitive)
        assertEquals(EXECUTION_ID, cancellation?.executionId)
        val shell = decodeCompanionCancellation(
            Json.parseToJsonElement(
                """{"primitive":"ShizukuProcessCancel","execution_id":"$EXECUTION_ID"}""",
            ),
        )
        assertEquals(AndroidPrimitive.ShizukuProcessCancel, shell?.primitive)

        for (body in listOf(
            """{"primitive":"AppProcessStart","execution_id":"$EXECUTION_ID"}""",
            """{"primitive":"ShizukuProcessStart","execution_id":"$EXECUTION_ID"}""",
            """{"primitive":"ContentInspect","execution_id":"$EXECUTION_ID"}""",
            """{"primitive":"AppProcessCancel","execution_id":"not-a-uuid"}""",
            """{"primitive":"AppProcessCancel","execution_id":"$EXECUTION_ID","timeout_ms":1000}""",
            """{"execution_id":"$EXECUTION_ID"}""",
        )) {
            assertNull(decodeCompanionCancellation(Json.parseToJsonElement(body)))
        }
    }

    private fun request(
        primitive: AndroidPrimitive,
        payload: ByteArray = """{"operation":"process_start"}""".encodeToByteArray(),
    ): AndroidExecutionRequest = AndroidExecutionRequest(
        primitive = primitive,
        payload = payload,
        executionId = EXECUTION_ID,
        runtimeEpoch = RUNTIME_EPOCH,
        hostGeneration = HOST_GENERATION,
        runtimeInstanceId = INSTANCE,
    )

    private companion object {
        const val RUNTIME_EPOCH = "10000000-0000-4000-8000-000000000001"
        const val INSTANCE = "10000000-0000-4000-8000-000000000002"
        const val EXECUTION_ID = "10000000-0000-4000-8000-000000000003"
        const val HOST_GENERATION = 4L
        const val SHIZUKU_SOURCE_GENERATION = 1_789_171_200_000_000_000L
    }
}
