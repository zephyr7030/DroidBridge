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
import com.droidbridge.android.execution.android.ObservedNotification
import com.droidbridge.android.execution.android.VisiblePackageFact
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

    private fun notification(handle: String, remote: Boolean = true) = ObservedNotification(
        key = "0|chat|1",
        packageName = "com.example.chat",
        postedAtMillis = 1_000,
        title = "Alice",
        text = "See you",
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
}
