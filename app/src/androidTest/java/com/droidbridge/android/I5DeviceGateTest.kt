package com.droidbridge.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.runtimehost.IDroidBridgeRuntime
import com.droidbridge.android.runtimehost.II5Benchmark
import com.droidbridge.android.runtimehost.IRuntimeCallback
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.system.measureNanoTime
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class I5DeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun I5_G01_apkRuntimeBinderJniAndTaskRoundTrip() {
        withApkRuntime { runtime ->
            val status = JSONObject(submit(runtime, contextRequest()).toString(Charsets.UTF_8))
            assertEquals(status.toString(), "success", status.getString("outcome"))
            val result = status.getJSONObject("result")
            assertEquals("apk_runtime", result.getJSONObject("runtime").getString("host"))

            val tasks = JSONObject(submit(runtime, taskListRequest()).toString(Charsets.UTF_8))
            assertEquals("success", tasks.getString("outcome"))
            assertEquals(0, tasks.getJSONObject("result").getJSONArray("tasks").length())
        }
    }

    @Test
    fun I5_G07_contextRefreshRoundTripMeetsPhysicalDeviceBudget() {
        withApkRuntime { runtime ->
            val samples = LongArray(100) {
                measureNanoTime {
                    val response = JSONObject(submit(runtime, contextRequest()).toString(Charsets.UTF_8))
                    assertEquals(response.toString(), "success", response.getString("outcome"))
                }
            }.sorted()
            assertTrue(samples[94] <= TimeUnit.MILLISECONDS.toNanos(250))
            assertTrue(samples[98] <= TimeUnit.MILLISECONDS.toNanos(500))
        }
    }

    @Test
    fun I5_G07_deviceProtectedStoreMeetsPhysicalDeviceBudget() {
        withBenchmark { benchmark ->
            val result = JSONObject(benchmark.runBenchmark())
            assertFalse(result.has("error"))
            assertTrue(result.getLong("two_mib_p95_ms") <= 75)
            assertTrue(result.getLong("two_mib_p99_ms") <= 150)
            assertTrue(result.getLong("eight_mib_p95_ms") <= 250)
            assertTrue(result.getLong("eight_mib_p99_ms") <= 500)
            assertTrue(result.getLong("steady_elapsed_ms") >= 60_000)
            assertTrue(result.getLong("steady_commits") >= 600)
            assertTrue(result.getLong("steady_max_lock_wait_ms") <= 500)
        }
    }

    @Test
    fun I5_G08_appGuardPublishesOnlyVerifiedAvailability() {
        withApkRuntime { runtime ->
            val status = JSONObject(submit(runtime, contextRequest()).toString(Charsets.UTF_8))
            assertEquals(status.toString(), "success", status.getString("outcome"))
            val guard = status.getJSONObject("result")
                .getJSONObject("grants")
                .getJSONObject("execution.app_guard")
            assertEquals("available", guard.getString("state"))
            assertFalse(guard.has("reason"))
        }
    }

    private fun contextRequest(): ByteArray = request(
        tool = "context",
        action = "status",
        input = JSONObject().put("detail", "full"),
    )

    private fun taskListRequest(): ByteArray = request(
        tool = "task_control",
        action = "list",
        input = JSONObject(),
    )

    private fun request(tool: String, action: String, input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put("payload", JSONObject().put("tool", tool).put("action", action).put("input", input))
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun submit(runtime: IDroidBridgeRuntime, request: ByteArray): ByteArray {
        val latch = CountDownLatch(1)
        var response: ByteArray? = null
        runtime.submit(request, object : IRuntimeCallback.Stub() {
            override fun onResponse(value: ByteArray?) {
                response = value
                latch.countDown()
            }
        })
        assertTrue(latch.await(5, TimeUnit.SECONDS))
        return requireNotNull(response)
    }

    private fun withRuntime(block: (IDroidBridgeRuntime) -> Unit) {
        withService(
            "com.droidbridge.android.runtimehost.DroidBridgeService",
            { IDroidBridgeRuntime.Stub.asInterface(it) },
            block,
        )
    }

    private fun withApkRuntime(block: (IDroidBridgeRuntime) -> Unit) {
        withRuntime { runtime ->
            val status = requireNotNull(awaitSuccessfulContext(runtime))
            val result = status.getJSONObject("result")
            val observedHost = result.getJSONObject("runtime").getString("host")
            if (observedHost != "apk_runtime") {
                assertEquals(status.toString(), "magisk_backend", observedHost)
                assertEquals(
                    status.toString(),
                    "available",
                    result.getJSONObject("grants")
                        .getJSONObject("magisk.module")
                        .getString("state"),
                )
                return@withRuntime
            }
            block(runtime)
        }
    }

    private fun awaitSuccessfulContext(runtime: IDroidBridgeRuntime): JSONObject? {
        val deadline = SystemClock.elapsedRealtime() + 45_000
        var status = JSONObject()
        do {
            status = JSONObject(submit(runtime, contextRequest()).toString(Charsets.UTF_8))
            if (status.optString("outcome") == "success") return status
            assertEquals(
                status.toString(),
                "CAPABILITY_UNAVAILABLE",
                status.optJSONObject("error")?.optString("code"),
            )
            SystemClock.sleep(100)
        } while (SystemClock.elapsedRealtime() < deadline)
        return null
    }

    private fun withBenchmark(block: (II5Benchmark) -> Unit) {
        withService(
            "com.droidbridge.android.runtimehost.I5BenchmarkService",
            { II5Benchmark.Stub.asInterface(it) },
            block,
        )
    }

    private fun <T> withService(className: String, convert: (IBinder) -> T, block: (T) -> Unit) {
        val connected = CountDownLatch(1)
        var service: T? = null
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: IBinder) {
                service = convert(binder)
                connected.countDown()
            }

            override fun onServiceDisconnected(name: ComponentName) = Unit
        }
        val intent = Intent().setComponent(ComponentName(context.packageName, className))
        assertTrue(context.bindService(intent, connection, Context.BIND_AUTO_CREATE))
        try {
            assertTrue(connected.await(10, TimeUnit.SECONDS))
            block(requireNotNull(service))
        } finally {
            context.unbindService(connection)
        }
    }
}
