package com.droidbridge.android

import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.mcp.McpListenerState
import com.droidbridge.android.product.mcp.McpSettingsReplies
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URI
import java.util.UUID
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The loopback MCP facade as ChatGPT reaches it: every tool call is answered with a tool result
 * that carries its own outcome, never with an opaque JSON-RPC internal error.
 */
@RunWith(AndroidJUnit4::class)
class I14McpFacadeGateTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun everyToolCallIsAnsweredWithItsOwnOutcome() = runBlocking {
        withListener { endpoint, token ->
            val status = structured(
                call(endpoint, token, 1, "context", "status", JSONObject().put("detail", "full")),
                "context.status",
            )
            val coordinateInput = status.getJSONObject("capabilities").getJSONObject("visual.coordinate_input")
            println("visual facts: ${status.getJSONObject("capabilities")}")

            val observed = structured(
                call(
                    endpoint, token, 2, "visual", "observe",
                    JSONObject().put("include_image", false),
                ),
                "visual.observe",
            )
            println("visual.observe: $observed")

            // The exact coordinate tap that answered -32603: it answers the reason it cannot run,
            // and while the accessibility grant is that reason, the code says so.
            val tap = call(
                endpoint, token, 3, "visual", "interact",
                JSONObject()
                    .put("operation", "tap")
                    .put("target", "coordinate")
                    .put("observation_id", observed.getString("observation_id"))
                    .put("x", 540)
                    .put("y", 960),
            )
            println("visual.interact: $tap")
            val answered = structured(tap, "visual.interact")
            if (coordinateInput.getString("state") == "available") {
                assertFalse("tap refused: $answered", tap.getJSONObject("result").getBoolean("isError"))
            } else {
                assertTrue("tap served: $answered", tap.getJSONObject("result").getBoolean("isError"))
                assertEquals(answered.toString(), "CAPABILITY_UNAVAILABLE", answered.getString("code"))
            }

            // A name the VPN resolves by itself: whether it completes, times out or cannot run, the
            // answer states which one it was.
            structured(
                call(
                    endpoint, token, 4, "network", "diagnose",
                    JSONObject().put("test", "tls").put("host", "example.com").put("port", 443),
                ),
                "network.diagnose",
            )
        }
    }

    @Test
    fun aCaptureSettlesByItsBoundAndCarriesItsInterfaceTraffic() = runBlocking {
        withListener { endpoint, token ->
            val interfaces = structured(
                call(endpoint, token, 11, "network", "inspect", JSONObject().put("scope", "interfaces")),
                "network.inspect",
            )
            val entries = interfaces.getJSONArray("interfaces")
            val names = (0 until entries.length()).map { entries.getJSONObject(it).getString("name") }
            assumeTrue(TUN_INTERFACE in names)

            // The reported capture, as it was reported: bounded, on the VPN's own interface, and
            // it must settle by itself instead of holding the Task past every bound it states.
            deleteIfPresent(endpoint, token, 10, CAPTURE_PATH)
            val started = served(
                call(
                    endpoint, token, 12, "network", "capture",
                    JSONObject()
                        .put("operation", "start")
                        .put("interface", TUN_INTERFACE)
                        .put("max_packets", 20)
                        .put("max_bytes", 1048576)
                        .put("max_duration_ms", 1500)
                        .put("persist_to", path(CAPTURE_PATH)),
                ),
                "network.capture",
            )
            assertTrue("no task: $started", started.has("task_id"))
            val record = try {
                awaitTask(endpoint, token, started.getString("task_id"))
            } catch (failure: AssertionError) {
                // A capture this gate could not settle is stopped before the gate gives up on it.
                throw AssertionError(
                    "${failure.message}; stop: ${stopCapture(endpoint, token, started.getString("capture_id"))}",
                    failure,
                )
            }
            println("tun0 capture: $record")
            assertEquals(record.toString(), "completed", record.getString("state"))

            // A settled capture publishes the stream it holds and commits its destination whether
            // or not its device reported a packet, so the file this gate asked for exists with the
            // device's own link type and reads back exactly the records the settlement reported.
            val settled = record.getJSONObject("result")
            assertTrue("settled without a capture_ref: $settled", settled.has("capture_ref"))
            assertEquals(
                "settled without the destination it requested: $settled",
                CAPTURE_PATH,
                settled.getJSONObject("destination").getString("value"),
            )
            val bytes = readBytes(endpoint, token, 13, CAPTURE_PATH)
            assertTrue(bytes.size >= PCAP_FILE_HEADER_BYTES)
            assertEquals(PCAP_LINKTYPE_RAW, littleEndian(bytes, 20))
            val packets = captureRecords(endpoint, token, 14, CAPTURE_PATH)
            assertEquals(
                "the file holds what the settlement reported: $settled",
                settled.getLong("packets_captured"),
                packets.length().toLong(),
            )
            // A raw-IP device's records decode as bare IP packets, never as Ethernet frames.
            assertTrue(
                "records of a raw-IP device: $packets",
                (0 until packets.length())
                    .map { packets.getJSONObject(it).getString("protocol") }
                    .all { it in RAW_IP_PROTOCOLS },
            )

            // The same capture on the interface this gate's own tool calls travel: the listener's
            // loopback traffic is on it by construction, so a capture that reads nothing says it
            // missed what its own device carried.
            deleteIfPresent(endpoint, token, 15, LOOPBACK_PATH)
            val carried = served(
                call(
                    endpoint, token, 16, "network", "capture",
                    JSONObject()
                        .put("operation", "start")
                        .put("interface", LOOPBACK_INTERFACE)
                        .put("max_packets", 20)
                        .put("max_bytes", 1048576)
                        .put("max_duration_ms", 3000)
                        .put("persist_to", path(LOOPBACK_PATH)),
                ),
                "network.capture",
            )
            val carriedTask = carried.getString("task_id")
            var calls = 0
            while (calls < TRAFFIC_CALL_LIMIT && taskState(endpoint, token, carriedTask) !in TERMINAL_TASK_STATES) {
                served(call(endpoint, token, 200 + calls, "context", "status", JSONObject()), "context.status")
                calls += 1
            }
            val carriedRecord = awaitTask(endpoint, token, carriedTask)
            println("loopback capture after $calls calls: $carriedRecord")
            assertEquals(carriedRecord.toString(), "completed", carriedRecord.getString("state"))
            val held = carriedRecord.getJSONObject("result")
            assertTrue("no records after $calls calls: $held", held.getLong("packets_captured") > 0)
            assertEquals(LOOPBACK_PATH, held.getJSONObject("destination").getString("value"))
            val carriedBytes = readBytes(endpoint, token, 17, LOOPBACK_PATH)
            assertEquals(PCAP_LINKTYPE_ETHERNET, littleEndian(carriedBytes, 20))
            val records = captureRecords(endpoint, token, 18, LOOPBACK_PATH)
            assertTrue("no records read: $held", records.length() > 0)
            val listenerPort = URI(endpoint).port
            assertTrue(
                "no record of this gate's own traffic on port $listenerPort: $records",
                (0 until records.length()).any { index ->
                    val packet = records.getJSONObject(index)
                    packet.optInt("src_port") == listenerPort || packet.optInt("dst_port") == listenerPort
                },
            )
            delete(endpoint, token, 19, LOOPBACK_PATH)
            // The file this gate created is the gate's to remove, and the read of what it removed
            // is the negative control for the reads above.
            delete(endpoint, token, 20, CAPTURE_PATH)
            assertEquals("NOT_FOUND", refusal(endpoint, token, 21, CAPTURE_PATH))
        }
    }

    @Test
    fun aRunningCaptureIsStoppedAndAnswersItsOwnResult() = runBlocking {
        withListener { endpoint, token ->
            val started = served(
                call(
                    endpoint, token, 40, "network", "capture",
                    JSONObject()
                        .put("operation", "start")
                        .put("interface", LOOPBACK_INTERFACE)
                        .put("max_packets", 1000000)
                        .put("max_bytes", 268435456)
                        .put("max_duration_ms", 30000),
                ),
                "network.capture",
            )
            val captureId = started.getString("capture_id")
            val taskId = started.getString("task_id")
            // A capture can be stopped once its device is open, so the gate waits for the Task to
            // run instead of racing its stop against the start it just made.
            val deadline = System.currentTimeMillis() + TASK_TIMEOUT_MS
            while (taskState(endpoint, token, taskId) == "pending") {
                if (System.currentTimeMillis() >= deadline) {
                    throw AssertionError("the capture never ran: ${taskRecord(endpoint, token, taskId)}")
                }
                delay(50)
            }
            delay(CAPTURE_STOP_DELAY_MS)

            val stopped = served(
                call(
                    endpoint, token, 41, "network", "capture",
                    JSONObject().put("operation", "stop").put("capture_id", captureId),
                ),
                "network.capture",
            )
            assertEquals("capture_result", stopped.getString("operation"))
            assertEquals(captureId, stopped.getString("capture_id"))
            val record = awaitTask(endpoint, token, taskId)
            println("stopped capture: $record")
            assertEquals(record.toString(), "completed", record.getString("state"))
        }
    }

    @Test
    fun sharedStorageReplaceAndEditPublishTheNewContent() = runBlocking {
        withListener { endpoint, token ->
            // A run that failed before its cleanup leaves its file behind; a test owns its target.
            deleteIfPresent(endpoint, token, 20, SHARED_PATH)

            val created = served(
                write(endpoint, token, 21, "create", "alpha"),
                "filesystem.write",
            )
            assertEquals(5L, created.getLong("bytes_written"))
            assertEquals("alpha", readText(endpoint, token, 22, SHARED_PATH))

            served(write(endpoint, token, 23, "replace", "bravo"), "filesystem.write")
            assertEquals("bravo", readText(endpoint, token, 24, SHARED_PATH))

            served(
                call(
                    endpoint, token, 25, "filesystem", "write",
                    JSONObject()
                        .put("mode", "edit")
                        .put("target", path(SHARED_PATH))
                        .put(
                            "replacements",
                            JSONArray().put(JSONObject().put("old", "bravo").put("new", "charlie")),
                        ),
                ),
                "filesystem.write",
            )
            assertEquals("charlie", readText(endpoint, token, 26, SHARED_PATH))

            delete(endpoint, token, 27, SHARED_PATH)
        }
    }

    @Test
    fun anArchiveIsExtractedOntoSharedStorageAndServesWhatItHeld() = runBlocking {
        withListener { endpoint, token ->
            // The reported failure: `filesystem` `archive` `extract` of a ZIP the Runtime itself
            // made, into a destination on shared storage — a different filesystem than the Task's
            // own temporary directory, where an extraction is staged.
            deleteTree(endpoint, token, 50, EXTRACT_DESTINATION)
            deleteIfPresent(endpoint, token, 51, EXTRACT_ARCHIVE)
            deleteTree(endpoint, token, 52, EXTRACT_SOURCE)
            served(
                call(
                    endpoint, token, 53, "filesystem", "manage",
                    JSONObject()
                        .put("operation", "mkdir")
                        .put("parents", true)
                        .put("target", path(EXTRACT_SOURCE)),
                ),
                "filesystem.manage",
            )
            served(
                call(
                    endpoint, token, 54, "filesystem", "write",
                    JSONObject()
                        .put("mode", "create")
                        .put("content", EXTRACT_CONTENT)
                        .put("target", path(EXTRACT_PAYLOAD)),
                ),
                "filesystem.write",
            )
            val archived = awaitTask(
                endpoint, token,
                served(
                    call(
                        endpoint, token, 55, "filesystem", "archive",
                        JSONObject()
                            .put("operation", "create")
                            .put("format", "zip")
                            .put("sources", JSONArray().put(path(EXTRACT_SOURCE)))
                            .put("destination", path(EXTRACT_ARCHIVE)),
                    ),
                    "filesystem.archive",
                ).getString("task_id"),
            )
            assertEquals(archived.toString(), "completed", archived.getString("state"))
            assertEquals(2L, archived.getJSONObject("result").getLong("entries_archived"))

            val extracted = awaitTask(
                endpoint, token,
                served(
                    call(
                        endpoint, token, 56, "filesystem", "archive",
                        JSONObject()
                            .put("operation", "extract")
                            .put("target", path(EXTRACT_ARCHIVE))
                            .put("destination", path(EXTRACT_DESTINATION)),
                    ),
                    "filesystem.archive",
                ).getString("task_id"),
            )
            println("shared-storage extract: $extracted")
            assertEquals(extracted.toString(), "completed", extracted.getString("state"))
            val result = extracted.getJSONObject("result")
            assertEquals("extract", result.getString("operation"))
            assertEquals(2L, result.getLong("entries_extracted"))
            assertEquals(
                EXTRACT_DESTINATION,
                result.getJSONObject("destination").getString("value"),
            )
            val inspected = served(
                call(
                    endpoint, token, 57, "filesystem", "inspect",
                    JSONObject().put("target", path(EXTRACT_DESTINATION)),
                ),
                "filesystem.inspect",
            )
            assertEquals("directory", inspected.getString("type"))
            assertEquals(
                EXTRACT_CONTENT,
                readText(
                    endpoint, token, 58,
                    "$EXTRACT_DESTINATION/$EXTRACT_DIRECTORY/$EXTRACT_FILE",
                ),
            )

            deleteTree(endpoint, token, 59, EXTRACT_DESTINATION)
            delete(endpoint, token, 60, EXTRACT_ARCHIVE)
            deleteTree(endpoint, token, 61, EXTRACT_SOURCE)
        }
    }

    @Test
    fun aFailedTlsHandshakeOnTheModuleBackendAnswersItsOutcome() = runBlocking {
        withListener { endpoint, token ->
            // The reported shape: TCP to the peer succeeds and the TLS handshake does not. The
            // listener this endpoint serves talks cleartext HTTP, so a TLS client reaches it
            // without the handshake completing. The answer is the probe's own outcome — a
            // structured result naming what the peer did — and never an internal error a client
            // can only read as `-32603`.
            val port = URI(endpoint).port
            val diagnosed = served(
                call(
                    endpoint, token, 300, "network", "diagnose",
                    JSONObject()
                        .put("test", "tls")
                        .put("host", "127.0.0.1")
                        .put("port", port)
                        .put("timeout_ms", 3_000),
                ),
                "network.diagnose",
            )
            println("tls against a cleartext peer: $diagnosed")
            assertEquals("tls", diagnosed.getString("test"))
            assertEquals("127.0.0.1", diagnosed.getString("server_name"))
            assertEquals(port, diagnosed.getInt("port"))
            assertFalse(diagnosed.getBoolean("certificate_verified"))
            assertTrue(
                diagnosed.toString(),
                TLS_FAILURE_OUTCOMES.contains(diagnosed.getString("outcome")),
            )
        }
    }

    @Test
    fun anIdenticalResendIsAnsweredWithTheRetainedResult() = runBlocking {
        withListener { endpoint, token ->
            // A create is exclusive, so a second execution of it answers ALREADY_EXISTS: success on
            // the resend is the Runtime returning the result it retained for that same request.
            deleteIfPresent(endpoint, token, 30, SHARED_PATH)
            val requestId = UUID.randomUUID().toString()
            served(write(endpoint, token, 31, "create", "alpha", requestId), "filesystem.write")
            served(write(endpoint, token, 32, "create", "alpha", requestId), "filesystem.write")
            assertEquals("alpha", readText(endpoint, token, 33, SHARED_PATH))
            delete(endpoint, token, 34, SHARED_PATH)
        }
    }

    /**
     * Text the platform `input text` cannot type (CJK, full-width, emoji, supplementary planes)
     * reaches the focused editor unchanged on the module backend, the clipboard it borrowed is put
     * back, and Ctrl+A is a real key combination rather than an unsupported meta state.
     */
    @Test
    fun unicodeTextAndKeyCombinationsReachTheFocusedEditor() = runBlocking {
        withListener { endpoint, token ->
            val instrumentation = InstrumentationRegistry.getInstrumentation()
            fun shell(command: String): String =
                android.os.ParcelFileDescriptor.AutoCloseInputStream(
                    instrumentation.uiAutomation.executeShellCommand(command),
                ).use { it.readBytes().decodeToString() }
            // The instrumentation already owns the UiAutomation connection, so the editor is read
            // through it rather than through a second `uiautomator dump` client.
            fun focusedText(): String {
                val root = instrumentation.uiAutomation.rootInActiveWindow
                    ?: throw AssertionError("no active window")
                val focused = root.findFocus(android.view.accessibility.AccessibilityNodeInfo.FOCUS_INPUT)
                    ?: throw AssertionError("no focused editor")
                return focused.text?.toString().orEmpty()
            }
            val previous = "droidbridge-g14-previous-clip"
            served(
                call(endpoint, token, 30, "android", "clipboard", JSONObject().put("operation", "write").put("text", previous)),
                "android.clipboard",
            )
            shell("am start -W -a android.settings.APP_SEARCH_SETTINGS")
            delay(2_000)
            try {
                val unicode = "卓爱桥中文｜繁體｜，。！？「」｜ＡＢＣ１２３｜🙂🚀𠮷"
                served(
                    call(endpoint, token, 31, "visual", "interact", JSONObject().put("operation", "text").put("text", unicode)),
                    "visual.interact",
                )
                delay(500)
                assertEquals(unicode, focusedText())
                val clip = served(
                    call(endpoint, token, 32, "android", "clipboard", JSONObject().put("operation", "read")),
                    "android.clipboard",
                )
                assertEquals(clip.toString(), previous, clip.getString("text"))

                // Ctrl+A selects everything, so the ASCII text typed next replaces it.
                served(
                    call(
                        endpoint, token, 33, "visual", "interact",
                        JSONObject().put("operation", "key").put("key_code", KEYCODE_A).put("meta_state", META_CTRL_ON),
                    ),
                    "visual.interact",
                )
                served(
                    call(endpoint, token, 34, "visual", "interact", JSONObject().put("operation", "text").put("text", "replaced")),
                    "visual.interact",
                )
                delay(500)
                assertEquals("replaced", focusedText())
            } finally {
                shell("input keyevent KEYCODE_BACK")
                shell("input keyevent KEYCODE_BACK")
                call(endpoint, token, 35, "android", "clipboard", JSONObject().put("operation", "clear"))
            }
        }
    }

    private suspend fun withListener(block: suspend (String, String) -> Unit) {
        val client = DroidBridgeClient(context)
        var enabled = false
        try {
            client.bind()
            withTimeout(RUNTIME_AVAILABLE_TIMEOUT_MS) {
                client.state.filterIsInstance<ClientState.Available>().first()
            }
            val settings = McpSettingsReplies.settings(client.setMcpEnabled(true))
            assertEquals(McpListenerState.Running, settings?.listener)
            enabled = true
            val token = McpSettingsReplies.token(client.revealMcpToken())
            assertNotNull(token)
            val endpoint = requireNotNull(settings).endpoint
            awaitMagiskBackend(client, endpoint, requireNotNull(token))
            block(endpoint, requireNotNull(token))
        } finally {
            if (enabled) client.setMcpEnabled(false)
            client.unbind()
        }
    }

    /**
     * Every gate runs where the product runs: while the module's backend serves. An app installed
     * without its canonical store leaves the supervisor's daemon unable to start and backing off,
     * so the host is waited for through the product's own recheck rather than assumed.
     */
    private suspend fun awaitMagiskBackend(client: DroidBridgeClient, endpoint: String, token: String) {
        val deadline = System.currentTimeMillis() + HOST_TIMEOUT_MS
        var last = ""
        while (true) {
            val answered = call(endpoint, token, 5, "context", "status", JSONObject())
            last = answered.toString()
            val result = answered.getJSONObject("result")
            if (!result.getBoolean("isError")) {
                val status = structured(answered, "context.status")
                if (status.getJSONObject("runtime").getString("host") == MAGISK_BACKEND) return
            }
            if (System.currentTimeMillis() >= deadline) {
                throw AssertionError("the module backend never served: $last")
            }
            client.recheck()
            delay(HOST_POLL_INTERVAL_MS)
        }
    }

    private fun write(
        endpoint: String,
        token: String,
        id: Int,
        mode: String,
        content: String,
        requestId: String? = null,
    ): JSONObject = call(
        endpoint, token, id, "filesystem", "write",
        JSONObject().put("mode", mode).put("target", path(SHARED_PATH)).put("content", content),
        requestId,
    )

    private fun delete(endpoint: String, token: String, id: Int, value: String) {
        served(
            call(
                endpoint, token, id, "filesystem", "manage",
                JSONObject().put("operation", "delete").put("target", path(value)),
            ),
            "filesystem.manage",
        )
    }

    /** Removes a directory a previous run may have left, tolerating its absence. */
    private fun deleteTree(endpoint: String, token: String, id: Int, value: String) {
        val answered = call(
            endpoint, token, id, "filesystem", "manage",
            JSONObject()
                .put("operation", "delete")
                .put("recursive", true)
                .put("target", path(value)),
        )
        if (answered.getJSONObject("result").getBoolean("isError")) {
            assertEquals(answered.toString(), "NOT_FOUND", structured(answered, "filesystem.manage").getString("code"))
        }
    }

    /** Removes a target a previous run may have left, tolerating its absence. */
    private fun deleteIfPresent(endpoint: String, token: String, id: Int, value: String) {
        val answered = call(
            endpoint, token, id, "filesystem", "manage",
            JSONObject().put("operation", "delete").put("target", path(value)),
        )
        if (answered.getJSONObject("result").getBoolean("isError")) {
            assertEquals(answered.toString(), "NOT_FOUND", structured(answered, "filesystem.manage").getString("code"))
        }
    }

    private fun readText(endpoint: String, token: String, id: Int, value: String): String = served(
        call(
            endpoint, token, id, "filesystem", "read",
            JSONObject().put("target", path(value)).put("encoding", "utf8"),
        ),
        "filesystem.read",
    ).getString("data")

    private fun readBytes(endpoint: String, token: String, id: Int, value: String): ByteArray = Base64.decode(
        served(
            call(
                endpoint, token, id, "filesystem", "read",
                JSONObject().put("target", path(value)).put("encoding", "base64"),
            ),
            "filesystem.read",
        ).getString("data"),
        Base64.DEFAULT,
    )

    private fun captureRecords(endpoint: String, token: String, id: Int, value: String): JSONArray = served(
        call(
            endpoint, token, id, "network", "capture",
            JSONObject().put("operation", "read").put("file", path(value)),
        ),
        "network.capture",
    ).getJSONArray("packets")

    /** The code of a refusal the caller expected, or the answer that was served instead. */
    private fun refusal(endpoint: String, token: String, id: Int, value: String): String {
        val answered = call(
            endpoint, token, id, "filesystem", "read",
            JSONObject().put("target", path(value)).put("encoding", "base64"),
        )
        if (!answered.getJSONObject("result").getBoolean("isError")) return answered.toString()
        return structured(answered, "filesystem.read").getString("code")
    }

    private suspend fun awaitTask(endpoint: String, token: String, taskId: String): JSONObject {
        var record = taskRecord(endpoint, token, taskId)
        val deadline = System.currentTimeMillis() + TASK_TIMEOUT_MS
        while (record.getString("state") !in TERMINAL_TASK_STATES) {
            if (System.currentTimeMillis() >= deadline) {
                throw AssertionError("the task never settled: $record")
            }
            delay(250)
            record = taskRecord(endpoint, token, taskId)
        }
        return record
    }

    /** The capture owner's stop, whose own answer is reported by the caller that needed it. */
    private fun stopCapture(endpoint: String, token: String, captureId: String): String = call(
        endpoint, token, 16, "network", "capture",
        JSONObject().put("operation", "stop").put("capture_id", captureId),
    ).getJSONObject("result").getJSONObject("structuredContent").toString()

    private fun taskRecord(endpoint: String, token: String, taskId: String): JSONObject = structured(
        call(
            endpoint, token, 100, "task_control", "get",
            JSONObject().put("task_id", taskId),
        ),
        "task_control.get",
    )

    private fun taskState(endpoint: String, token: String, taskId: String): String =
        taskRecord(endpoint, token, taskId).getString("state")

    /**
     * One tool answer that must have been served: a refusal the test did not expect cannot pass as
     * the result it did expect.
     */
    private fun served(response: JSONObject, operation: String): JSONObject {
        assertFalse("refused: $response", response.getJSONObject("result").getBoolean("isError"))
        return structured(response, operation)
    }

    /**
     * One tool answer: a failure names the operation it belongs to and a code the Runtime's catalog
     * has, and a failure this process answered itself also says what went wrong.
     */
    private fun structured(response: JSONObject, operation: String): JSONObject {
        assertFalse("JSON-RPC error: $response", response.has("error"))
        val result = response.getJSONObject("result")
        assertEquals("complete", result.getString("resultType"))
        val structured = result.getJSONObject("structuredContent")
        if (result.getBoolean("isError")) {
            val code = structured.getString("code")
            assertTrue("code $code", ERROR_CODES.contains(code))
            // Every message carries the whole answer, so a refusal nobody expected still says what
            // it was instead of only which operation it named.
            assertEquals(structured.toString(), operation, structured.getString("operation"))
            assertFalse(structured.toString(), structured.getBoolean("retryable"))
            if (code == "INTERNAL_ERROR") {
                assertTrue("no reason: $structured", structured.getString("message").isNotEmpty())
            }
        }
        return structured
    }

    /**
     * One request over a plain socket, framed the way the listener frames HTTP/1.1: the platform's
     * HTTP stacks refuse cleartext to a package whose target SDK forbids it, and this package is a
     * remote client, not the product.
     */
    private fun call(
        endpoint: String,
        token: String,
        id: Int,
        tool: String,
        action: String,
        input: JSONObject,
        requestId: String? = null,
    ): JSONObject {
        val target = URI(endpoint)
        val meta = JSONObject()
            .put("io.modelcontextprotocol/protocolVersion", PROTOCOL_VERSION)
            .put("io.modelcontextprotocol/clientCapabilities", JSONObject())
        requestId?.let { meta.put("io.droidbridge/requestId", it) }
        val body = JSONObject()
            .put("jsonrpc", "2.0")
            .put("id", id)
            .put("method", "tools/call")
            .put(
                "params",
                JSONObject()
                    .put("name", tool)
                    .put(
                        "arguments",
                        JSONObject().put("action", action).put("input", input),
                    )
                    .put("_meta", meta),
            )
            .toString()
            .toByteArray(Charsets.UTF_8)
        val head = buildString {
            append("POST ${target.rawPath} HTTP/1.1\r\n")
            append("Host: ${target.host}:${target.port}\r\n")
            append("Authorization: Bearer $token\r\n")
            append("Content-Type: application/json\r\n")
            append("Accept: application/json, text/event-stream\r\n")
            append("MCP-Protocol-Version: $PROTOCOL_VERSION\r\n")
            append("Mcp-Method: tools/call\r\n")
            append("Mcp-Name: $tool\r\n")
            append("Content-Length: ${body.size}\r\n")
            append("Connection: close\r\n\r\n")
        }.toByteArray(Charsets.UTF_8)

        val answered = Socket().use { socket ->
            socket.connect(InetSocketAddress(target.host, target.port), CONNECT_TIMEOUT_MS.toInt())
            socket.soTimeout = CALL_TIMEOUT_MS.toInt()
            socket.getOutputStream().apply {
                write(head)
                write(body)
                flush()
            }
            socket.getInputStream().readBytes().decodeToString()
        }
        val (status, text) = answered.splitOnce()
        assertTrue("HTTP $status: $text", status.startsWith("HTTP/1.1 200 "))
        return JSONObject(text)
    }

    private fun String.splitOnce(): Pair<String, String> {
        val boundary = indexOf("\r\n\r\n")
        assertTrue("no header terminator: $this", boundary >= 0)
        return substring(0, boundary).substringBefore("\r\n") to substring(boundary + 4)
    }

    private companion object {
        const val RUNTIME_AVAILABLE_TIMEOUT_MS = 60_000L
        const val HOST_TIMEOUT_MS = 180_000L
        const val HOST_POLL_INTERVAL_MS = 2_000L
        const val CONNECT_TIMEOUT_MS = 15_000L
        const val MAGISK_BACKEND = "magisk_backend"
        const val KEYCODE_A = 29
        const val META_CTRL_ON = 0x1000
        const val TASK_TIMEOUT_MS = 30_000L
        const val CALL_TIMEOUT_MS = 300_000L
        const val PROTOCOL_VERSION = "2026-07-28"
        const val TUN_INTERFACE = "tun0"
        const val LOOPBACK_INTERFACE = "lo"
        /** Bounds on keeping the listener busy while the loopback capture runs. */
        const val TRAFFIC_CALL_LIMIT = 400
        const val CAPTURE_STOP_DELAY_MS = 250L
        const val CAPTURE_PATH = "/data/local/tmp/droidbridge-g14-tun0.pcap"
        const val LOOPBACK_PATH = "/data/local/tmp/droidbridge-g14-lo.pcap"
        const val SHARED_PATH = "/sdcard/Download/droidbridge-g14-write.txt"
        const val EXTRACT_DIRECTORY = "droidbridge-g14-extract-src"
        const val EXTRACT_SOURCE = "/sdcard/Download/$EXTRACT_DIRECTORY"
        const val EXTRACT_PAYLOAD = "$EXTRACT_SOURCE/payload.txt"
        const val EXTRACT_ARCHIVE = "/sdcard/Download/droidbridge-g14-extract.zip"
        const val EXTRACT_DESTINATION = "/sdcard/Download/droidbridge-g14-extracted"
        const val EXTRACT_FILE = "payload.txt"
        const val EXTRACT_CONTENT = "extracted through shared storage"
        const val PCAP_FILE_HEADER_BYTES = 24
        const val PCAP_LINKTYPE_ETHERNET = 1
        const val PCAP_LINKTYPE_RAW = 101
        val TERMINAL_TASK_STATES = setOf("completed", "failed", "cancelled", "interrupted")
        /** A bare IP record decodes to an IP family, never to an Ethernet frame. */
        val RAW_IP_PROTOCOLS = setOf("ipv4", "ipv6", "tcp", "udp", "icmp", "other")
        /**
         * The peer answered something that is not a TLS record, or accepted the connection and
         * never answered at all. Either way the handshake is what failed, which is an outcome of a
         * performed probe rather than a Runtime error.
         */
        val TLS_FAILURE_OUTCOMES = setOf("tls_error", "timeout")
        val ERROR_CODES = setOf(
            "INVALID_ARGUMENT", "NOT_FOUND", "ALREADY_EXISTS", "PERMISSION_DENIED",
            "CAPABILITY_UNAVAILABLE", "UNSUPPORTED", "STALE_AUTHORITY", "STALE_REFERENCE",
            "REVISION_CONFLICT", "TIMEOUT", "CANCELLED", "IO_ERROR", "PROTOCOL_INCOMPATIBLE",
            "RESOURCE_LIMIT", "INTERNAL_ERROR", "NOT_EMPTY", "ARCHIVE_CORRUPT", "ARCHIVE_ENCRYPTED",
            "RUN_AS_UNAVAILABLE", "EXECUTION_FAILED", "CANCEL_FAILED", "CAPTURE_FAILED",
            "HOST_TRANSITION_PENDING",
        )

        fun path(value: String): JSONObject = JSONObject().put("type", "path").put("value", value)

        fun littleEndian(bytes: ByteArray, offset: Int): Int =
            (0..3).fold(0) { value, index ->
                value or ((bytes[offset + index].toInt() and 0xff) shl (8 * index))
            }
    }
}
