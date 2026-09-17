package com.droidbridge.android

import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.util.Log
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
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I8-ANDROID device evidence through the canonical public ingress. The fixture argument
 * names the effective host/provider the run must be admitted under before any assertion.
 */
@RunWith(AndroidJUnit4::class)
class I8_AndroidDeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun I8_ANDROID_G01_packageListIsTheCompletePrivilegedInventoryOrUnavailable() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val first = submit(runtime, android("package", JSONObject().put("operation", "list").put("include_system", true).put("limit", 200)))
        if (fixture == FRAMEWORK) {
            assertEquals(first.toString(), "CAPABILITY_UNAVAILABLE", errorCode(first))
            return@withRuntime
        }
        val names = mutableListOf<String>()
        var page = result(first)
        var guard = 0
        while (true) {
            val packages = page.getJSONArray("packages")
            for (index in 0 until packages.length()) {
                val fact = packages.getJSONObject(index)
                assertTrue(fact.toString(), fact.has("version_code") && fact.has("system"))
                names += fact.getString("package_name")
            }
            if (!page.getBoolean("truncated")) {
                assertFalse(page.has("next_after_package"))
                break
            }
            assertEquals(names.last(), page.getString("next_after_package"))
            page = result(
                submit(
                    runtime,
                    android(
                        "package",
                        JSONObject().put("operation", "list").put("include_system", true)
                            .put("limit", 200).put("after_package", names.last()),
                    ),
                ),
            )
            assertTrue(++guard < 100)
        }
        assertEquals(names.sorted(), names)
        assertEquals(names.toSet().size, names.size)
        assertTrue(names.containsAll(listOf("android", context.packageName, SHIZUKU_MANAGER)))
    }

    @Test
    fun I8_ANDROID_G02_forceStopUsesOnlyPrivilegedExecution() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val own = submit(runtime, forceStop(context.packageName))
        val other = submit(runtime, forceStop(SETTINGS))
        if (fixture == FRAMEWORK) {
            assertEquals(own.toString(), "CAPABILITY_UNAVAILABLE", errorCode(own))
            assertEquals(other.toString(), "CAPABILITY_UNAVAILABLE", errorCode(other))
        } else {
            assertEquals(own.toString(), "INVALID_ARGUMENT", errorCode(own))
            val stopped = result(other)
            assertEquals(stopped.toString(), "force_stop", stopped.getString("operation"))
            assertEquals(stopped.toString(), SETTINGS, stopped.getString("package_name"))
            assertEquals(stopped.toString(), true, stopped.getBoolean("completed"))
        }
    }

    @Test
    fun I8_ANDROID_G03_packageLaunchWorksWithoutBroadPackageVisibility() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val launched = result(submit(runtime, android("launch", JSONObject().put("operation", "package").put("package_name", SETTINGS))))
        assertEquals(true, launched.getBoolean("launched"))
        assertEquals(SETTINGS, launched.getString("package_name"))
        val inspect = submit(runtime, android("package", JSONObject().put("operation", "inspect").put("package_name", context.packageName)))
        assertEquals(context.packageName, result(inspect).getJSONObject("package").getString("package_name"))
        val absent = submit(runtime, android("package", JSONObject().put("operation", "inspect").put("package_name", "com.droidbridge.absent.fixture")))
        assertEquals(
            absent.toString(),
            if (fixture == FRAMEWORK) "CAPABILITY_UNAVAILABLE" else "NOT_FOUND",
            errorCode(absent),
        )
        InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand("input keyevent HOME").close()
    }

    @Test
    fun I8_ANDROID_G04_G05_clipboardKeepsItsProviderSemantics() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val status = result(submit(runtime, contextStatusRequest()))
        val magiskClipboard = grantState(status, "magisk.clipboard") == "available"
        val text = "droidbridge-i8-${UUID.randomUUID()}"
        val written = result(submit(runtime, android("clipboard", JSONObject().put("operation", "write").put("text", text))))
        assertEquals(written.toString(), "write", written.getString("operation"))
        assertEquals(written.toString(), true, written.getBoolean("written"))
        val read = result(submit(runtime, android("clipboard", JSONObject().put("operation", "read"))))
        if (fixture == MAGISK && magiskClipboard) {
            assertEquals(read.toString(), text, read.getString("text"))
        } else if (read.getBoolean("has_text")) {
            assertEquals(read.toString(), text, read.getString("text"))
        } else {
            assertFalse(read.toString(), read.has("text"))
        }
        val cleared = result(submit(runtime, android("clipboard", JSONObject().put("operation", "clear"))))
        assertEquals(cleared.toString(), "clear", cleared.getString("operation"))
        assertEquals(cleared.toString(), true, cleared.getBoolean("cleared"))
        val after = result(submit(runtime, contextStatusRequest()))
        assertEquals(grantState(status, "android.notification_listener"), grantState(after, "android.notification_listener"))
    }

    @Test
    fun I8_ANDROID_G06_G07_notificationAccessIsTruthfulAndRefsNeverRetarget() = withRuntime { runtime ->
        awaitAdmittedFixture(runtime)
        val systemGranted = context.getSystemService(NotificationManager::class.java)
            .isNotificationListenerAccessGranted(ComponentName(context.packageName, LISTENER_SERVICE))
        var status = result(submit(runtime, contextStatusRequest()))
        if (systemGranted && grantState(status, "magisk.notifications") != "available") {
            val deadline = SystemClock.elapsedRealtime() + LISTENER_BIND_DEADLINE_MS
            while (grantState(status, "android.notification_listener") != "available") {
                assertTrue(status.toString(), SystemClock.elapsedRealtime() < deadline)
                SystemClock.sleep(250)
                status = result(submit(runtime, contextStatusRequest()))
            }
        }
        val listener = grantState(status, "android.notification_listener")
        val magisk = grantState(status, "magisk.notifications")
        if (!systemGranted) {
            assertNotEquals("the listener grant is never fabricated -> $status", "available", listener)
        }
        Log.i(EVIDENCE_TAG, "notification systemGranted=$systemGranted listener=$listener magisk=$magisk")
        val first = submit(runtime, android("notification", JSONObject().put("operation", "list").put("limit", 100)))
        if (listener != "available" && magisk != "available") {
            assertEquals(first.toString(), "CAPABILITY_UNAVAILABLE", errorCode(first))
            return@withRuntime
        }
        result(first)
        try {
            post("first")
            val original = awaitOwnNotification(runtime) { it.optString("title") == "first" }
            post("replacement")
            awaitOwnNotification(runtime) { it.optString("title") == "replacement" }
            val stale = submit(runtime, android("notification", JSONObject().put("operation", "get").put("notification_ref", original)))
            assertEquals(stale.toString(), "STALE_REFERENCE", errorCode(stale))
            val staleDismiss = submit(runtime, android("notification", JSONObject().put("operation", "dismiss").put("notification_ref", original)))
            assertEquals(staleDismiss.toString(), "STALE_REFERENCE", errorCode(staleDismiss))
            assertTrue(fixturePosted())
            val current = awaitOwnNotification(runtime) { it.optString("title") == "replacement" }
            assertNotEquals(original, current)
            val dismissed = result(submit(runtime, android("notification", JSONObject().put("operation", "dismiss").put("notification_ref", current))))
            assertTrue(dismissed.getBoolean("dismissed"))
            val deadline = SystemClock.elapsedRealtime() + 10_000
            while (fixturePosted()) {
                assertTrue(SystemClock.elapsedRealtime() < deadline)
                SystemClock.sleep(100)
            }
            val after = result(submit(runtime, contextStatusRequest()))
            assertEquals(listener, grantState(after, "android.notification_listener"))
        } finally {
            shell("cmd notification cancel $NOTIFICATION_TAG")
        }
    }

    @Test
    fun I8_ANDROID_G08_sensitiveAndroidPayloadsStayOutOfOrdinaryLogs() = withRuntime { runtime ->
        awaitAdmittedFixture(runtime)
        val secret = "droidbridge-secret-${UUID.randomUUID()}"
        submit(runtime, android("clipboard", JSONObject().put("operation", "write").put("text", secret)))
        submit(
            runtime,
            android(
                "intent",
                JSONObject().put("operation", "explicit_activity").put("package_name", "com.droidbridge.absent.fixture")
                    .put("class_name", "com.droidbridge.Absent").put("extras", JSONObject().put("token", secret)),
            ),
        )
        submit(runtime, android("clipboard", JSONObject().put("operation", "clear")))
        val log = shell("logcat -d -t 5000")
        assertTrue(log.isNotEmpty())
        assertFalse(log.contains(secret))
    }

    @Test
    fun I8_ANDROID_G09_eachMagiskFamilyIsPublishedFromItsOwnProbe() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val status = result(submit(runtime, contextStatusRequest()))
        Log.i(
            EVIDENCE_TAG,
            listOf(
                "magisk.framework", "magisk.launch", "magisk.clipboard", "magisk.notifications",
                "android.notification_listener", "shizuku.shell", "execution.app_guard",
                "execution.shell_guard",
            ).joinToString(prefix = "fixture=$fixture ") {
                "$it=${grantState(status, it)}" +
                    (status.optJSONObject("grants")?.optJSONObject(it)?.optString("reason")
                        ?.takeIf(String::isNotEmpty)?.let { reason -> "($reason)" } ?: "")
            },
        )
        if (fixture != MAGISK) {
            listOf("magisk.launch", "magisk.clipboard", "magisk.notifications").forEach { key ->
                assertNotEquals(status.toString(), "available", grantState(status, key))
            }
            return@withRuntime
        }
        assertEquals("available", grantState(status, "magisk.framework"))
        val families = listOf("magisk.launch", "magisk.clipboard", "magisk.notifications")
            .associateWith { grantState(status, it) }
        assertTrue(families.toString(), families.values.all { it == "available" || it == "unavailable" })
        if (families.getValue("magisk.launch") == "available") {
            result(submit(runtime, android("launch", JSONObject().put("operation", "package").put("package_name", SETTINGS))))
            InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand("input keyevent HOME").close()
        }
        if (families.getValue("magisk.clipboard") == "available") {
            result(submit(runtime, android("clipboard", JSONObject().put("operation", "clear"))))
        }
        if (families.getValue("magisk.notifications") == "available") {
            result(submit(runtime, android("notification", JSONObject().put("operation", "list"))))
        }
        val after = result(submit(runtime, contextStatusRequest()))
        families.keys.forEach { key -> assertEquals(key, families.getValue(key), grantState(after, key)) }
    }

    /** The fixture notification is owned by the shell package, never by the product App. */
    private fun post(title: String) {
        shell("cmd notification post -t $title $NOTIFICATION_TAG fixture")
    }

    private fun fixturePosted(): Boolean =
        shell("dumpsys notification --noredact").contains("|$NOTIFICATION_TAG|")

    private fun awaitOwnNotification(runtime: IDroidBridgeRuntime, matches: (JSONObject) -> Boolean): String {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (true) {
            val notifications = result(
                submit(runtime, android("notification", JSONObject().put("operation", "list").put("limit", 100))),
            ).getJSONArray("notifications")
            for (index in 0 until notifications.length()) {
                val summary = notifications.getJSONObject(index)
                if (summary.getString("package_name") == SHELL_PACKAGE && matches(summary)) {
                    return summary.getString("notification_ref")
                }
            }
            assertTrue("own notification was not observed", SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(200)
        }
    }

    private fun shell(command: String): String =
        ParcelFileDescriptor.AutoCloseInputStream(
            InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(command),
        ).use { it.readBytes().toString(Charsets.UTF_8) }

    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments().getString("i8AndroidFixture") ?: SHIZUKU
        require(fixture in setOf(FRAMEWORK, SHIZUKU, MAGISK)) { "unknown I8 Android fixture: $fixture" }
        val deadline = SystemClock.elapsedRealtime() + 75_000
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            if (fixtureAdmitted(status, fixture)) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) error("I8 Android fixture $fixture was not admitted: $status")
            SystemClock.sleep(100)
        }
    }

    private fun fixtureAdmitted(status: JSONObject, fixture: String): Boolean {
        val result = status.optJSONObject("result") ?: return false
        val host = result.optJSONObject("runtime")?.optString("host")
        return when (fixture) {
            FRAMEWORK -> host == "apk_runtime" && grantState(result, "shizuku.shell") == "unavailable"
            SHIZUKU -> host == "apk_runtime" &&
                grantState(result, "shizuku.shell") == "available" &&
                grantState(result, "execution.shell_guard") == "available"
            else -> host == "magisk_backend" && grantState(result, "magisk.module") == "available"
        }
    }

    private fun grantState(status: JSONObject, key: String): String? =
        status.optJSONObject("grants")?.optJSONObject(key)?.optString("state")

    private fun result(response: JSONObject): JSONObject {
        assertEquals(response.toString(), "success", response.getString("outcome"))
        return response.getJSONObject("result")
    }

    private fun errorCode(response: JSONObject): String? =
        response.optJSONObject("error")?.optString("code")

    private fun forceStop(packageName: String): ByteArray =
        android("package", JSONObject().put("operation", "force_stop").put("package_name", packageName))

    private fun android(action: String, input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put("payload", JSONObject().put("tool", "android").put("action", action).put("input", input))
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun contextStatusRequest(): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject().put("tool", "context").put("action", "status").put("input", JSONObject().put("detail", "full")),
        )
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
        const val FRAMEWORK = "framework"
        const val SHIZUKU = "shizuku"
        const val MAGISK = "magisk"
        const val SETTINGS = "com.android.settings"
        const val SHIZUKU_MANAGER = "moe.shizuku.privileged.api"
        const val EVIDENCE_TAG = "I8AndroidGate"
        const val LISTENER_SERVICE = "com.droidbridge.android.runtimehost.DroidBridgeNotificationListenerService"
        const val LISTENER_BIND_DEADLINE_MS = 60_000L
        const val SHELL_PACKAGE = "com.android.shell"
        const val NOTIFICATION_TAG = "droidbridge_i8_ref"
    }
}
