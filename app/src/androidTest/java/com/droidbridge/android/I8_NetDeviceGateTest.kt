package com.droidbridge.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.runtimehost.IDroidBridgeRuntime
import com.droidbridge.android.runtimehost.IRuntimeCallback
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I8-NET device evidence. Every gate here reads only the public `context.status` and
 * `network` entry points, so what it proves is the shipped surface rather than a projection
 * built for the test.
 */
@RunWith(AndroidJUnit4::class)
class I8_NetDeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    /**
     * S-NET-001/R-NET-002: every requested family is reported with the availability entry of
     * the source that actually owns it, a family's data array is present exactly when its
     * state is `available`, and `truncated` mirrors exactly the present arrays. The Magisk
     * host owns all four families from daemon-native providers; the APK host owns
     * interfaces/routes/dns from App/framework facts and consults Shizuku only for the
     * read-only socket supplement, which is therefore unavailable exactly when the Shizuku
     * grant is.
     */
    @Test
    fun I8_NET_G05_everyRequestedFamilyReportsTheAvailabilityOfItsOwnSource() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val status = requireNotNull(submit(runtime, contextStatusRequest()).optJSONObject("result"))
        assertEquals(
            "available",
            status.getJSONObject("capabilities").getJSONObject("network.inspect").getString("state"),
        )

        val responses = SCOPES.associateWith { scope ->
            submit(runtime, networkRequest("inspect", inspectInput(scope, 200)))
        }
        assertEquals(
            SCOPES.joinToString("; ") { "$it=${responses.getValue(it)}" },
            emptyList<String>(),
            SCOPES.filter { responses.getValue(it).getString("outcome") != "success" },
        )
        val result = responses.getValue("all").getJSONObject("result")
        assertEquals(FAMILIES.toSet(), result.getJSONObject("availability").keys().asSequence().toSet())

        val present = FAMILIES.filter { family -> result.has(family) }
        for (family in FAMILIES) {
            val availability = result.getJSONObject("availability").getJSONObject(family)
            val state = availability.getString("state")
            assertTrue("$family state $state -> $result", state in STATES)
            assertEquals("$family data array presence -> $result", family in present, state == "available")
        }
        assertEquals(
            "truncated mirrors the present data arrays -> $result",
            present.toSet(),
            result.getJSONObject("truncated").keys().asSequence().toSet(),
        )
        // `all` is the union of the four assignments S-NET-001 fixes.
        for (scope in FAMILIES) {
            val one = responses.getValue(scope).getJSONObject("result")
            assertEquals(
                "$scope is not the family `all` reported",
                result.optJSONArray(scope)?.length() ?: -1,
                one.optJSONArray(scope)?.length() ?: -1,
            )
            assertEquals(
                "$scope availability",
                result.getJSONObject("availability").getJSONObject(scope).toString(),
                one.getJSONObject("availability").getJSONObject(scope).toString(),
            )
        }

        if (fixture == MAGISK_FIXTURE) {
            // S-AUTH-NET-001 gives the Magisk executor every family from daemon-native
            // providers, so the APK-hosted supplements cannot appear here.
            assertEquals(FAMILIES.toSet(), present.toSet())
        } else {
            val sockets = result.getJSONObject("availability").getJSONObject("sockets")
            val shizuku = status.getJSONObject("grants").getJSONObject("shizuku.shell").getString("state")
            assertEquals(
                "the socket family is owned by the Shizuku supplement -> sockets=$sockets " +
                    "grants=${status.getJSONObject("grants")}",
                shizuku == "available",
                sockets.getString("state") == "available",
            )
            if (sockets.getString("state") != "available") {
                assertEquals("SHIZUKU_UNAVAILABLE", sockets.getString("reason"))
                assertFalse(result.has("sockets"))
                assertFalse(result.getJSONObject("truncated").has("sockets"))
            }
        }

        // R-NET-002: an available array is a fact the source established. A device with a
        // default network has interfaces and a configured resolver.
        for (family in present - "sockets") {
            assertTrue("$family is empty -> $result", result.getJSONArray(family).length() > 0)
        }
        for (entry in entries(result.getJSONArray("interfaces"))) {
            assertTrue(entry.getString("name").isNotEmpty())
            assertTrue(entry.has("addresses"))
        }
    }

    /**
     * R-NET-011/S-AUTH-NET-001: the API37 local-network grant gates only the APK executor's
     * LAN targets. A Magisk-served diagnosis is never subject to the App grant, a loopback
     * or named target is not a LAN feature at all, and a negative outcome never moves the
     * request to the other host surface.
     */
    @Test
    fun I8_NET_G06_api37LanAccessIsAGrantOnTheAppExecutorOnly() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        val status = requireNotNull(submit(runtime, contextStatusRequest()).optJSONObject("result"))
        val host = status.getJSONObject("runtime").getString("host")
        val grants = status.getJSONObject("grants")
        val capabilities = status.getJSONObject("capabilities")
        val lanTarget = lanTarget(runtime)

        val appExecutor = host == "apk_runtime" && fixture != MAGISK_FIXTURE
        val granted = grants.getJSONObject("android.local_network").getString("state") == "available"
        val gated = appExecutor && Build.VERSION.SDK_INT >= 37 && !granted
        if (appExecutor && Build.VERSION.SDK_INT >= 37) {
            // R-NET-011: without Magisk coverage the missing grant is an explicit capability
            // failure, and MCP/background calls never open the permission dialog. The
            // fixture therefore has to start from the ungranted state.
            assertFalse(
                "ACCESS_LOCAL_NETWORK is granted; revoke it before running this fixture",
                granted,
            )
            assertNotEquals(
                "network.local with the grant absent: $capabilities",
                "available",
                capabilities.getJSONObject("network.local").getString("state"),
            )
        }

        for (input in listOf(
            JSONObject().put("test", "route").put("destination_ip", lanTarget),
            JSONObject().put("test", "tcp").put("host", lanTarget).put("port", 80).put("timeout_ms", 2_000),
        )) {
            val response = submit(runtime, networkRequest("diagnose", input))
            if (gated) {
                assertEquals(response.toString(), "error", response.getString("outcome"))
                assertEquals(
                    response.toString(),
                    "CAPABILITY_UNAVAILABLE",
                    response.getJSONObject("error").getString("code"),
                )
            } else {
                assertEquals(response.toString(), "success", response.getString("outcome"))
                assertTrue(
                    response.toString(),
                    response.getJSONObject("result").getString("outcome") in OUTCOMES,
                )
            }
        }

        // R-NET-011 names the same-device loopback listener as not a LAN feature, and a
        // hostname is resolved by the provider, so neither may be refused by this gate.
        for (input in listOf(
            JSONObject().put("test", "tcp").put("host", LOOPBACK).put("port", 1).put("timeout_ms", 2_000),
            JSONObject().put("test", "dns").put("name", LOCAL_NAME),
            JSONObject().put("test", "connectivity"),
        )) {
            val response = submit(runtime, networkRequest("diagnose", input))
            assertEquals(response.toString(), "success", response.getString("outcome"))
            assertTrue(
                response.toString(),
                response.getJSONObject("result").getString("outcome") in OUTCOMES,
            )
            assertTrue(response.getJSONObject("result").has("duration_ms"))
        }

        // S-AUTH-NET-001: a request never changes host surface after a negative outcome.
        val after = requireNotNull(submit(runtime, contextStatusRequest()).optJSONObject("result"))
        assertEquals(host, after.getJSONObject("runtime").getString("host"))
        if (gated) {
            assertFalse(
                "the refused LAN request must not have obtained the grant",
                after.getJSONObject("grants").getJSONObject("android.local_network")
                    .getString("state") == "available",
            )
        }
    }

    /**
     * S-AUTH-NET-001/S-NET-004/R-NET-010: raw capture and injection belong to the Magisk
     * executor, the APK host refuses them as a capability failure rather than starting a
     * device, and every stated bound is refused before the executor is reached.
     */
    @Test
    fun I8_NET_G07_captureAndInjectionAreBoundedBeforeTheExecutorRuns() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)

        val built = successResult(
            runtime,
            "packet",
            JSONObject()
                .put("operation", "build")
                .put("ethernet", JSONObject().put("src_mac", SRC_MAC).put("dst_mac", DST_MAC))
                .put(
                    "network",
                    JSONObject().put("type", "ipv4").put("src", IPV4_SRC).put("dst", IPV4_DST),
                )
                .put(
                    "transport",
                    JSONObject().put("type", "udp").put("src_port", 40_000).put("dst_port", 53),
                )
                .put("payload_base64", PAYLOAD_BASE64),
        )
        assertEquals(BUILT_LENGTH, built.getLong("length"))
        assertEquals(64, built.getString("sha256").length)
        assertTrue(built.getString("packet_ref").isNotEmpty())

        // R-NET-007 -> R-NET-006: the built packet decodes back to the exact wire facts it
        // was built from, through the same shared wire owner.
        val decoded = successResult(
            runtime,
            "packet",
            JSONObject()
                .put("operation", "decode")
                .put("packet_ref", built.getString("packet_ref")),
        )
        assertEquals(BUILT_LENGTH, decoded.getLong("length"))
        assertEquals(SRC_MAC, decoded.getJSONObject("ethernet").getString("src_mac"))
        assertEquals(0x0800, decoded.getJSONObject("ethernet").getInt("ether_type"))
        assertEquals(IPV4_SRC, decoded.getJSONObject("ipv4").getString("src"))
        assertEquals(17, decoded.getJSONObject("ipv4").getInt("protocol"))
        assertEquals(40_000, decoded.getJSONObject("udp").getInt("src_port"))
        assertEquals(53, decoded.getJSONObject("udp").getInt("dst_port"))
        assertEquals(PAYLOAD_BYTES, decoded.getLong("payload_total_bytes"))
        assertFalse(decoded.getBoolean("payload_truncated"))

        // R-NET-007 caps the payload at 131,072 decoded bytes.
        val oversized = submit(
            runtime,
            networkRequest(
                "packet",
                JSONObject()
                    .put("operation", "build")
                    .put(
                        "network",
                        JSONObject().put("type", "ipv4").put("src", IPV4_SRC).put("dst", IPV4_DST),
                    )
                    .put(
                        "transport",
                        JSONObject().put("type", "udp").put("src_port", 1).put("dst_port", 2),
                    )
                    .put("payload_base64", base64Of(MAX_PACKET_BYTES + 1)),
            ),
        )
        assertEquals(oversized.toString(), "error", oversized.getString("outcome"))
        assertEquals(
            oversized.toString(),
            "INVALID_ARGUMENT",
            oversized.getJSONObject("error").getString("code"),
        )

        for (input in listOf(
            JSONObject()
                .put("operation", "start")
                .put("interface", "wlan0")
                .put("max_bytes", MAX_CAPTURE_BYTES + 1),
            JSONObject()
                .put("operation", "start")
                .put("interface", "wlan0")
                .put("max_packets", 1_000_001),
        )) {
            val refused = submit(runtime, networkRequest("capture", input))
            assertEquals(refused.toString(), "error", refused.getString("outcome"))
            assertEquals(
                refused.toString(),
                "INVALID_ARGUMENT",
                refused.getJSONObject("error").getString("code"),
            )
        }

        val capture = JSONObject().put("operation", "start").put("interface", "wlan0")
        val inject = JSONObject()
            .put("operation", "inject")
            .put("interface", "wlan0")
            .put("count", 1)
            .put("packet", JSONObject().put("packet_ref", built.getString("packet_ref")))
        if (fixture == MAGISK_FIXTURE) {
            // The Magisk host owns raw capture and injection.
            val injected = successResult(runtime, "packet", inject)
            assertEquals(1L, injected.getLong("requested_packets"))
            assertTrue(injected.getLong("accepted_packets") <= 1)
            val started = successResult(runtime, "capture", capture)
            assertEquals("start", started.getString("operation"))
            assertEquals(36, started.getString("capture_id").length)
            // R-NET-004: stop waits for the retained terminal result instead of leaving the
            // Task and its capture resources behind.
            val stopped = successResult(
                runtime,
                "capture",
                JSONObject()
                    .put("operation", "stop")
                    .put("capture_id", started.getString("capture_id")),
            )
            assertEquals("capture_result", stopped.getString("operation"))
            assertEquals(started.getString("capture_id"), stopped.getString("capture_id"))
            val task = requireNotNull(
                submit(runtime, taskControlRequest("get", started.getString("task_id")))
                    .optJSONObject("result"),
            )
            assertEquals("completed", task.getString("state"))
        } else {
            for ((action, input) in listOf("capture" to capture, "packet" to inject)) {
                val refused = submit(runtime, networkRequest(action, input))
                assertEquals(refused.toString(), "error", refused.getString("outcome"))
                assertEquals(
                    refused.toString(),
                    "CAPABILITY_UNAVAILABLE",
                    refused.getJSONObject("error").getString("code"),
                )
            }
        }
    }

    /** The device's own LAN address, so the gate is exercised against a real target. */
    private fun lanTarget(runtime: IDroidBridgeRuntime): String {
        val interfaces = runCatching {
            successResult(runtime, "inspect", inspectInput("interfaces", 200)).optJSONArray("interfaces")
        }.getOrNull()
        for (entry in entries(interfaces)) {
            for (address in entries(entry.optJSONArray("addresses"))) {
                val text = address.optString("address")
                if (isPrivateIpv4(text)) return text
            }
        }
        return FALLBACK_LAN
    }

    private fun isPrivateIpv4(text: String): Boolean {
        val octets = text.split('.').mapNotNull { it.toIntOrNull() }
        if (octets.size != 4 || octets.any { it !in 0..255 }) return false
        return octets[0] == 10 ||
            (octets[0] == 192 && octets[1] == 168) ||
            (octets[0] == 172 && octets[1] in 16..31)
    }

    private fun entries(array: JSONArray?): List<JSONObject> {
        if (array == null) return emptyList()
        return (0 until array.length()).mapNotNull { array.optJSONObject(it) }
    }

    private fun base64Of(bytes: Int): String =
        android.util.Base64.encodeToString(ByteArray(bytes), android.util.Base64.NO_WRAP)

    private fun successResult(
        runtime: IDroidBridgeRuntime,
        action: String,
        input: JSONObject,
    ): JSONObject {
        val response = submit(runtime, networkRequest(action, input))
        assertEquals(response.toString(), "success", response.getString("outcome"))
        return response.getJSONObject("result")
    }

    private fun inspectInput(scope: String, maxEntries: Int): JSONObject = JSONObject()
        .put("scope", scope)
        .put("max_entries", maxEntries)

    private fun networkRequest(action: String, input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject().put("tool", "network").put("action", action).put("input", input),
        )
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun taskControlRequest(action: String, taskId: String): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject()
                .put("tool", "task_control")
                .put("action", action)
                .put("input", JSONObject().put("task_id", taskId)),
        )
        .toString()
        .toByteArray(Charsets.UTF_8)

    // A freshly installed App runs the runtime before the fixture host and its grants are
    // admitted, and submissions during that window are refused. Every gate starts from the
    // same admitted state instead of racing the host transition.
    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments()
            .getString("i8NetworkFixture")
            ?: APK_FIXTURE
        require(fixture == APK_FIXTURE || fixture == MAGISK_FIXTURE) {
            "unknown I8 network fixture: $fixture"
        }
        if (InstrumentationRegistry.getArguments().getString("i8RequestShizuku") == "true") {
            assertTrue(runtime.requestShizukuAuthorization())
        }
        val deadline = SystemClock.elapsedRealtime() + ADMISSION_DEADLINE_MS
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            if (fixtureAdmitted(status, fixture)) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) {
                error("I8 network fixture $fixture was not admitted: $status")
            }
            SystemClock.sleep(100)
        }
    }

    private fun fixtureAdmitted(status: JSONObject, fixture: String): Boolean {
        val result = status.optJSONObject("result") ?: return false
        val runtime = result.optJSONObject("runtime") ?: return false
        if (runtime.optString("readiness") != "ready") return false
        val grants = result.optJSONObject("grants") ?: return false
        return if (fixture == MAGISK_FIXTURE) {
            runtime.optString("host") == "magisk_backend" &&
                grants.optJSONObject("magisk.root")?.optString("state") == "available"
        } else {
            // A freshly started App Runtime publishes shizuku.shell before its Shizuku session
            // settles; the supplement families are only comparable once that session has.
            val shizuku = grants.optJSONObject("shizuku.shell")?.optString("state")
            val shellGuard = grants.optJSONObject("execution.shell_guard")?.optString("state")
            runtime.optString("host") == "apk_runtime" &&
                grants.optJSONObject("execution.app_guard")?.optString("state") == "available" &&
                shizuku != "unknown" &&
                (shizuku != "available" || shellGuard != "unknown")
        }
    }

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
        const val APK_FIXTURE = "apk"
        const val MAGISK_FIXTURE = "magisk"
        const val LOOPBACK = "127.0.0.1"
        const val LOCAL_NAME = "localhost"
        const val FALLBACK_LAN = "192.168.1.1"
        const val SRC_MAC = "02:00:00:00:00:01"
        const val DST_MAC = "02:00:00:00:00:02"
        const val IPV4_SRC = "10.0.2.15"
        const val IPV4_DST = "10.0.2.3"
        const val PAYLOAD_BASE64 = "dGVzdA=="
        const val PAYLOAD_BYTES = 4L
        const val BUILT_LENGTH = 14L + 20L + 8L + PAYLOAD_BYTES
        const val MAX_PACKET_BYTES = 131_072
        const val MAX_CAPTURE_BYTES = 268_435_456L
        const val ADMISSION_DEADLINE_MS = 75_000L
        val FAMILIES = listOf("interfaces", "routes", "dns", "sockets")
        val SCOPES = FAMILIES + "all"
        val STATES = setOf("available", "unavailable", "unknown")
        val OUTCOMES = setOf(
            "success",
            "not_found",
            "refused",
            "timeout",
            "unreachable",
            "dns_error",
            "tls_error",
            "no_route",
        )
    }
}
