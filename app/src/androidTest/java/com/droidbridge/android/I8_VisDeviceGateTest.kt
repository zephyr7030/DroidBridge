package com.droidbridge.android

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.Activity
import android.app.UiAutomation
import android.graphics.Rect
import android.view.accessibility.AccessibilityNodeInfo
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.media.projection.MediaProjectionManager
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.util.Log
import androidx.activity.result.ActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.runtimehost.IDroidBridgeRuntime
import com.droidbridge.android.runtimehost.IRuntimeCallback
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * I8-VIS device evidence through the canonical public ingress. The fixture argument names the
 * source set the run must be admitted under: `accessibility` and `projection` have no privileged
 * provider, `shizuku` is the APK host with a UID2000 session, `magisk` is the Magisk host.
 */
@RunWith(AndroidJUnit4::class)
class I8_VisDeviceGateTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context
        get() = instrumentation.targetContext

    @Test
    fun I8_VIS_G01_G06_G08_G10_observeKeepsOneSourceAndPrivilegedXmlIssuesNoRefs() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        if (fixture == PROJECTION) return@withRuntime
        openSettings(runtime, fixture)

        val first =result(submit(runtime, visual("observe", observeInput(image = true, nodes = true))))
        val second = result(submit(runtime, visual("observe", observeInput(image = false, nodes = true))))
        val display = first.getJSONObject("display")
        assertTrue(display.toString(), display.getInt("width") > 0 && display.getInt("height") > 0)
        assertEquals(
            "one admitted display geometry",
            geometry(display),
            geometry(second.getJSONObject("display")),
        )
        assertNotEquals(first.getString("observation_id"), second.getString("observation_id"))

        assertFalse(first.toString(), first.has("image_unavailable_reason"))
        val imageRef = first.getString("image_ref")
        assertTrue(imageRef.startsWith("dbref:image:"))
        val format = first.getString("image_format")
        assertTrue(format, format == "heic" || format == "png")

        assertTrue(first.toString(), first.has("nodes"))
        val nodes = first.getJSONArray("nodes")
        assertTrue(first.toString(), nodes.length() > 0)
        val refs = (0 until nodes.length()).map { nodes.getJSONObject(it) }.filter { it.has("node_ref") }
        if (fixture == ACCESSIBILITY) {
            assertTrue("Accessibility observations carry actionable node refs", refs.isNotEmpty())
        } else {
            assertTrue("privileged XML never issues a node_ref: $refs", refs.isEmpty())
        }

        // The hierarchy part resolves on this real page - it used to fail as IO_ERROR while the
        // screenshot succeeded - and the observation states what its own nodes can address.
        assertFalse(first.toString(), first.has("nodes_unavailable_reason"))
        val interact = first.getJSONObject("interact")
        assertTrue(first.toString(), interact.getBoolean("coordinate"))
        assertEquals(300_000L, interact.getLong("ttl_ms"))
        if (fixture == ACCESSIBILITY) {
            assertFalse(interact.toString(), interact.has("node_unavailable_reason"))
        } else {
            // A privileged hierarchy carries no identity, so none of its nodes is addressable.
            assertEquals("NODE_REFS_UNAVAILABLE", interact.optString("node_unavailable_reason"))
            // Every node carries the four edges its owner reported, however the device clipped them.
            // The exact contract for a clipped node is pinned by
            // i8_vis_g08_empty_node_rect_is_reported_verbatim_and_never_voids_the_page in the host
            // suite; a live screen may legitimately carry no clipped node at all.
            val malformed = (0 until nodes.length()).map { nodes.getJSONObject(it) }
                .filterNot { node -> EDGES.all { node.getJSONObject("bounds").has(it) } }
            assertTrue("every node reports its four device edges: $malformed", malformed.isEmpty())
        }

        val view = result(submit(runtime, visual("view", JSONObject().put("image_ref", imageRef))))
        assertEquals(display.getInt("width"), view.getInt("width"))
        assertEquals(display.getInt("height"), view.getInt("height"))
        val viewFormat = view.getString("format")
        assertTrue(viewFormat, viewFormat == "heic" || viewFormat == "png")
        val clipped = (0 until nodes.length()).map { nodes.getJSONObject(it) }.count { emptyRect(it.getJSONObject("bounds")) }
        Log.i(TAG, "fixture=$fixture observe image=$format nodes=${nodes.length()} clipped=$clipped refs=${refs.size} view=$viewFormat")
        goHome(runtime, fixture)
    }

    @Test
    fun I8_VIS_G07_aCoordinateOutlivesItsSceneAndDiesWithItsDisplay() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        if (fixture == PROJECTION) return@withRuntime
        openSettings(runtime, fixture)
        val observation = result(submit(runtime, visual("observe", observeInput(image = false, nodes = true))))
        val display = observation.getJSONObject("display")
        assertTrue(observation.toString(), observation.has("nodes"))
        val scene = observation.getJSONArray("nodes").toString()
        val coordinate = JSONObject()
            .put("operation", "tap")
            .put("target", "coordinate")
            .put("observation_id", observation.getString("observation_id"))
            .put("x", display.getInt("width") / 2)
            .put("y", display.getInt("height") / 2)

        // DroidBridge's own pages state their own live facts, so a call repaints the page under the
        // caller. A coordinate names a place on the display, and a scene that changed in between is
        // not a reason to drop the gesture.
        replaceScene(runtime, fixture)
        val replaced = result(submit(runtime, visual("observe", observeInput(image = false, nodes = true))))
        assertNotEquals("the scene must actually change", scene, replaced.getJSONArray("nodes").toString())
        val delivered = result(submit(runtime, visual("interact", coordinate)))
        assertTrue(delivered.toString(), delivered.getBoolean("delivered"))

        // The display the caller observed is the coordinate's whole identity, so a display that is
        // no longer the observed one refuses it before any input. A size override is a display fact
        // this device applies to the running session; a requested user rotation is not, because a
        // portrait-locked screen never turns.
        val savedSize = wm(runtime, fixture, "size").trim()
        val savedOverride = Regex("Override size: (\\d+x\\d+)").find(savedSize)?.groupValues?.get(1)
        try {
            wm(runtime, fixture, "size ${display.getInt("width")}x${display.getInt("height") - 200}")
            SystemClock.sleep(SETTLE_MS)
            val now = result(submit(runtime, visual("observe", observeInput(image = false, nodes = false))))
                .getJSONObject("display")
            assertNotEquals("the display must actually change", geometry(display), geometry(now))
            val refused = submit(runtime, visual("interact", coordinate))
            assertEquals(refused.toString(), "STALE_REFERENCE", errorCode(refused))
        } finally {
            wm(runtime, fixture, if (savedOverride == null) "size reset" else "size $savedOverride")
        }
        goHome(runtime, fixture)
    }

    @Test
    fun I8_VIS_G07_aNodeTargetStillDiesWithItsScene() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        if (fixture != ACCESSIBILITY) return@withRuntime
        openSettings(runtime, fixture)
        val observation = result(submit(runtime, visual("observe", observeInput(image = false, nodes = true))))
        assertTrue(observation.toString(), observation.has("nodes"))
        val nodes = observation.getJSONArray("nodes")
        val node = (0 until nodes.length()).map { nodes.getJSONObject(it) }.first { it.has("node_ref") }
        val target = JSONObject()
            .put("operation", "tap")
            .put("target", "node")
            .put("node_ref", node.getString("node_ref"))

        // A node names a place in the observed scene, so another screen replacing that scene must
        // refuse the target before any input.
        shell("am start -W -a android.settings.DATE_SETTINGS")
        SystemClock.sleep(SETTLE_MS)
        val before = foregroundActivity()
        val refused = submit(runtime, visual("interact", target))
        assertEquals(refused.toString(), "STALE_REFERENCE", errorCode(refused))
        SystemClock.sleep(SETTLE_MS)
        assertEquals("a refused node target delivers no input", before, foregroundActivity())
        val key = submit(runtime, visual("interact", homeKey()))
        assertEquals(key.toString(), "CAPABILITY_UNAVAILABLE", errorCode(key))
        goHome(runtime, fixture)
    }

    @Test
    fun I8_VIS_G09_anImageOnlyObservationAddressesItsCoordinate() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        if (fixture == PROJECTION) return@withRuntime
        openSettings(runtime, fixture)
        val observation = result(submit(runtime, visual("observe", observeInput(image = false, nodes = false))))
        assertFalse(observation.toString(), observation.has("nodes"))
        val interact = observation.getJSONObject("interact")
        assertEquals("NODES_NOT_REQUESTED", interact.getString("node_unavailable_reason"))
        assertTrue(observation.toString(), interact.getBoolean("coordinate"))

        // What the observation announced is what its coordinate can address.
        val display = observation.getJSONObject("display")
        val delivered = result(
            submit(
                runtime,
                visual(
                    "interact",
                    JSONObject()
                        .put("operation", "tap")
                        .put("target", "coordinate")
                        .put("observation_id", observation.getString("observation_id"))
                        .put("x", display.getInt("width") / 2)
                        .put("y", display.getInt("height") / 2),
                ),
            ),
        )
        assertTrue(delivered.toString(), delivered.getBoolean("delivered"))
        goHome(runtime, fixture)
    }

    /**
     * Opens Settings through the admitted surface. Privileged fixtures never open this run's
     * UiAutomation connection, because a second UiAutomation client would make the provider's
     * own `uiautomator dump` fail.
     */
    private fun openSettings(runtime: IDroidBridgeRuntime, fixture: String) {
        if (fixture == ACCESSIBILITY) {
            shell("am start -W -a android.settings.SETTINGS")
        } else {
            launchPackage(runtime, SETTINGS)
            return
        }
        SystemClock.sleep(SETTLE_MS)
    }

    private fun launchPackage(runtime: IDroidBridgeRuntime, packageName: String) {
        result(
            submit(
                runtime,
                request(
                    JSONObject()
                        .put("tool", "android")
                        .put("action", "launch")
                        .put("input", JSONObject().put("operation", "package").put("package_name", packageName)),
                ),
            ),
        )
        SystemClock.sleep(SETTLE_MS)
    }

    private fun goHome(runtime: IDroidBridgeRuntime, fixture: String) {
        if (fixture == ACCESSIBILITY) {
            shell("input keyevent HOME")
        } else {
            result(submit(runtime, visual("interact", homeKey())))
        }
    }

    /**
     * Replaces the observed scene: the accessibility fixture has no key capability, so it takes
     * another page through the shell, while a privileged one launches DroidBridge's own page - the
     * page whose own facts repaint it on every call.
     */
    private fun replaceScene(runtime: IDroidBridgeRuntime, fixture: String) {
        if (fixture == ACCESSIBILITY) {
            shell("am start -W -a android.settings.DATE_SETTINGS")
            SystemClock.sleep(SETTLE_MS)
        } else {
            launchPackage(runtime, context.packageName)
        }
    }

    private fun homeKey() = JSONObject().put("operation", "key").put("key_code", KEYCODE_HOME)

    @Test
    fun I8_VIS_G02_oneConsentServesRepeatedCapturesUntilStop() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)
        if (fixture != PROJECTION) return@withRuntime
        val initial = result(submit(runtime, contextStatusRequest()))
        assertEquals("unavailable", grantState(initial, "visual.media_projection_session"))
        val refused = result(submit(runtime, visual("observe", observeInput(image = true, nodes = false))))
        assertTrue(refused.toString(), refused.has("image_unavailable_reason") && !refused.has("image_ref"))

        val consent = requestProjectionConsent()
        assertEquals(Activity.RESULT_OK, consent.resultCode)
        context.startService(
            runtimeServiceIntent()
                .setAction(ACTION_CONSENT)
                .putExtra(EXTRA_RESULT_CODE, consent.resultCode)
                .putExtra(EXTRA_RESULT_DATA, requireNotNull(consent.data)),
        )
        awaitGrant(runtime, "visual.media_projection_session", "available")
        shell("input keyevent HOME")
        SystemClock.sleep(SETTLE_MS)

        // One visible consent serves repeated background captures through the same session.
        val refs = (1..3).map {
            val capture = result(submit(runtime, visual("observe", observeInput(image = true, nodes = false))))
            assertFalse(capture.toString(), capture.has("image_unavailable_reason"))
            capture.getString("image_ref")
        }
        assertEquals(refs.toString(), refs.size, refs.toSet().size)
        assertEquals("available", grantState(result(submit(runtime, contextStatusRequest())), "visual.media_projection_session"))

        context.startService(runtimeServiceIntent().setAction(ACTION_STOP))
        awaitGrant(runtime, "visual.media_projection_session", "unavailable")
        val stopped = result(submit(runtime, visual("observe", observeInput(image = true, nodes = false))))
        assertTrue(stopped.toString(), stopped.has("image_unavailable_reason") && !stopped.has("image_ref"))
    }

    private fun requestProjectionConsent(): ActivityResult {
        val outcome = AtomicReference<ActivityResult?>(null)
        val done = CountDownLatch(1)
        val manager = context.getSystemService(MediaProjectionManager::class.java)
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            scenario.onActivity { activity ->
                activity.activityResultRegistry
                    .register("i8-vis-consent", ActivityResultContracts.StartActivityForResult()) { result ->
                        outcome.set(result)
                        done.countDown()
                    }
                    .launch(manager.createScreenCaptureIntent())
            }
            val deadline = SystemClock.elapsedRealtime() + CONSENT_DEADLINE_MS
            while (done.count > 0 && SystemClock.elapsedRealtime() < deadline) {
                acceptConsentDialog()
                done.await(1, TimeUnit.SECONDS)
            }
            assertTrue("screen capture consent was not granted", done.await(1, TimeUnit.SECONDS))
        }
        return requireNotNull(outcome.get())
    }

    /** Accepts the system consent dialog: an entire-screen choice first when offered, then start. */
    private fun acceptConsentDialog() {
        val nodes = windowNodes()
        val labels = nodes.mapNotNull(::label)
        Log.i(TAG, "consent dialog labels=$labels")
        val singleApp = nodes.firstOrNull { label(it) in SINGLE_APP_TEXTS }
        val entireScreen = nodes.firstOrNull { label(it) in ENTIRE_SCREEN_TEXTS }
        when {
            // The share-scope menu is open: choose the entire screen.
            singleApp != null && entireScreen != null -> click(entireScreen)
            // The scope selector still names one app: open it.
            singleApp != null -> click(singleApp)
            // The entire screen is selected: confirm.
            entireScreen != null || labels.any { it in START_TEXTS } ->
                nodes.firstOrNull { label(it) in START_TEXTS || label(it) == NEXT_TEXT }?.let(::click)
        }
    }

    private fun label(node: AccessibilityNodeInfo): String? =
        (node.text ?: node.contentDescription)?.toString()?.trim()?.takeIf(String::isNotEmpty)

    /** Every node of every interactive window, read through this run's UiAutomation connection. */
    private fun windowNodes(): List<AccessibilityNodeInfo> {
        val automation = uiAutomation()
        automation.serviceInfo = automation.serviceInfo.apply {
            flags = flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        }
        val roots = automation.windows.mapNotNull { it.root }
            .ifEmpty { listOfNotNull(automation.rootInActiveWindow) }
        val nodes = mutableListOf<AccessibilityNodeInfo>()
        val queue = ArrayDeque(roots)
        while (queue.isNotEmpty()) {
            val node = queue.removeFirst()
            nodes += node
            for (index in 0 until node.childCount) node.getChild(index)?.let(queue::addLast)
        }
        return nodes
    }

    private fun click(node: AccessibilityNodeInfo) {
        var clickable: AccessibilityNodeInfo? = node
        while (clickable != null && !clickable.isClickable) clickable = clickable.parent
        if (clickable?.performAction(AccessibilityNodeInfo.ACTION_CLICK) != true) {
            val bounds = Rect()
            node.getBoundsInScreen(bounds)
            shell("input tap ${bounds.centerX()} ${bounds.centerY()}")
        }
        SystemClock.sleep(1_000)
    }

    private fun uiAutomation(): UiAutomation =
        instrumentation.getUiAutomation(UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES)

    private fun foregroundActivity(): String =
        shell("dumpsys activity activities").lineSequence()
            .firstOrNull { it.contains("topResumedActivity") || it.contains("mResumedActivity") }
            ?.trim()
            .orEmpty()

    private fun geometry(display: JSONObject) =
        Triple(display.getInt("width"), display.getInt("height"), display.getInt("rotation"))

    /** Android reports a node with no visible area as an inverted rect rather than omitting it. */
    private fun emptyRect(bounds: JSONObject): Boolean =
        bounds.getInt("right") < bounds.getInt("left") || bounds.getInt("bottom") < bounds.getInt("top")

    private fun awaitGrant(runtime: IDroidBridgeRuntime, key: String, state: String) {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (true) {
            val status = result(submit(runtime, contextStatusRequest()))
            if (grantState(status, key) == state) return
            assertTrue("$key did not become $state: $status", SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(200)
        }
    }

    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments().getString("i8VisualFixture") ?: ACCESSIBILITY
        require(fixture in setOf(ACCESSIBILITY, PROJECTION, SHIZUKU, MAGISK)) { "unknown I8 visual fixture: $fixture" }
        if (fixture == ACCESSIBILITY) {
            // Starting the run force-stops the App, and Android does not rebind a stopped
            // package's accessibility service until the user's enabled-service setting is
            // written again, which is exactly what re-enabling it in Settings does.
            shell("settings delete secure enabled_accessibility_services")
            shell("settings put secure enabled_accessibility_services $ACCESSIBILITY_SERVICE")
            shell("settings put secure accessibility_enabled 1")
        }
        val deadline = SystemClock.elapsedRealtime() + 75_000
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            if (fixtureAdmitted(status, fixture)) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) error("I8 visual fixture $fixture was not admitted: $status")
            SystemClock.sleep(100)
        }
    }

    private fun fixtureAdmitted(status: JSONObject, fixture: String): Boolean {
        val result = status.optJSONObject("result") ?: return false
        val host = result.optJSONObject("runtime")?.optString("host")
        val shizuku = grantState(result, "shizuku.shell")
        val accessibility = grantState(result, "visual.accessibility")
        return when (fixture) {
            ACCESSIBILITY -> host == "apk_runtime" && shizuku == "unavailable" && accessibility == "available"
            PROJECTION -> host == "apk_runtime" && shizuku == "unavailable" && accessibility == "unavailable"
            SHIZUKU -> host == "apk_runtime" && shizuku == "available" &&
                grantState(result, "execution.shell_guard") == "available" && accessibility != "available"
            // Image and hierarchy transforms run through the framework companion, which
            // reconnects with back-off after the run force-stops the App.
            else -> host == "magisk_backend" && grantState(result, "magisk.root") == "available" &&
                grantState(result, "magisk.framework") == "available"
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

    private fun observeInput(image: Boolean, nodes: Boolean) = JSONObject()
        .put("include_image", image)
        .put("include_nodes", nodes)
        .put("max_nodes", 500)

    private fun visual(action: String, input: JSONObject): ByteArray = request(
        JSONObject().put("tool", "visual").put("action", action).put("input", input),
    )

    private fun contextStatusRequest(): ByteArray = request(
        JSONObject().put("tool", "context").put("action", "status").put("input", JSONObject().put("detail", "full")),
    )

    private fun request(payload: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put("payload", payload)
        .toString()
        .toByteArray(Charsets.UTF_8)

    private fun runtimeServiceIntent(): Intent =
        Intent().setComponent(ComponentName(context.packageName, RUNTIME_SERVICE))

    // The default UiAutomation connection suppresses other accessibility services, which would
    // disconnect the DroidBridge service this fixture depends on.
    private fun shell(command: String): String =
        ParcelFileDescriptor.AutoCloseInputStream(
            instrumentation.getUiAutomation(UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES)
                .executeShellCommand(command),
        ).use { it.readBytes().toString(Charsets.UTF_8) }

    /**
     * Runs one `wm` command for the display lever, with everything after `wm` as `arguments`. A
     * privileged fixture goes through the runtime rather than this process's shell, for the reason
     * [openSettings] states: opening this run's UiAutomation connection there would make the
     * provider's own `uiautomator dump` fail for the rest of the run.
     */
    private fun wm(runtime: IDroidBridgeRuntime, fixture: String, arguments: String): String {
        if (fixture == ACCESSIBILITY) return shell("wm $arguments")
        val response = submit(
            runtime,
            request(
                JSONObject()
                    .put("tool", "command")
                    .put("action", "run")
                    .put(
                        "input",
                        JSONObject().put("command", "wm $arguments")
                            .put("run_as", if (fixture == MAGISK) "root" else "shell"),
                    ),
            ),
        )
        assertEquals(response.toString(), "success", response.getString("outcome"))
        return response.getJSONObject("result").getString("stdout")
    }

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
        assertTrue(context.bindService(runtimeServiceIntent(), connection, Context.BIND_AUTO_CREATE))
        try {
            assertTrue(connected.await(10, TimeUnit.SECONDS))
            block(requireNotNull(runtime))
        } finally {
            context.unbindService(connection)
        }
    }

    private companion object {
        const val TAG = "I8VisGate"
        const val ACCESSIBILITY = "accessibility"
        const val PROJECTION = "projection"
        const val SHIZUKU = "shizuku"
        const val MAGISK = "magisk"
        const val SETTLE_MS = 1_500L
        const val CONSENT_DEADLINE_MS = 30_000L
        const val KEYCODE_HOME = 3
        const val SETTINGS = "com.android.settings"
        const val RUNTIME_SERVICE = "com.droidbridge.android.runtimehost.DroidBridgeService"
        const val ACCESSIBILITY_SERVICE =
            "com.droidbridge.android.debug/com.droidbridge.android.execution.android.DroidBridgeAccessibilityService"
        const val ACTION_CONSENT = "com.droidbridge.android.action.MEDIA_PROJECTION_CONSENT"
        const val ACTION_STOP = "com.droidbridge.android.action.MEDIA_PROJECTION_STOP"
        const val EXTRA_RESULT_CODE = "result_code"
        const val EXTRA_RESULT_DATA = "result_data"
        val START_TEXTS = setOf("立即开始", "开始", "共享屏幕", "开始共享", "Start now", "Start", "Share screen")
        const val NEXT_TEXT = "下一步"
        val EDGES = listOf("left", "top", "right", "bottom")
        val SINGLE_APP_TEXTS = setOf("共享一个应用", "Share one app", "A single app")
        val ENTIRE_SCREEN_TEXTS = setOf(
            "整个屏幕",
            "共享整个屏幕",
            "录制整个屏幕",
            "Entire screen",
            "Share entire screen",
        )
    }
}
