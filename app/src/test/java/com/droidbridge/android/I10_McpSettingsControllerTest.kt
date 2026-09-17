package com.droidbridge.android

import com.droidbridge.android.product.mcp.McpListenerState
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.runtimehost.McpListenerPort
import com.droidbridge.android.runtimehost.McpSettingsController
import com.droidbridge.android.runtimehost.McpSettingsFileSystem
import java.io.File
import java.nio.file.Files
import java.util.Base64
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I10_McpSettingsControllerTest {
    private val directory: File = Files.createTempDirectory("droidbridge-i10-mcp").toFile()
    private val listener = FakeListener()
    private val fileSystem = FakeFileSystem()
    private val foreground = mutableListOf<Boolean>()

    @After
    fun cleanUp() {
        directory.deleteRecursively()
    }

    private fun controller() = McpSettingsController(directory, listener, DEBUG_PORT, fileSystem)

    private fun file(): Map<String, Any> {
        val value = Json.parseToJsonElement(File(directory, "mcp.json").readText()).jsonObject
        assertEquals(setOf("schema_version", "enabled", "token"), value.keys)
        return mapOf(
            "enabled" to value.getValue("enabled").jsonPrimitive.boolean,
            "token" to value.getValue("token").jsonPrimitive.content,
        )
    }

    @Test
    fun I10_G06_missingFileIsCreatedDisabledWithAFreshOwnerOnlyToken() {
        val reply = requireNotNull(McpSettingsReplies.settings(controller().settings()))

        assertFalse(reply.enabled)
        assertEquals(McpListenerState.Stopped, reply.listener)
        assertEquals("http://127.0.0.1:18765/mcp", reply.endpoint)
        assertEquals("2026-07-28", reply.protocolVersion)
        val token = file()["token"] as String
        assertEquals(32, Base64.getUrlDecoder().decode(token).size)
        assertFalse(token.contains('='))
        assertTrue(fileSystem.restricted.isNotEmpty())
        assertFalse(File(directory, "mcp.json.tmp").exists())
        // The settings reply never carries the token.
        assertFalse(controller().settings().contains(token))
    }

    @Test
    fun I10_G06_enableCommitsThenTakesForegroundThenBindsTheCommittedToken() {
        val controller = controller()
        val reply = requireNotNull(McpSettingsReplies.settings(controller.setEnabled(true, foreground::add)))

        assertTrue(reply.enabled)
        assertEquals(McpListenerState.Running, reply.listener)
        assertEquals(listOf(true), foreground)
        assertEquals(true, file()["enabled"])
        assertEquals(file()["token"], listener.token)
        assertEquals(listOf("commit", "foreground", "start"), fileSystem.events.take(1) + listener.events)

        val disabled = requireNotNull(McpSettingsReplies.settings(controller.setEnabled(false, foreground::add)))
        assertFalse(disabled.enabled)
        assertEquals(McpListenerState.Stopped, disabled.listener)
        assertEquals(listOf(true, false), foreground)
        assertEquals(false, file()["enabled"])
    }

    @Test
    fun I10_G06_listenerAndForegroundFailuresKeepEnabledCommittedWithTheirReasons() {
        val rejecting = McpSettingsController(directory, listener, DEBUG_PORT, fileSystem)
        val rejected = requireNotNull(
            McpSettingsReplies.settings(rejecting.setEnabled(true) { throw IllegalStateException("background") }),
        )
        assertEquals(McpListenerState.Failed, rejected.listener)
        assertEquals("FGS_START_REJECTED", rejected.reason)
        assertEquals(true, file()["enabled"])
        assertNull(listener.token)

        listener.bindable = false
        val failed = requireNotNull(McpSettingsReplies.settings(controller().setEnabled(true, foreground::add)))
        assertEquals(McpListenerState.Failed, failed.listener)
        assertEquals("MCP_LISTENER_FAILED", failed.reason)
        // The keeper reason is released when no listener needs it.
        assertEquals(listOf(true, false), foreground)
    }

    @Test
    fun I10_G06_rotationCommitsBeforeReplacingTheAcceptedToken() {
        val controller = controller()
        controller.setEnabled(true, foreground::add)
        val old = requireNotNull(McpSettingsReplies.token(controller.reveal()))

        controller.rotate()
        val rotated = requireNotNull(McpSettingsReplies.token(controller.reveal()))
        assertNotEquals(old, rotated)
        assertEquals(rotated, file()["token"])
        assertEquals(rotated, listener.token)

        // A failed commit leaves the file, the snapshot and the listener token unchanged.
        fileSystem.failRestrict = true
        assertEquals(IO_ERROR, controller.rotate())
        fileSystem.failRestrict = false
        assertEquals(rotated, listener.token)
        assertEquals(rotated, McpSettingsReplies.token(controller.reveal()))
    }

    @Test
    fun I10_G06_malformedOrExposedSettingsFailEveryMethodAndNeverStartTheListener() {
        for (content in listOf(
            "{\"schema_version\":1,\"enabled\":true}",
            "{\"schema_version\":2,\"enabled\":true,\"token\":\"$VALID_TOKEN\"}",
            "{\"schema_version\":1,\"enabled\":\"true\",\"token\":\"$VALID_TOKEN\"}",
            "{\"schema_version\":1,\"enabled\":true,\"token\":\"short\"}",
            "not json",
        )) {
            File(directory, "mcp.json").writeText(content)
            val controller = controller()
            assertEquals(content, IO_ERROR, controller.settings())
            assertEquals(content, IO_ERROR, controller.setEnabled(true, foreground::add))
            assertEquals(content, IO_ERROR, controller.rotate())
            assertEquals(content, IO_ERROR, controller.reveal())
            controller.restore(foreground::add)
            assertEquals(content, File(directory, "mcp.json").readText())
        }
        fileSystem.ownerOnly = false
        File(directory, "mcp.json").writeText("{\"schema_version\":1,\"enabled\":true,\"token\":\"$VALID_TOKEN\"}")
        assertEquals(IO_ERROR, controller().settings())
        assertTrue(listener.events.isEmpty())
        assertTrue(foreground.isEmpty())
    }

    @Test
    fun I10_G06_restoreStartsOnlyAnEnabledStoppedListener() {
        File(directory, "mcp.json").writeText("{\"schema_version\":1,\"enabled\":false,\"token\":\"$VALID_TOKEN\"}")
        controller().restore(foreground::add)
        assertTrue(listener.events.isEmpty())

        File(directory, "mcp.json").writeText("{\"schema_version\":1,\"enabled\":true,\"token\":\"$VALID_TOKEN\"}")
        val controller = controller()
        controller.restore(foreground::add)
        assertEquals(listOf("foreground", "start"), listener.events)
        assertEquals(VALID_TOKEN, listener.token)
        controller.restore(foreground::add)
        assertEquals(listOf("foreground", "start"), listener.events)

        controller.suspendListener()
        assertEquals(McpListenerState.Stopped, McpSettingsReplies.settings(controller.settings())?.listener)
        assertEquals(true, file()["enabled"])
    }

    private inner class FakeListener : McpListenerPort {
        var token: String? = null
        var bindable = true
        private var running = false
        val events = mutableListOf<String>()

        override fun start(port: Int, token: String): Boolean {
            assertEquals(DEBUG_PORT, port)
            // The keeper reason is always taken before the bind.
            events += "foreground".takeIf { foreground.lastOrNull() == true } ?: "start-without-foreground"
            events += "start"
            if (!bindable) return false
            this.token = token
            running = true
            return true
        }

        override fun setToken(token: String): Boolean {
            if (running) this.token = token
            return true
        }

        override fun stop(): Boolean {
            running = false
            return true
        }

        override fun state(): String = if (running) "running" else "stopped"
    }

    private class FakeFileSystem : McpSettingsFileSystem {
        val restricted = mutableListOf<File>()
        val events = mutableListOf<String>()
        var failRestrict = false
        var ownerOnly = true

        override fun restrictToOwner(file: File) {
            if (failRestrict) throw java.io.IOException("chmod failed")
            restricted += file
        }

        override fun isOwnerOnly(file: File): Boolean = ownerOnly

        override fun syncDirectory(directory: File) {
            events += "commit"
        }
    }

    private companion object {
        const val DEBUG_PORT = 18765
        const val IO_ERROR = "{\"schema_version\":1,\"error\":\"IO_ERROR\"}"
        const val VALID_TOKEN = "tXw1sO3n6b2WqQm9J0gKf8yVhZcR4dLpA7eNuTiYkMs"
    }
}
