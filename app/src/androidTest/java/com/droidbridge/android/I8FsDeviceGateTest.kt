package com.droidbridge.android

import android.content.ComponentName
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.SystemClock
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.runtimehost.IDroidBridgeRuntime
import com.droidbridge.android.runtimehost.IRuntimeCallback
import java.io.File
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
class I8FsDeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun I8_FS_G02_contentResolverInspectAndReadUseTheAdmittedFrameworkExecutor() {
        val resolver = context.contentResolver
        val uri = requireNotNull(
            resolver.insert(
                MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                ContentValues().apply {
                    put(MediaStore.MediaColumns.DISPLAY_NAME, "droidbridge-i8-${UUID.randomUUID()}.txt")
                    put(MediaStore.MediaColumns.MIME_TYPE, "text/plain")
                    put(MediaStore.MediaColumns.RELATIVE_PATH, "Download/DroidBridgeTests")
                },
            ),
        )
        try {
            resolver.openOutputStream(uri, "w")!!.use { it.write("content-bridge".encodeToByteArray()) }
            withRuntime { runtime ->
                awaitAdmittedFixture(runtime)
                val target = JSONObject().put("type", "content_uri").put("value", uri.toString())
                val inspect = submit(
                    runtime,
                    request(
                        "inspect",
                        JSONObject()
                            .put("target", target)
                            .put("recursive", false)
                            .put("max_depth", 1)
                            .put("max_entries", 10),
                    ),
                )
                assertEquals(inspect.toString(), "success", inspect.getString("outcome"))
                val inspected = inspect.getJSONObject("result")
                assertEquals("file", inspected.getString("type"))
                assertEquals(14, inspected.getLong("size"))
                assertFalse(inspected.has("entries"))
                assertFalse(inspected.has("truncated"))

                val read = submit(
                    runtime,
                    request(
                        "read",
                        JSONObject()
                            .put("target", target)
                            .put("offset", 0)
                            .put("max_bytes", 64)
                            .put("encoding", "utf8"),
                    ),
                )
                assertEquals(read.toString(), "success", read.getString("outcome"))
                val result = read.getJSONObject("result")
                assertEquals("content-bridge", result.getString("data"))
                assertEquals(14, result.getLong("returned_bytes"))
                assertFalse(result.getBoolean("truncated"))
                assertTrue(result.has("sha256"))
            }
        } finally {
            resolver.delete(uri, null, null)
        }
    }

    @Test
    fun I8_FS_G01_privilegedFixtureUsesTheSelectedRuntimeHost() {
        withRuntime { runtime ->
            val fixture = awaitAdmittedFixture(runtime)
            val expectedHost: String
            val fixturePath: String
            val fixtureContent: String
            val mutationParent: String
            when (fixture) {
                "shizuku" -> {
                    expectedHost = "apk_runtime"
                    fixturePath = SHIZUKU_FIXTURE_PATH
                    fixtureContent = "shell-bridge"
                    mutationParent = "/data/local/tmp"
                }
                "magisk" -> {
                    expectedHost = "magisk_backend"
                    fixturePath = MAGISK_FIXTURE_PATH
                    fixtureContent = "root-bridge"
                    mutationParent = "/data/adb"
                }
                else -> error("unknown I8 filesystem fixture: $fixture")
            }
            val status = submit(runtime, contextStatusRequest())
            val result = requireNotNull(status.optJSONObject("result")) { status.toString() }
            assertEquals(status.toString(), expectedHost, result.getJSONObject("runtime").getString("host"))
            val grants = result.getJSONObject("grants")
            if (fixture == "shizuku") {
                assertEquals(
                    status.toString(),
                    "available",
                    grants.getJSONObject("shizuku.shell").getString("state"),
                )
                assertEquals(
                    status.toString(),
                    "available",
                    grants.getJSONObject("execution.shell_guard").getString("state"),
                )
            } else {
                assertEquals(
                    status.toString(),
                    "available",
                    grants.getJSONObject("magisk.module").getString("state"),
                )
            }

            val read = submit(
                runtime,
                request(
                    "read",
                    JSONObject()
                        .put(
                            "target",
                            JSONObject()
                                .put("type", "path")
                                .put("value", fixturePath),
                        )
                        .put("offset", 0)
                        .put("max_bytes", 64)
                        .put("encoding", "utf8"),
                ),
            )
            assertEquals(read.toString(), "success", read.getString("outcome"))
            assertEquals(fixtureContent, read.getJSONObject("result").getString("data"))
            assertSuccess(
                submit(
                    runtime,
                    request(
                        "inspect",
                        JSONObject()
                            .put("target", pathTarget(mutationParent))
                            .put("recursive", false)
                            .put("max_depth", 1)
                            .put("max_entries", 10),
                    ),
                ),
            )

            val directory = "$mutationParent/droidbridge-i8-${UUID.randomUUID()}"
            val source = "$directory/source.txt"
            val copied = "$directory/copied.txt"
            val moved = "$directory/moved.txt"
            val protectedDirectory = "$directory/protected"
            val replacementSource = "$directory/replacement.txt"
            try {
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "mkdir")
                                .put("target", pathTarget(directory))
                                .put("parents", false),
                        ),
                    ),
                )
                val created = submit(
                    runtime,
                    request(
                        "write",
                        JSONObject()
                            .put("mode", "create")
                            .put("target", pathTarget(source))
                            .put("content", "first")
                            .put("encoding", "utf8"),
                    ),
                )
                assertSuccess(created)
                assertEquals(5, created.getJSONObject("result").getLong("bytes_written"))
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "write",
                            JSONObject()
                                .put("mode", "replace")
                                .put("target", pathTarget(source))
                                .put("content", "second")
                                .put("encoding", "utf8"),
                        ),
                    ),
                )
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "mkdir")
                                .put("target", pathTarget(protectedDirectory))
                                .put("parents", false),
                        ),
                    ),
                )
                for ((path, content) in listOf(
                    "$protectedDirectory/keep.txt" to "keep",
                    replacementSource to "replacement",
                )) {
                    assertSuccess(
                        submit(
                            runtime,
                            request(
                                "write",
                                JSONObject()
                                    .put("mode", "create")
                                    .put("target", pathTarget(path))
                                    .put("content", content)
                                    .put("encoding", "utf8"),
                            ),
                        ),
                    )
                }
                val rejectedReplacement = submit(
                    runtime,
                    request(
                        "manage",
                        JSONObject()
                            .put("operation", "move")
                            .put("source", pathTarget(replacementSource))
                            .put("destination", pathTarget(protectedDirectory))
                            .put("recursive", false)
                            .put("overwrite", true),
                    ),
                )
                assertEquals(
                    rejectedReplacement.toString(),
                    "error",
                    rejectedReplacement.getString("outcome"),
                )
                assertEquals(
                    rejectedReplacement.toString(),
                    "UNSUPPORTED",
                    rejectedReplacement.getJSONObject("error").getString("code"),
                )
                val preserved = submit(
                    runtime,
                    request(
                        "read",
                        JSONObject()
                            .put("target", pathTarget("$protectedDirectory/keep.txt"))
                            .put("offset", 0)
                            .put("max_bytes", 64)
                            .put("encoding", "utf8"),
                    ),
                )
                assertSuccess(preserved)
                assertEquals("keep", preserved.getJSONObject("result").getString("data"))
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "write",
                            JSONObject()
                                .put("mode", "edit")
                                .put("target", pathTarget(source))
                                .put(
                                    "replacements",
                                    org.json.JSONArray().put(
                                        JSONObject().put("old", "second").put("new", "edited"),
                                    ),
                                ),
                        ),
                    ),
                )
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "copy")
                                .put("source", pathTarget(source))
                                .put("destination", pathTarget(copied))
                                .put("recursive", false)
                                .put("overwrite", false),
                        ),
                    ),
                )
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "move")
                                .put("source", pathTarget(copied))
                                .put("destination", pathTarget(moved))
                                .put("recursive", false)
                                .put("overwrite", false),
                        ),
                    ),
                )
                val verified = submit(
                    runtime,
                    request(
                        "read",
                        JSONObject()
                            .put("target", pathTarget(moved))
                            .put("offset", 0)
                            .put("max_bytes", 64)
                            .put("encoding", "utf8"),
                    ),
                )
                assertSuccess(verified)
                assertEquals("edited", verified.getJSONObject("result").getString("data"))
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "delete")
                                .put("target", pathTarget(moved))
                                .put("recursive", false),
                        ),
                    ),
                )
            } finally {
                submit(
                    runtime,
                    request(
                        "manage",
                        JSONObject()
                            .put("operation", "delete")
                            .put("target", pathTarget(directory))
                            .put("recursive", true),
                    ),
                )
            }
        }
    }

    // The App candidate owns an absolute path under its own data directory, so this is the only
    // place where the App identity commits a new regular file through the real kernel: no host
    // transition, no Shizuku fallback and no fixture path can substitute for it.
    @Test
    fun I8_FS_G01_appPathCreatePublishesExclusivelyUnderTheAppIdentity() {
        withRuntime { runtime ->
            val fixture = awaitAdmittedFixture(runtime)
            val directory = File(context.filesDir, "droidbridge-i8-${UUID.randomUUID()}")
            val target = File(directory, "created.txt")
            try {
                assertSuccess(
                    submit(
                        runtime,
                        request(
                            "manage",
                            JSONObject()
                                .put("operation", "mkdir")
                                .put("target", pathTarget(directory.absolutePath))
                                .put("parents", false),
                        ),
                    ),
                )
                val created = submit(
                    runtime,
                    request(
                        "write",
                        JSONObject()
                            .put("mode", "create")
                            .put("target", pathTarget(target.absolutePath))
                            .put("content", "exclusive")
                            .put("encoding", "utf8"),
                    ),
                )
                assertEquals("$fixture $created", "success", created.getString("outcome"))
                assertEquals(9, created.getJSONObject("result").getLong("bytes_written"))
                assertEquals("exclusive", target.readText())
                val collision = submit(
                    runtime,
                    request(
                        "write",
                        JSONObject()
                            .put("mode", "create")
                            .put("target", pathTarget(target.absolutePath))
                            .put("content", "replacement")
                            .put("encoding", "utf8"),
                    ),
                )
                // A non-overwriting create on an existing target is rejected either by the identity
                // preflight, which admits no executor for it, or by the exclusive commit itself; the
                // contract fixes the rejection and the preserved content, not which of the two reports it.
                assertEquals("$fixture $collision", "error", collision.getString("outcome"))
                assertEquals("exclusive", target.readText())
            } finally {
                submit(
                    runtime,
                    request(
                        "manage",
                        JSONObject()
                            .put("operation", "delete")
                            .put("target", pathTarget(directory.absolutePath))
                            .put("recursive", true),
                    ),
                )
            }
        }
    }

    private fun assertSuccess(response: JSONObject) {
        assertEquals(response.toString(), "success", response.getString("outcome"))
    }

    // A freshly installed App runs the runtime before the fixture host and its grants are admitted,
    // and submissions during that window are refused. Every gate for this fixture starts from the
    // same admitted state instead of racing the host transition.
    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments()
            .getString("i8FilesystemFixture")
            ?: "shizuku"
        require(fixture == "shizuku" || fixture == "magisk") {
            "unknown I8 filesystem fixture: $fixture"
        }
        if (fixture == "shizuku" &&
            InstrumentationRegistry.getArguments().getString("i8RequestShizuku") == "true"
        ) {
            assertTrue(runtime.requestShizukuAuthorization())
        }
        val deadline = SystemClock.elapsedRealtime() + 75_000
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            if (fixtureAdmitted(status, fixture)) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) {
                error("I8 filesystem fixture $fixture was not admitted: $status")
            }
            SystemClock.sleep(100)
        }
    }

    private fun fixtureAdmitted(status: JSONObject, fixture: String): Boolean {
        val result = status.optJSONObject("result") ?: return false
        val host = result.optJSONObject("runtime")?.optString("host")
        val grants = result.optJSONObject("grants") ?: return false
        return if (fixture == "shizuku") {
            host == "apk_runtime" &&
                grants.optJSONObject("shizuku.shell")?.optString("state") == "available" &&
                grants.optJSONObject("execution.shell_guard")?.optString("state") == "available"
        } else {
            host == "magisk_backend" &&
                grants.optJSONObject("magisk.module")?.optString("state") == "available"
        }
    }

    private fun pathTarget(path: String): JSONObject =
        JSONObject().put("type", "path").put("value", path)

    private fun contextStatusRequest(): ByteArray = JSONObject()
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
        .toByteArray(Charsets.UTF_8)

    private fun request(action: String, input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject().put("tool", "filesystem").put("action", action).put("input", input),
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
        assertTrue(latch.await(30, TimeUnit.SECONDS))
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
            ComponentName(
                context.packageName,
                "com.droidbridge.android.runtimehost.DroidBridgeService",
            ),
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
        const val SHIZUKU_FIXTURE_PATH = "/data/local/tmp/droidbridge-i8-shizuku.txt"
        const val MAGISK_FIXTURE_PATH = "/data/adb/droidbridge-i8-magisk.txt"
    }
}
