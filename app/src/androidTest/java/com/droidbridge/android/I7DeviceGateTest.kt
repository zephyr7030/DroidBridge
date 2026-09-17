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
class I7DeviceGateTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context
        get() = instrumentation.targetContext

    @Test
    fun I7_G01_G04_G05_G06_G12_magiskCapabilitiesRemainIndependentOnDevice() {
        val helperFixture =
            InstrumentationRegistry.getArguments().getString("i7Helper") ?: "available"
        withRuntime { runtime ->
            val deadline = SystemClock.elapsedRealtime() + 75_000
            var response = JSONObject()
            var grants: JSONObject? = null
            do {
                response = JSONObject(submit(runtime).toString(Charsets.UTF_8))
                grants = response.optJSONObject("result")?.optJSONObject("grants")
                if (grants != null && matches(helperFixture, grants)) break
                SystemClock.sleep(100)
            } while (SystemClock.elapsedRealtime() < deadline)
            val observed = requireNotNull(grants) { response.toString() }

            assertAvailable(observed, "magisk.module")
            assertAvailable(observed, "magisk.root")
            assertAvailable(observed, "execution.root_guard")
            assertAvailable(observed, "magisk.wake_alarm")
            assertTrue(
                observed.getJSONObject("shizuku.shell").getString("state") in
                    setOf("available", "unavailable", "unknown"),
            )
            when (helperFixture) {
                "available" -> assertAvailable(observed, "magisk.framework")
                "unavailable" -> {
                    assertUnavailable(observed, "magisk.framework", "HELPER_UNAVAILABLE")
                    assertUnavailable(observed, "magisk.launch", "HELPER_UNAVAILABLE")
                    assertUnavailable(observed, "magisk.clipboard", "HELPER_UNAVAILABLE")
                    assertUnavailable(observed, "magisk.notifications", "HELPER_UNAVAILABLE")
                }
                else -> throw AssertionError("unknown I7 helper fixture")
            }
        }
    }

    private fun matches(fixture: String, grants: JSONObject): Boolean =
        grants.optJSONObject("magisk.framework")?.optString("state") == fixture

    private fun assertAvailable(grants: JSONObject, key: String) {
        val capability = grants.getJSONObject(key)
        assertEquals(capability.toString(), "available", capability.getString("state"))
        assertFalse(capability.has("reason"))
    }

    private fun assertUnavailable(grants: JSONObject, key: String, reason: String) {
        val capability = grants.getJSONObject(key)
        assertEquals(capability.toString(), "unavailable", capability.getString("state"))
        assertEquals(capability.toString(), reason, capability.getString("reason"))
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
        assertTrue(latch.await(30, TimeUnit.SECONDS))
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
