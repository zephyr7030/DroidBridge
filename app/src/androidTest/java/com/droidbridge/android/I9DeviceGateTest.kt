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
import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I9 device evidence for one host wake projection. The APK fixture's only time wake is the
 * single exact alarm and the Magisk fixture's is the CLOCK_REALTIME_ALARM timerfd, so an
 * execution admitted at the saved due proves that host projection armed the persisted due truth.
 * Every step uses only the public `context.status` and `automation` entry points.
 */
@RunWith(AndroidJUnit4::class)
class I9DeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun I9_G02_hostWakeProjectionAdmitsTheSavedDueExactlyOnce() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val due = Instant.now().plusSeconds(DUE_DELAY_SECONDS).truncatedTo(ChronoUnit.MILLIS)
        val id = save(runtime, "i9 $fixture wake", due, enabled = true)
        try {
            val automation = awaitCompletedExecution(runtime, id, due)
            assertEquals(
                "the admitted execution ran the saved action -> $automation",
                fixture,
                automation.getJSONObject("state").getString(STATE_KEY),
            )
            // An `at` due is consumed by its admission, so no second execution follows.
            SystemClock.sleep(AFTER_DUE_SETTLE_MS)
            assertEquals(1, history(runtime, id).length())
        } finally {
            delete(runtime, id)
        }
    }

    @Test
    fun I9_G02_disablingBeforeTheDueDisarmsWithoutAdmission() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val due = Instant.now().plusSeconds(DUE_DELAY_SECONDS).truncatedTo(ChronoUnit.MILLIS)
        val id = save(runtime, "i9 $fixture disarm", due, enabled = true)
        try {
            val revision = get(runtime, id).getJSONObject("automation").getLong("revision")
            success(
                runtime,
                "set_enabled",
                JSONObject().put("automation_id", id).put("enabled", false).put("expected_revision", revision),
            )
            val waitUntil = due.toEpochMilli() + AFTER_DUE_SETTLE_MS
            while (System.currentTimeMillis() < waitUntil) SystemClock.sleep(500)
            assertEquals("a disabled at-due is never admitted", 0, history(runtime, id).length())
        } finally {
            delete(runtime, id)
        }
    }

    private fun save(runtime: IDroidBridgeRuntime, name: String, due: Instant, enabled: Boolean): String =
        success(
            runtime,
            "save",
            JSONObject()
                .put("name", name)
                .put("enabled", enabled)
                .put("trigger", JSONObject().put("type", "at").put("at", due.toString()))
                .put(
                    "action",
                    JSONObject()
                        .put("type", "set_state")
                        .put("key", STATE_KEY)
                        .put("value", fixture()),
                ),
        ).getString("automation_id")

    private fun awaitCompletedExecution(runtime: IDroidBridgeRuntime, id: String, due: Instant): JSONObject {
        val deadline = due.toEpochMilli() + EXECUTION_DEADLINE_MS
        while (true) {
            val fetched = get(runtime, id)
            val executions = fetched.getJSONArray("history")
            for (index in 0 until executions.length()) {
                val execution = executions.getJSONObject(index)
                if (execution.getString("state") == "completed") {
                    assertEquals(
                        "the execution was triggered at the persisted due -> $execution",
                        due,
                        Instant.parse(execution.getString("triggered_at")),
                    )
                    assertTrue(
                        "the wake was not early -> $execution",
                        !Instant.parse(execution.getString("started_at")).isBefore(due),
                    )
                    return fetched.getJSONObject("automation")
                }
                assertTrue("execution failed -> $execution", execution.getString("state") in LIVE_STATES)
            }
            if (System.currentTimeMillis() >= deadline) {
                error("no execution completed within ${EXECUTION_DEADLINE_MS}ms of $due -> $fetched")
            }
            SystemClock.sleep(500)
        }
    }

    private fun history(runtime: IDroidBridgeRuntime, id: String): JSONArray = get(runtime, id).getJSONArray("history")

    private fun get(runtime: IDroidBridgeRuntime, id: String): JSONObject =
        success(runtime, "get", JSONObject().put("automation_id", id).put("history_limit", 10))

    private fun delete(runtime: IDroidBridgeRuntime, id: String) {
        val revision = get(runtime, id).getJSONObject("automation").getLong("revision")
        success(runtime, "delete", JSONObject().put("automation_id", id).put("expected_revision", revision))
    }

    private fun success(runtime: IDroidBridgeRuntime, action: String, input: JSONObject): JSONObject {
        val response = submit(
            runtime,
            JSONObject()
                .put("protocol_version", 1)
                .put("request_id", UUID.randomUUID().toString())
                .put("payload", JSONObject().put("tool", "automation").put("action", action).put("input", input))
                .toString()
                .toByteArray(Charsets.UTF_8),
        )
        assertEquals("automation.$action -> $response", "success", response.getString("outcome"))
        return response.getJSONObject("result")
    }

    private fun fixture(): String {
        val fixture = InstrumentationRegistry.getArguments().getString("i9AutomationFixture") ?: APK_FIXTURE
        require(fixture == APK_FIXTURE || fixture == MAGISK_FIXTURE) { "unknown I9 automation fixture: $fixture" }
        return fixture
    }

    // Every gate starts from the admitted host and its time-wake grant instead of racing startup.
    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = fixture()
        val deadline = SystemClock.elapsedRealtime() + ADMISSION_DEADLINE_MS
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            val result = status.optJSONObject("result")
            val host = result?.optJSONObject("runtime")
            val grants = result?.optJSONObject("grants")
            val admitted = host?.optString("readiness") == "ready" && grants != null && when (fixture) {
                MAGISK_FIXTURE -> host.optString("host") == "magisk_backend" &&
                    grants.optJSONObject("magisk.wake_alarm")?.optString("state") == "available"
                else -> host.optString("host") == "apk_runtime" &&
                    grants.optJSONObject("automation.exact_alarm")?.optString("state") == "available"
            }
            if (admitted) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) error("I9 fixture $fixture was not admitted: $status")
            SystemClock.sleep(200)
        }
    }

    private fun contextStatusRequest(): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put("payload", JSONObject().put("tool", "context").put("action", "status").put("input", JSONObject().put("detail", "full")))
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun submit(runtime: IDroidBridgeRuntime, request: ByteArray): JSONObject {
        val latch = CountDownLatch(1)
        var response: ByteArray? = null
        runtime.submit(request, object : IRuntimeCallback.Stub() {
            override fun onResponse(value: ByteArray?) {
                response = value
                latch.countDown()
            }
        })
        assertTrue(latch.await(60, TimeUnit.SECONDS))
        return JSONObject(requireNotNull(response).toString(Charsets.UTF_8))
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

    private companion object {
        const val APK_FIXTURE = "apk"
        const val MAGISK_FIXTURE = "magisk"
        const val STATE_KEY = "i9_fired"
        const val DUE_DELAY_SECONDS = 20L
        const val AFTER_DUE_SETTLE_MS = 10_000L
        const val EXECUTION_DEADLINE_MS = 60_000L
        const val ADMISSION_DEADLINE_MS = 75_000L
        val LIVE_STATES = setOf("queued", "running")
    }
}
