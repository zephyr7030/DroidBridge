package com.droidbridge.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import android.os.Process
import android.os.SystemClock
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
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class I8CmdDeviceGateTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    /**
     * S-AUTH-CMD-001 gives both host surfaces one public result and error contract. The
     * `app` and `shell` identities are served by two different providers on the same host,
     * so their settled shape has to be identical down to the field set, while the
     * identity facts and the execution class stay exactly the ones S-AUTH-CMD-001 assigns.
     */
    @Test
    fun I8_CMD_G01_bothHostSurfacesSettleTheSamePublicCommandResultAndError() = withRuntime { runtime ->
        awaitAdmittedFixture(runtime)

        val app = successResult(runtime, "app", OK_COMMAND)
        val shell = successResult(runtime, "shell", OK_COMMAND)
        assertEquals(APP_RESULT_KEYS, resultKeys(app))
        assertEquals(APP_RESULT_KEYS, resultKeys(shell))
        for (result in listOf(app, shell)) {
            assertEquals("completed", result.getString("state"))
            assertEquals(0, result.getInt("exit_code"))
            assertFalse(result.has("failure_code"))
            assertEquals("$OK_STDOUT\n", result.getString("stdout"))
            assertEquals("", result.getString("stderr"))
            assertFalse(result.getBoolean("stdout_truncated"))
            assertFalse(result.getBoolean("stderr_truncated"))
        }
        assertEquals("app", app.getString("execution_class"))
        assertEquals("shizuku", shell.getString("execution_class"))

        val failed = successResult(runtime, "app", FAIL_COMMAND)
        val failedShell = successResult(runtime, "shell", FAIL_COMMAND)
        assertEquals(FAILED_RESULT_KEYS, resultKeys(failed))
        assertEquals(FAILED_RESULT_KEYS, resultKeys(failedShell))
        for (result in listOf(failed, failedShell)) {
            assertEquals("failed", result.getString("state"))
            assertEquals("EXECUTION_FAILED", result.getString("failure_code"))
            assertEquals(7, result.getInt("exit_code"))
            assertEquals("$FAIL_STDOUT\n", result.getString("stdout"))
        }

        // Every bound S-TASK-002 and R-CMD-002 fix is refused through the one typed error
        // shape both hosts produce, before any process primitive is reached.
        for (input in listOf(
            commandInput("id -u", "app").put("max_output_bytes", 512),
            commandInput("id -u", "app").put("timeout_ms", 999),
            commandInput("", "app"),
        )) {
            for (runAsInput in listOf(input, JSONObject(input.toString()).put("run_as", "shell"))) {
                val refused = submit(runtime, commandRequest(runAsInput))
                assertEquals(refused.toString(), "error", refused.getString("outcome"))
                assertEquals(
                    refused.toString(),
                    "INVALID_ARGUMENT",
                    refused.getJSONObject("error").getString("code"),
                )
            }
        }
    }

    /**
     * S-AUTH-CMD-001: each identity request is reported with the identity that actually ran
     * the command. The report is checked against the uid the process really had, so a
     * substitution cannot pass as the requested identity.
     */
    @Test
    fun I8_CMD_G02_everyIdentityIsServedAndReportedExactlyAsRequested() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)

        assertIdentity(runtime, "app", Process.myUid().toString(), "app")
        assertIdentity(runtime, "shell", SHELL_UID, "shizuku")

        if (fixture == MAGISK_FIXTURE) {
            assertIdentity(runtime, "root", "0", "magisk")
        } else {
            val refused = submit(runtime, commandInput("id -u", "root").let(::commandRequest))
            assertEquals(refused.toString(), "error", refused.getString("outcome"))
            assertEquals(
                refused.toString(),
                "RUN_AS_UNAVAILABLE",
                refused.getJSONObject("error").getString("code"),
            )
        }
    }

    /**
     * Shizuku supplies one process primitive for the `shell` identity and never a second
     * Command implementation: the Shizuku package publishes only its provider grants, and
     * a shell command settles through the same shared result contract as `app` while
     * carrying the Shizuku execution class the resolver assigned.
     */
    @Test
    fun I8_CMD_G03_theShellIdentityConsumesTheShizukuProcessPrimitiveOnly() = withRuntime { runtime ->
        awaitAdmittedFixture(runtime)

        val status = requireNotNull(
            submit(runtime, contextStatusRequest()).optJSONObject("result"),
        )
        val grants = status.getJSONObject("grants")
        assertEquals("available", grants.getJSONObject("shizuku.shell").getString("state"))
        for (key in grants.keys()) {
            assertFalse("$key is not a provider grant", key.startsWith("command."))
        }

        val shell = successResult(runtime, "shell", OK_COMMAND)
        assertEquals("shizuku", shell.getString("execution_class"))
        assertEquals("shell", shell.getString("requested_run_as"))
        assertEquals("shell", shell.getString("actual_run_as"))
        assertEquals(APP_RESULT_KEYS, resultKeys(shell))
    }

    /**
     * S-TASK-002..003: the deadline, the retained-stream bound and the cancellation of one
     * execution all belong to the process the requested identity started. A timeout settles
     * as a `TIMEOUT` result instead of an error, a bounded stream keeps its own prefix, and
     * a cancel is answered only after the run it addressed has been reaped.
     */
    @Test
    fun I8_CMD_G04_deadlineBoundedOutputAndCancelStayWithTheRequestedIdentity() = withRuntime { runtime ->
        val fixture = awaitAdmittedFixture(runtime)

        val timedOut = successResult(
            runtime,
            primaryIdentity(fixture),
            SLEEP_COMMAND,
            JSONObject().put("timeout_ms", 1_000),
        )
        assertEquals("failed", timedOut.getString("state"))
        assertEquals("TIMEOUT", timedOut.getString("failure_code"))
        assertFalse(timedOut.has("exit_code"))
        assertTrue("${timedOut.getLong("duration_ms")}", timedOut.getLong("duration_ms") >= 1_000)
        assertTrue("${timedOut.getLong("duration_ms")}", timedOut.getLong("duration_ms") < 20_000)

        val bounded = successResult(
            runtime,
            "app",
            OUTPUT_COMMAND,
            JSONObject().put("max_output_bytes", 1_024),
        )
        assertEquals("completed", bounded.getString("state"))
        assertTrue(bounded.getBoolean("stdout_truncated"))
        assertEquals(1_024, bounded.getString("stdout").length)
        assertTrue(bounded.getString("stdout").all { byte -> byte == 'a' })
        assertFalse(bounded.has("stdout_ref"))
        assertEquals("", bounded.getString("stderr"))
        assertFalse(bounded.getBoolean("stderr_truncated"))

        assertCancelled(runtime, primaryIdentity(fixture))

        // An identity the surface does not own is refused instead of being served by the
        // runner of an identity it does own.
        if (fixture == MAGISK_FIXTURE) {
            assertIdentity(runtime, "shell", SHELL_UID, "shizuku")
        } else {
            val refused = submit(runtime, commandRequest(commandInput("id -u", "root")))
            assertEquals(refused.toString(), "error", refused.getString("outcome"))
            assertEquals(
                refused.toString(),
                "RUN_AS_UNAVAILABLE",
                refused.getJSONObject("error").getString("code"),
            )
        }
    }

    private fun assertIdentity(
        runtime: IDroidBridgeRuntime,
        runAs: String,
        expectedUid: String,
        expectedClass: String,
    ): JSONObject {
        val result = successResult(runtime, runAs, IDENTITY_COMMAND)
        assertEquals("completed", result.getString("state"))
        assertEquals(runAs, result.getString("requested_run_as"))
        assertEquals(runAs, result.getString("actual_run_as"))
        assertEquals(expectedClass, result.getString("execution_class"))
        assertEquals(expectedUid, result.getString("stdout").trim())
        return result
    }

    private fun assertCancelled(runtime: IDroidBridgeRuntime, runAs: String) {
        val accepted = submit(
            runtime,
            commandRequest(
                commandInput(SLEEP_COMMAND, runAs)
                    .put("as_task", true)
                    .put("timeout_ms", 150_000),
            ),
        )
        assertEquals(accepted.toString(), "success", accepted.getString("outcome"))
        val taskId = accepted.getJSONObject("result").getString("task_id")
        awaitTaskState(runtime, taskId, setOf("running"))
        val cancelled = awaitTaskState(
            runtime,
            taskId,
            setOf("cancelled"),
            cancel = true,
        )
        assertEquals("cancelled", cancelled.getString("state"))
    }

    private fun awaitTaskState(
        runtime: IDroidBridgeRuntime,
        taskId: String,
        expected: Set<String>,
        cancel: Boolean = false,
    ): JSONObject {
        if (cancel) {
            val response = submit(runtime, taskControlRequest("cancel", taskId))
            assertEquals(response.toString(), "success", response.getString("outcome"))
        }
        val deadline = SystemClock.elapsedRealtime() + TASK_DEADLINE_MS
        while (true) {
            val response = submit(runtime, taskControlRequest("get", taskId))
            assertEquals(response.toString(), "success", response.getString("outcome"))
            val snapshot = response.getJSONObject("result")
            if (snapshot.getString("state") in expected) return snapshot
            if (SystemClock.elapsedRealtime() >= deadline) {
                error("task $taskId did not reach $expected: $snapshot")
            }
            SystemClock.sleep(100)
        }
    }

    private fun successResult(
        runtime: IDroidBridgeRuntime,
        runAs: String,
        command: String,
        extra: JSONObject = JSONObject(),
    ): JSONObject {
        val response = submit(runtime, commandRequest(commandInput(command, runAs, extra)))
        assertEquals(response.toString(), "success", response.getString("outcome"))
        return response.getJSONObject("result")
    }

    private fun resultKeys(result: JSONObject): Set<String> = result.keys().asSequence().toSet()

    private fun commandInput(
        command: String,
        runAs: String,
        extra: JSONObject = JSONObject(),
    ): JSONObject {
        val input = JSONObject(extra.toString())
            .put("command", command)
            .put("run_as", runAs)
        return input
    }

    private fun primaryIdentity(fixture: String): String =
        if (fixture == MAGISK_FIXTURE) "root" else "shell"

    private fun commandRequest(input: JSONObject): ByteArray = JSONObject()
        .put("protocol_version", 1)
        .put("request_id", UUID.randomUUID().toString())
        .put(
            "payload",
            JSONObject().put("tool", "command").put("action", "run").put("input", input),
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

    // A freshly installed App runs the runtime before the fixture host and its grants are admitted,
    // and submissions during that window are refused. Every gate for this fixture starts from the
    // same admitted state instead of racing the host transition.
    private fun awaitAdmittedFixture(runtime: IDroidBridgeRuntime): String {
        val fixture = InstrumentationRegistry.getArguments()
            .getString("i8CommandFixture")
            ?: SHIZUKU_FIXTURE
        require(fixture == SHIZUKU_FIXTURE || fixture == MAGISK_FIXTURE) {
            "unknown I8 command fixture: $fixture"
        }
        if (InstrumentationRegistry.getArguments().getString("i8RequestShizuku") == "true") {
            assertTrue(runtime.requestShizukuAuthorization())
        }
        val deadline = SystemClock.elapsedRealtime() + ADMISSION_DEADLINE_MS
        while (true) {
            val status = submit(runtime, contextStatusRequest())
            if (fixtureAdmitted(status, fixture)) return fixture
            if (SystemClock.elapsedRealtime() >= deadline) {
                error("I8 command fixture $fixture was not admitted: $status")
            }
            SystemClock.sleep(100)
        }
    }

    private fun fixtureAdmitted(status: JSONObject, fixture: String): Boolean {
        val result = status.optJSONObject("result") ?: return false
        val host = result.optJSONObject("runtime")?.optString("host")
        val grants = result.optJSONObject("grants") ?: return false
        val appGuard = grants.optJSONObject("execution.app_guard")?.optString("state")
        val shizuku = grants.optJSONObject("shizuku.shell")?.optString("state")
        return if (fixture == SHIZUKU_FIXTURE) {
            host == "apk_runtime" && appGuard == "available" && shizuku == "available" &&
                grants.optJSONObject("execution.shell_guard")?.optString("state") == "available"
        } else {
            host == "magisk_backend" &&
                grants.optJSONObject("magisk.root")?.optString("state") == "available" &&
                appGuard == "available" && shizuku == "available"
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
        const val SHIZUKU_FIXTURE = "shizuku"
        const val MAGISK_FIXTURE = "magisk"
        const val SHELL_UID = "2000"
        const val OK_COMMAND = "echo droidbridge-command"
        const val FAIL_COMMAND = "echo failed-semantics; exit 7"
        const val OK_STDOUT = "droidbridge-command"
        const val FAIL_STDOUT = "failed-semantics"
        const val IDENTITY_COMMAND = "id -u"
        const val SLEEP_COMMAND = "sleep 30"
        const val OUTPUT_COMMAND = "dd if=/dev/zero bs=1 count=1500 2>/dev/null | tr '\\0' a"
        const val ADMISSION_DEADLINE_MS = 75_000L
        const val TASK_DEADLINE_MS = 60_000L
        val APP_RESULT_KEYS = setOf(
            "state",
            "exit_code",
            "requested_run_as",
            "actual_run_as",
            "execution_class",
            "duration_ms",
            "stdout",
            "stdout_truncated",
            "stderr",
            "stderr_truncated",
        )
        val FAILED_RESULT_KEYS = APP_RESULT_KEYS + "failure_code"
    }
}
