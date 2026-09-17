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
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * S-NET-002 device evidence on the shipped surface: one pinned Rustls configuration serves
 * `filesystem.download`, `network.diagnose tls` and the OpenAI tunnel, and it performs both
 * chain and hostname verification in whichever Runtime host the fixture admits.
 *
 * `-e i8FilesystemFixture` selects the host: `shizuku` is the APK Runtime with the shell identity
 * admitted, `magisk` is the Magisk backend. Both hosts build their client from the one pinned
 * configuration, so each fixture observes the same trust policy on its own surface.
 *
 * The download target is immutable: jsDelivr serves `rust-lang/rust` at the pinned commit, so
 * the body cannot change under this gate. The expected size and digest are the bytes the tool
 * must reproduce, not a value the tool reported.
 */
@RunWith(AndroidJUnit4::class)
class I8TlsPolicyDeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    /**
     * A public HTTPS download through the admitted host's own client. The destination is an
     * App-identity path, so this process hashes the bytes that actually landed on disk instead
     * of trusting the tool's self-report.
     */
    @Test
    fun I8_TLS_G01_publicHttpsDownloadPublishesTheVerifiedBytes() {
        withRuntime { runtime ->
            val fixture = awaitAdmittedHost(runtime)
            val destination = File(context.filesDir, "droidbridge-tls-${UUID.randomUUID()}.md")
            try {
                val downloaded = submit(
                    runtime,
                    filesystemRequest(
                        "download",
                        JSONObject()
                            .put("url", DOWNLOAD_URL)
                            .put("destination", pathTarget(destination.absolutePath))
                            .put("overwrite", false)
                            .put("timeout_ms", 30_000),
                    ),
                )
                assertEquals("$fixture $downloaded", "success", downloaded.getString("outcome"))
                val task = awaitTerminalTask(
                    runtime,
                    downloaded.getJSONObject("result").getString("task_id"),
                )
                val resolved = resolution(runtime, fixture)
                assertEquals(
                    "$fixture download task $task; $resolved",
                    "completed",
                    task.getString("state"),
                )
                val result = requireNotNull(task.optJSONObject("result")) { task.toString() }
                assertEquals("$fixture download task $task", EXPECTED_SIZE, result.getLong("size"))
                assertEquals("$fixture download task $task", EXPECTED_SHA256, result.getString("sha256"))

                // Ground truth, read by this process from the same file the tool published.
                assertTrue("$fixture download did not land: $destination", destination.isFile)
                assertEquals("$fixture on-disk size", EXPECTED_SIZE, destination.length())
                assertEquals("$fixture on-disk sha256", EXPECTED_SHA256, sha256Of(destination))

                val inspected = submit(
                    runtime,
                    filesystemRequest(
                        "inspect",
                        JSONObject()
                            .put("target", pathTarget(destination.absolutePath))
                            .put("recursive", false)
                            .put("max_depth", 1)
                            .put("max_entries", 10),
                    ),
                )
                assertEquals("$fixture $inspected", "success", inspected.getString("outcome"))
                assertEquals(
                    "$fixture $inspected",
                    EXPECTED_SIZE,
                    inspected.getJSONObject("result").getLong("size"),
                )
            } finally {
                submit(
                    runtime,
                    filesystemRequest(
                        "manage",
                        JSONObject()
                            .put("operation", "delete")
                            .put("target", pathTarget(destination.absolutePath))
                            .put("recursive", false),
                    ),
                )
                destination.delete()
            }
        }
    }

    /**
     * R-NET-009's `certificate_verified` is the probe's own claim about the same handshake, so
     * the pair is what makes the result non-vacuous: a configuration whose verification were
     * off would complete the mismatched probe too. The address probes repeat both verdicts with
     * the already resolved address as the connection target, so the host's TLS policy is
     * observable without depending on its resolver.
     */
    @Test
    fun I8_TLS_G02_theHostsTlsProbeVerifiesChainsAndHostnames() {
        withRuntime { runtime ->
            val fixture = awaitAdmittedHost(runtime)
            val verified = probe(runtime, PROBE_HOST, PROBE_HOST)
            assertEquals("$fixture $verified", "success", verified.getString("outcome"))
            assertTrue("$fixture $verified", verified.getBoolean("certificate_verified"))
            assertEquals("$fixture $verified", PROBE_HOST, verified.getString("server_name"))
            assertEquals("$fixture $verified", 443, verified.getInt("port"))
            val remoteIp = requireNotNull(verified.optString("remote_ip").takeIf { it.isNotEmpty() }) {
                "$fixture verified probe reported no remote address: $verified"
            }

            val mismatched = probe(runtime, PROBE_HOST, WRONG_SERVER_NAME)
            assertEquals(
                "$fixture a certificate that does not name $WRONG_SERVER_NAME must not pass: $mismatched",
                false,
                mismatched.getString("outcome") == "success",
            )
            assertFalse("$fixture $mismatched", mismatched.getBoolean("certificate_verified"))
            assertEquals("$fixture $mismatched", WRONG_SERVER_NAME, mismatched.getString("server_name"))

            val byAddress = probe(runtime, remoteIp, PROBE_HOST)
            assertEquals("$fixture $byAddress", "success", byAddress.getString("outcome"))
            assertTrue("$fixture $byAddress", byAddress.getBoolean("certificate_verified"))

            val unverifiable = probe(runtime, remoteIp, WRONG_SERVER_NAME)
            assertEquals(
                "$fixture $unverifiable",
                false,
                unverifiable.getString("outcome") == "success",
            )
            assertFalse("$fixture $unverifiable", unverifiable.getBoolean("certificate_verified"))
        }
    }

    /**
     * The same two verdicts with the connection pinned to an address literal, so the host's TLS
     * policy is observable on a device whose resolver hands the privileged host addresses it
     * cannot reach. The name still comes from the certificate, so a pass here is chain and
     * hostname verification, not a reachability result.
     */
    @Test
    fun I8_TLS_G03_theHostsTlsProbeVerifiesAChainWithoutTheResolver() {
        withRuntime { runtime ->
            val fixture = awaitAdmittedHost(runtime)
            val verified = probe(runtime, ADDRESS_PROBE_HOST, ADDRESS_PROBE_NAME)
            assertEquals("$fixture $verified", "success", verified.getString("outcome"))
            assertTrue("$fixture $verified", verified.getBoolean("certificate_verified"))
            assertEquals("$fixture $verified", ADDRESS_PROBE_NAME, verified.getString("server_name"))
            assertEquals("$fixture $verified", ADDRESS_PROBE_HOST, verified.getString("remote_ip"))

            val namesTheAddress = probe(runtime, ADDRESS_PROBE_HOST, ADDRESS_PROBE_HOST)
            assertEquals("$fixture $namesTheAddress", "success", namesTheAddress.getString("outcome"))
            assertTrue("$fixture $namesTheAddress", namesTheAddress.getBoolean("certificate_verified"))

            val mismatched = probe(runtime, ADDRESS_PROBE_HOST, WRONG_SERVER_NAME)
            assertEquals(
                "$fixture a certificate that does not name $WRONG_SERVER_NAME must not pass: $mismatched",
                false,
                mismatched.getString("outcome") == "success",
            )
            assertFalse("$fixture $mismatched", mismatched.getBoolean("certificate_verified"))
            assertEquals("$fixture $mismatched", WRONG_SERVER_NAME, mismatched.getString("server_name"))
        }
    }

    /** The host's own resolution, so a failed download states which address the host reached. */
    private fun resolution(runtime: IDroidBridgeRuntime, fixture: String): String {
        val response = submit(
            runtime,
            networkRequest(
                "diagnose",
                JSONObject().put("test", "dns").put("name", PROBE_HOST).put("record_type", "A"),
            ),
        )
        assertEquals(response.toString(), "success", response.getString("outcome"))
        val result = response.getJSONObject("result")
        return "$fixture resolved $PROBE_HOST to ${result.optJSONArray("addresses") ?: JSONArray()}" +
            " with ${result.getString("outcome")}"
    }

    private fun probe(runtime: IDroidBridgeRuntime, host: String, serverName: String): JSONObject {
        val response = submit(
            runtime,
            networkRequest(
                "diagnose",
                JSONObject()
                    .put("test", "tls")
                    .put("host", host)
                    .put("server_name", serverName)
                    .put("port", 443)
                    .put("timeout_ms", 15_000),
            ),
        )
        assertEquals(response.toString(), "success", response.getString("outcome"))
        return response.getJSONObject("result")
    }

    private fun awaitTerminalTask(runtime: IDroidBridgeRuntime, taskId: String): JSONObject {
        val deadline = SystemClock.elapsedRealtime() + TASK_DEADLINE_MS
        while (true) {
            val response = submit(runtime, taskControlRequest("get", taskId))
            assertEquals(response.toString(), "success", response.getString("outcome"))
            val snapshot = requireNotNull(response.optJSONObject("result")) { response.toString() }
            if (snapshot.getString("state") in TERMINAL_STATES) return snapshot
            if (SystemClock.elapsedRealtime() >= deadline) {
                error("task $taskId never reached a terminal state: $snapshot")
            }
            SystemClock.sleep(200)
        }
    }

    private fun sha256Of(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { stream ->
            val buffer = ByteArray(8 * 1024)
            while (true) {
                val read = stream.read(buffer)
                if (read <= 0) break
                digest.update(buffer, 0, read)
            }
        }
        return digest.digest().joinToString("") { byte -> "%02x".format(byte) }
    }

    // A freshly installed App runs the runtime before the fixture host and its grants are
    // admitted, and submissions during that window are refused. Every gate for this fixture
    // starts from the same admitted state instead of racing the host transition.
    private fun awaitAdmittedHost(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments()
            .getString("i8FilesystemFixture")
            ?: SHIZUKU_FIXTURE
        require(fixture in FIXTURES) { "unknown I8 filesystem fixture: $fixture" }
        if (fixture == SHIZUKU_FIXTURE &&
            InstrumentationRegistry.getArguments().getString("i8RequestShizuku") == "true"
        ) {
            assertTrue(runtime.requestShizukuAuthorization())
        }
        val deadline = SystemClock.elapsedRealtime() + ADMISSION_DEADLINE_MS
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
        val runtime = result.optJSONObject("runtime") ?: return false
        // A freshly started App publishes its host and grants before the runtime serves
        // submissions, so readiness is what closes the host-transition window.
        if (runtime.optString("readiness") != "ready") return false
        val host = runtime.optString("host")
        val grants = result.optJSONObject("grants") ?: return false
        return when (fixture) {
            SHIZUKU_FIXTURE -> host == "apk_runtime" &&
                grants.optJSONObject("shizuku.shell")?.optString("state") == "available" &&
                grants.optJSONObject("execution.shell_guard")?.optString("state") == "available"

            else -> host == "magisk_backend" &&
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

    private fun filesystemRequest(action: String, input: JSONObject): ByteArray =
        toolRequest("filesystem", action, input)

    private fun networkRequest(action: String, input: JSONObject): ByteArray =
        toolRequest("network", action, input)

    private fun taskControlRequest(action: String, taskId: String): ByteArray =
        toolRequest("task_control", action, JSONObject().put("task_id", taskId))

    private fun toolRequest(tool: String, action: String, input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject().put("tool", tool).put("action", action).put("input", input),
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
        const val SHIZUKU_FIXTURE = "shizuku"
        const val MAGISK_FIXTURE = "magisk"
        val FIXTURES = setOf(SHIZUKU_FIXTURE, MAGISK_FIXTURE)
        const val ADMISSION_DEADLINE_MS = 75_000L
        const val TASK_DEADLINE_MS = 90_000L
        const val PROBE_HOST = "cdn.jsdelivr.net"
        const val WRONG_SERVER_NAME = "droidbridge-tls-policy.invalid"

        // AliDNS's web endpoint: reachable as an address literal, its certificate covers
        // `*.alidns.com` and the address itself, and it is not a name this device resolves.
        const val ADDRESS_PROBE_HOST = "223.5.5.5"
        const val ADDRESS_PROBE_NAME = "dns.alidns.com"
        val TERMINAL_STATES = setOf("completed", "failed", "cancelled", "interrupted")

        // rust-lang/rust at commit 88d9e12ae178fab0fb5cc050a94da85685d449ea, served by
        // jsDelivr. Verified immutable for this gate: HTTP 200, 3304 bytes, this digest.
        const val DOWNLOAD_URL =
            "https://cdn.jsdelivr.net/gh/rust-lang/rust@88d9e12ae178fab0fb5cc050a94da85685d449ea/README.md"
        const val EXPECTED_SIZE = 3304L
        const val EXPECTED_SHA256 =
            "b3f6ef2fef88b98cb9ec013a5c86213095e53e40eb228679574e4d06517f33c8"
    }
}
