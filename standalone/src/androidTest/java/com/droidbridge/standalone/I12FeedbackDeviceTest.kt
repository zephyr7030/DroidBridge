package com.droidbridge.standalone

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.IBinder
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.standalone.product.release.ReleaseConfig
import com.droidbridge.standalone.product.release.ReleaseHash
import com.droidbridge.standalone.runtimehost.AndroidApkSessionInstaller
import com.droidbridge.standalone.runtimehost.AndroidPackageFacts
import com.droidbridge.standalone.runtimehost.IDroidBridgeRuntime
import com.droidbridge.standalone.runtimehost.IRuntimeCallback
import com.droidbridge.standalone.runtimehost.MaintenanceHost
import com.droidbridge.standalone.runtimehost.MaintenancePhase
import com.droidbridge.standalone.runtimehost.PackageInstallerResultReceiver
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceController
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceRecord
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceStore
import java.io.File
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class I12FeedbackDeviceTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun runtimeHasNoActiveTasksWhilePreservingRetainedHistory() = withRuntime { runtime ->
        val deadline = SystemClock.elapsedRealtime() + 45_000
        var status: JSONObject
        do {
            status = submit(runtime, "context", "status", JSONObject().put("detail", "full"))
            if (status.optString("outcome") == "success") break
            assertEquals("CAPABILITY_UNAVAILABLE", status.optJSONObject("error")?.optString("code"))
            SystemClock.sleep(100)
        } while (SystemClock.elapsedRealtime() < deadline)
        assertEquals(status.toString(), "success", status.getString("outcome"))
        assertEquals("apk_runtime", status.getJSONObject("result").getJSONObject("runtime").getString("host"))
        val tasks = submit(
            runtime, "task_control", "list",
            JSONObject().put("states", JSONArray(listOf("created", "queued", "running"))),
        )
        assertEquals(tasks.toString(), "success", tasks.getString("outcome"))
        assertEquals(tasks.toString(), 0, tasks.getJSONObject("result").getJSONArray("tasks").length())
    }

    @Test
    fun runtimeRefusalsPreserveCodesAndStagesWithoutCreatingMaintenance() = withRuntime { runtime ->
        val before = JSONObject(runtime.getUpdateMaintenance())
        assertTrue(before.isNull("record"))
        assertFalse(before.getBoolean("configured"))
        val id = UUID.randomUUID().toString()
        assertRefusal(runtime.beginProductUpdate(byteArrayOf(), byteArrayOf()), "CAPABILITY_UNAVAILABLE", "release_configuration")
        assertRefusal(runtime.installUpdateApk(id), "STALE_AUTHORITY", "validate_install")
        assertRefusal(runtime.cancelUpdate(id), "STALE_AUTHORITY", "validate_cancel")
        val after = JSONObject(runtime.getUpdateMaintenance())
        assertTrue(after.isNull("record"))
        assertEquals(before.getLong("installed_version_code"), after.getLong("installed_version_code"))
    }

    @Test
    fun installedReceiverIsPrivateAndRunsInTheRuntimeProcess() {
        val receiver = context.packageManager.getReceiverInfo(
            ComponentName(context, PackageInstallerResultReceiver::class.java),
            PackageManager.ComponentInfoFlags.of(0),
        )
        assertFalse(receiver.exported)
        assertEquals(context.packageName + ":runtime", receiver.processName)
    }

    @Test
    fun realPackageFactsReconcilePreparedMaintenanceAndKeepCacheReferencedUntilRecovery() {
        val root = File(context.cacheDir, "i12-feedback-" + UUID.randomUUID()).apply { check(mkdir()) }
        try {
            val base = File(root, "canonical")
            val cache = File(root, "updates")
            val facts = AndroidPackageFacts(context)
            val source = File(context.applicationInfo.sourceDir)
            val archive = requireNotNull(facts.archive(source))
            val version = requireNotNull(context.packageManager.getPackageInfo(
                context.packageName, PackageManager.PackageInfoFlags.of(0),
            ).versionName)
            val apk = File(File(cache, version), "droidbridge-" + version + "-arm64-v8a.apk")
            check(apk.parentFile!!.mkdirs())
            source.copyTo(apk)
            val now = System.currentTimeMillis()
            check(apk.setLastModified(now - TimeUnit.HOURS.toMillis(25)))
            val unused = File(cache, "unused.apk").apply {
                writeText("unused")
                check(setLastModified(now - TimeUnit.HOURS.toMillis(25)))
            }
            val record = UpdateMaintenanceRecord(
                UUID.randomUUID().toString(), version, archive.versionCode,
                ReleaseHash.sha256(apk), apk.length(), requireNotNull(archive.signerSha256),
                MaintenancePhase.Prepared, null,
            )
            val store = UpdateMaintenanceStore.android(base)
            store.create(record)
            var reopened = false
            val host = object : MaintenanceHost {
                override fun ensureApkHost(): String? = null
                override fun closeAdmission(): String? = null
                override fun reopenAdmission(): Boolean = true.also { reopened = true }
            }
            val controller = UpdateMaintenanceController(
                context.packageName, ReleaseConfig.Unconfigured, store, host, facts,
                AndroidApkSessionInstaller(context), cache,
            )
            controller.cleanupDownloads()
            assertTrue(apk.exists())
            assertFalse(unused.exists())
            File(base, UpdateMaintenanceStore.RECORD).writeText("invalid")
            assertRefusal(controller.refresh(), "IO_ERROR", "recover_maintenance")
            assertTrue(apk.exists())
            File(base, UpdateMaintenanceStore.RECORD).writeText(record.encode())
            controller.recover()
            assertNull(store.read())
            assertTrue(reopened)
            controller.cleanupDownloads()
            assertFalse(apk.exists())
        } finally {
            assertTrue(root.deleteRecursively())
            assertFalse(root.exists())
        }
    }

    private fun assertRefusal(reply: String, code: String, stage: String) {
        val value = JSONObject(reply)
        assertEquals(reply, 1, value.getInt("schema_version"))
        assertEquals(reply, code, value.getString("error"))
        assertEquals(reply, stage, value.getString("stage"))
    }

    private fun submit(runtime: IDroidBridgeRuntime, tool: String, action: String, input: JSONObject): JSONObject {
        val request = JSONObject()
            .put("protocol_version", 1)
            .put("request_id", UUID.randomUUID().toString())
            .put("payload", JSONObject().put("tool", tool).put("action", action).put("input", input))
            .toString().toByteArray()
        val completed = CountDownLatch(1)
        var response: ByteArray? = null
        runtime.submit(request, object : IRuntimeCallback.Stub() {
            override fun onResponse(value: ByteArray?) {
                response = value
                completed.countDown()
            }
        })
        assertTrue(completed.await(5, TimeUnit.SECONDS))
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
            ComponentName(context.packageName, "com.droidbridge.standalone.runtimehost.DroidBridgeService"),
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
