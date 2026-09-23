package com.droidbridge.android

import com.droidbridge.android.execution.android.ActivityStart
import com.droidbridge.android.execution.android.AndroidExecutionException
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidMotherToolAccess
import com.droidbridge.android.execution.android.AndroidMotherToolAdapter
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.NotificationActionFact
import com.droidbridge.android.execution.android.NotificationOperationSurface
import com.droidbridge.android.execution.android.NotificationPlatform
import com.droidbridge.android.execution.android.NativeAndroidExecutionDispatcher
import com.droidbridge.android.execution.android.ObservedNotification
import com.droidbridge.android.execution.android.VisiblePackageFact
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class I8_AndroidCoreTest {
    private class FakeAccess : AndroidMotherToolAccess {
        val calls = mutableListOf<String>()
        var clipboard: () -> String? = { null }

        override fun inspectPackage(packageName: String): VisiblePackageFact? {
            calls += "inspect"
            return null
        }

        override fun launchPackage(packageName: String) {
            calls += "launch:$packageName"
        }

        override fun startActivity(start: ActivityStart) {
            calls += "start:${start.action}:${start.packageName}/${start.className}:${start.extras}"
        }

        override fun readClipboardText(): String? {
            calls += "clipboard_read"
            return clipboard()
        }

        override fun writeClipboardText(text: String) {
            calls += "clipboard_write"
        }

        override fun clearClipboard() {
            calls += "clipboard_clear"
        }
    }

    private class FakePlatform : NotificationPlatform<String> {
        val cancelled = mutableListOf<String>()
        val sent = mutableListOf<Pair<String, Int>>()

        override fun cancel(key: String) {
            cancelled += key
        }

        override fun sendAction(handle: String, index: Int) {
            sent += handle to index
        }
    }

    private fun request(primitive: AndroidPrimitive, payload: String) = AndroidExecutionRequest(
        primitive = primitive,
        payload = payload.encodeToByteArray(),
        executionId = "11111111-1111-4111-8111-111111111111",
        runtimeEpoch = "22222222-2222-4222-8222-222222222222",
        hostGeneration = 4,
        runtimeInstanceId = "33333333-3333-4333-8333-333333333333",
    )

    private fun notification(
        handle: String,
        remote: Boolean = true,
        key: String = "0|chat|1",
        postedAtMillis: Long = 1_000,
    ) = ObservedNotification(
        key = key,
        packageName = "com.example.chat",
        postedAtMillis = postedAtMillis,
        title = "Alice",
        text = "See you",
        actionCount = 2,
        actions = listOf(
            NotificationActionFact("Reply", requiresRemoteInput = remote),
            NotificationActionFact("Mark read", requiresRemoteInput = false),
        ),
        handle = handle,
    )

    private fun code(block: suspend () -> Unit): String =
        assertThrows(AndroidExecutionException::class.java) { runBlocking { block() } }.code

    @Test
    fun I8_ANDROID_G03_packageLaunchSendsTheFrontDoorSenderWithoutInspectingVisibility() = runBlocking {
        val access = FakeAccess()
        val adapter = AndroidMotherToolAdapter(access) { _, _, _ -> true }
        val result = adapter.execute(
            request(AndroidPrimitive.LaunchActivity, """{"operation":"package","package_name":"com.example.hidden"}"""),
        )
        assertEquals("{\"completed\":true}", result.payload.decodeToString())
        assertEquals(listOf("launch:com.example.hidden"), access.calls)
        adapter.execute(
            request(
                AndroidPrimitive.IntentStart,
                """{"operation":"explicit_activity","package_name":"com.example.a","class_name":"com.example.Main","extras":{"n":7,"b":true,"s":"x"}}""",
            ),
        )
        assertEquals(
            "start:android.intent.action.MAIN:com.example.a/com.example.Main:{n=7, b=true, s=x}",
            access.calls.last(),
        )
        assertEquals(
            "INVALID_ARGUMENT",
            code {
                adapter.execute(
                    request(
                        AndroidPrimitive.IntentStart,
                        """{"operation":"view","data_uri":"https://example.com","flags":268435456}""",
                    ),
                )
            },
        )
    }

    @Test
    fun I8_ANDROID_G04_appClipboardCollapsesObservationsButThrownFailuresStayExplicit() = runBlocking {
        val access = FakeAccess()
        val adapter = AndroidMotherToolAdapter(access) { _, _, _ -> true }
        val empty = adapter.execute(request(AndroidPrimitive.ClipboardRead, "{}"))
        assertEquals("{}", empty.payload.decodeToString())
        access.clipboard = { "copied" }
        val text = adapter.execute(request(AndroidPrimitive.ClipboardRead, "{}"))
        assertEquals("{\"text\":\"copied\"}", text.payload.decodeToString())
        access.clipboard = { throw SecurityException("denied") }
        assertEquals(
            "PERMISSION_DENIED",
            code { adapter.execute(request(AndroidPrimitive.ClipboardRead, "{}")) },
        )
        assertEquals(
            "STALE_AUTHORITY",
            code {
                AndroidMotherToolAdapter(access) { _, _, _ -> false }
                    .execute(request(AndroidPrimitive.ClipboardRead, "{}"))
            },
        )
    }

    @Test
    fun I8_ANDROID_G07_notificationReplacementInvalidatesOldGenerationsAndActions() = runBlocking {
        val platform = FakePlatform()
        val surface = NotificationOperationSurface(platform) { _, _, _ -> true }
        surface.connected(listOf(notification("first")))
        val snapshot = surface.execute(request(AndroidPrimitive.NotificationSnapshot, "{}"))
        assertTrue(snapshot.payload.decodeToString().contains("\"generation\":1"))

        surface.posted(notification("replacement"))
        assertEquals(
            "STALE_REFERENCE",
            code {
                surface.execute(
                    request(
                        AndroidPrimitive.NotificationAction,
                        """{"key":"0|chat|1","generation":1,"action_index":1}""",
                    ),
                )
            },
        )
        assertEquals(
            "STALE_REFERENCE",
            code {
                surface.execute(
                    request(AndroidPrimitive.NotificationDismiss, """{"key":"0|chat|1","generation":1}"""),
                )
            },
        )
        assertEquals(
            "UNSUPPORTED",
            code {
                surface.execute(
                    request(
                        AndroidPrimitive.NotificationAction,
                        """{"key":"0|chat|1","generation":2,"action_index":0}""",
                    ),
                )
            },
        )
        surface.execute(
            request(
                AndroidPrimitive.NotificationAction,
                """{"key":"0|chat|1","generation":2,"action_index":1}""",
            ),
        )
        assertEquals(listOf("replacement" to 1), platform.sent)
        assertEquals(emptyList<String>(), platform.cancelled)

        surface.removed("0|chat|1")
        assertEquals(
            "STALE_REFERENCE",
            code {
                surface.execute(
                    request(AndroidPrimitive.NotificationDismiss, """{"key":"0|chat|1","generation":2}"""),
                )
            },
        )
    }

    @Test
    fun notificationStateIsBoundedAndReconnectNeverReusesAGeneration() = runBlocking {
        val platform = FakePlatform()
        val surface = NotificationOperationSurface(platform) { _, _, _ -> true }
        surface.connected(
            (0..300).map { index ->
                notification("handle-$index", key = "key-$index", postedAtMillis = index.toLong())
            },
        )
        val snapshot = Json.parseToJsonElement(
            surface.execute(request(AndroidPrimitive.NotificationSnapshot, "{}")).payload.decodeToString(),
        ).jsonObject["notifications"]!!.jsonArray
        assertEquals(256, snapshot.size)
        assertEquals("key-300", snapshot.first().jsonObject["key"]!!.jsonPrimitive.content)
        assertEquals("key-45", snapshot.last().jsonObject["key"]!!.jsonPrimitive.content)

        val generation = snapshot.first().jsonObject["generation"]!!.jsonPrimitive.content.toLong()
        surface.disconnected()
        surface.connected(listOf(notification("replacement", key = "key-300", postedAtMillis = 301)))
        assertEquals(
            "STALE_REFERENCE",
            code {
                surface.execute(
                    request(
                        AndroidPrimitive.NotificationDismiss,
                        """{"key":"key-300","generation":$generation}""",
                    ),
                )
            },
        )
    }

    @Test
    fun taskActivityRelayRejectsStaleCountsAndReplaysTruthAfterServiceRecreation() {
        val seen = mutableListOf<Long>()
        NativeAndroidExecutionDispatcher.installTaskActivitySink(seen::add)
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-a", 2, Long.MAX_VALUE - 2)
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-a", 2, Long.MAX_VALUE - 1)
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-a", 1, Long.MAX_VALUE - 3)
        NativeAndroidExecutionDispatcher.installTaskActivitySink(null)
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-a", 0, Long.MAX_VALUE)
        NativeAndroidExecutionDispatcher.installTaskActivitySink(seen::add)
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-b", 3, 1)
        NativeAndroidExecutionDispatcher.installTaskActivitySink(null)

        assertEquals(listOf(2L, 0L, 3L), seen)
    }

    @Test
    fun onlyADepartedDaemonsOwnTaskCountIsForgotten() {
        val seen = mutableListOf<Long>()
        NativeAndroidExecutionDispatcher.installTaskActivitySink(seen::add)
        seen.clear()
        NativeAndroidExecutionDispatcher.daemonTaskActivityChanged("epoch-c", 2, 10)
        NativeAndroidExecutionDispatcher.forgetDaemonTaskActivity()
        NativeAndroidExecutionDispatcher.forgetDaemonTaskActivity()
        // The daemon is back with the store revision sequence it left behind.
        NativeAndroidExecutionDispatcher.daemonTaskActivityChanged("epoch-c", 2, 11)
        // A Runtime hosted here takes the count over, and its hold outlives that daemon.
        NativeAndroidExecutionDispatcher.taskActivityChanged("epoch-c", 1, 12)
        NativeAndroidExecutionDispatcher.forgetDaemonTaskActivity()
        NativeAndroidExecutionDispatcher.daemonTaskActivityChanged("epoch-c", 3, 13)
        NativeAndroidExecutionDispatcher.forgetDaemonTaskActivity()
        NativeAndroidExecutionDispatcher.installTaskActivitySink(null)

        assertEquals(listOf(2L, 0L, 2L, 1L, 3L, 0L), seen)
    }
}
