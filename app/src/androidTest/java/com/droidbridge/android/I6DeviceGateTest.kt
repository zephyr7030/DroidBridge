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
import com.droidbridge.android.runtimehost.IRuntimeCallback
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class I6DeviceGateTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context
        get() = instrumentation.targetContext

    @Test
    fun I6_G01_G06_officialShizukuFixturePublishesOnlyVerifiedIdentityAndGuard() {
        val fixture = InstrumentationRegistry.getArguments().getString("i6Fixture") ?: "truthful"
        withRuntime { runtime ->
            val deadline = SystemClock.elapsedRealtime() + 75_000
            var lastResponse = JSONObject()
            var shizuku: JSONObject? = null
            var guard: JSONObject? = null
            do {
                lastResponse = JSONObject(submit(runtime).toString(Charsets.UTF_8))
                val grants = lastResponse.optJSONObject("result")?.optJSONObject("grants")
                shizuku = grants?.optJSONObject("shizuku.shell")
                guard = grants?.optJSONObject("execution.shell_guard")
                if (shizuku != null && guard != null && matches(fixture, shizuku, guard)) break
                SystemClock.sleep(100)
            } while (SystemClock.elapsedRealtime() < deadline)
            val observedShizuku = requireNotNull(shizuku) { lastResponse.toString() }
            val observedGuard = requireNotNull(guard) { lastResponse.toString() }

            when (fixture) {
                "ungranted" -> {
                    assertEquals("unavailable", observedShizuku.getString("state"))
                    assertEquals("GRANT_MISSING", observedShizuku.getString("reason"))
                    assertFalse(observedGuard.getString("state") == "available")
                }
                "uid2000" -> {
                    assertEquals(
                        observedShizuku.toString(),
                        "available",
                        observedShizuku.getString("state"),
                    )
                    assertFalse(observedShizuku.has("reason"))
                    assertEquals(observedGuard.toString(), "available", observedGuard.getString("state"))
                    assertFalse(observedGuard.has("reason"))
                }
                "truthful" -> assertTruthful(observedShizuku, observedGuard)
                else -> throw AssertionError("unknown I6 fixture")
            }
        }
    }

    private fun matches(fixture: String, shizuku: JSONObject, guard: JSONObject): Boolean =
        when (fixture) {
            "ungranted" -> shizuku.optString("reason") == "GRANT_MISSING"
            "uid2000" -> shizuku.optString("state") == "available" &&
                guard.optString("state") == "available"
            "truthful" -> runCatching { assertTruthful(shizuku, guard) }.isSuccess
            else -> false
        }

    private fun assertTruthful(shizuku: JSONObject, guard: JSONObject) {
        when (shizuku.getString("state")) {
            "unavailable" -> {
                assertTrue(
                    shizuku.getString("reason") in setOf(
                        "MANAGER_NOT_INSTALLED",
                        "BINDER_UNAVAILABLE",
                        "GRANT_MISSING",
                        "INCOMPATIBLE_IDENTITY",
                    ),
                )
                assertEquals("unavailable", guard.getString("state"))
            }
            "unknown" -> {
                assertEquals("CONNECTING", shizuku.getString("reason"))
                assertEquals("unknown", guard.getString("state"))
                assertEquals("CONNECTING", guard.getString("reason"))
            }
            "available" -> {
                assertFalse(shizuku.has("reason"))
                when (guard.getString("state")) {
                    "available" -> assertFalse(guard.has("reason"))
                    "unknown" -> assertEquals("CONNECTING", guard.getString("reason"))
                    "unavailable" -> assertTrue(
                        guard.getString("reason") in setOf(
                            "GUARD_PROBE_FAILED",
                            "CLEANUP_UNVERIFIED",
                        ),
                    )
                    else -> throw AssertionError("invalid guard state")
                }
            }
            else -> throw AssertionError("invalid Shizuku state")
        }
    }

    private fun submit(runtime: IDroidBridgeRuntime): ByteArray {
        val request = JSONObject()
            .put("protocol_version", 1)
            .put("request_id", UUID.randomUUID().toString())
            .put(
                "payload",
                JSONObject()
                    .put("tool", "context")
                    .put("action", "status")
                    .put("input", JSONObject().put("detail", "full")),
            )
            .toString()
            .toByteArray()
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
        val connected = CountDownLatch(1)
        var runtime: IDroidBridgeRuntime? = null
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: IBinder) {
                runtime = IDroidBridgeRuntime.Stub.asInterface(binder)
                connected.countDown()
            }

            override fun onServiceDisconnected(name: ComponentName) = Unit
        }
        val intent = Intent().setComponent(
            ComponentName(context.packageName, "com.droidbridge.android.runtimehost.DroidBridgeService"),
        )
        assertTrue(context.bindService(intent, connection, Context.BIND_AUTO_CREATE))
        try {
            assertTrue(connected.await(10, TimeUnit.SECONDS))
            block(requireNotNull(runtime))
        } finally {
            context.unbindService(connection)
        }
    }
}
