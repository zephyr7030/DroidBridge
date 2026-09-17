package com.droidbridge.android

import com.droidbridge.android.runtimehost.EncryptedTunnelCredential
import com.droidbridge.android.runtimehost.McpSettingsFileSystem
import com.droidbridge.android.runtimehost.TUNNEL_RUNNING
import com.droidbridge.android.runtimehost.TUNNEL_STOPPED
import com.droidbridge.android.runtimehost.TUNNEL_VALIDATION_UNAVAILABLE
import com.droidbridge.android.runtimehost.TunnelCredentialCipher
import com.droidbridge.android.runtimehost.TunnelNetworkMonitor
import com.droidbridge.android.runtimehost.TunnelRuntimePort
import com.droidbridge.android.runtimehost.TunnelSettingsController
import java.io.File
import java.nio.file.Files
import java.util.UUID
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class I13_TunnelSettingsControllerTest {
    private val directory: File = Files.createTempDirectory("droidbridge-i13-tunnel").toFile()

    private val runtime = FakeRuntime()
    private val network = FakeNetwork()
    private val cipher = FakeCipher()
    private val foreground = mutableListOf<Boolean>()

    private fun controller(
        runtime: FakeRuntime = this.runtime,
        network: FakeNetwork = this.network,
    ) = TunnelSettingsController(directory, runtime, network, cipher, FakeFileSystem())

    @Test
    fun credentials_commit_before_enable_and_plaintext_never_reaches_disk() {
        val controller = controller()
        assertFalse(boolean(controller.settings(), "configured"))

        val configured = controller.configure(TUNNEL_ID, API_KEY, foreground::add)
        assertTrue(boolean(configured, "configured"))
        assertFalse(boolean(configured, "enabled"))
        assertEquals(TUNNEL_ID, string(configured, "tunnel_id"))
        assertFalse(File(directory, "tunnel.json").readText().contains(API_KEY))
        assertTrue(foreground.isEmpty())

        val enabled = controller.setEnabled(true, foreground::add)
        assertTrue(boolean(enabled, "enabled"))
        assertEquals(listOf(true), foreground)
        assertEquals(0, runtime.starts)

        network.emit(true)
        assertEquals(1, runtime.starts)
        assertEquals(TUNNEL_ID, runtime.tunnelId)
        assertEquals(API_KEY, runtime.apiKey)
        assertEquals("running", string(controller.settings(), "state"))
    }

    @Test
    fun invalid_remote_credentials_are_distinguished_and_never_committed() {
        runtime.validation = "invalid_tunnel"
        assertEquals("TUNNEL_NOT_FOUND", error(controller().configure(TUNNEL_ID, API_KEY, foreground::add)))
        assertFalse(File(directory, "tunnel.json").exists())

        runtime.validation = "invalid_key"
        assertEquals("API_KEY_INVALID", error(controller().configure(TUNNEL_ID, API_KEY, foreground::add)))
        assertFalse(File(directory, "tunnel.json").exists())
    }

    @Test
    fun an_unclassified_validation_is_reported_as_openai_unavailable_and_never_committed() {
        runtime.validation = TUNNEL_VALIDATION_UNAVAILABLE
        assertEquals("OPENAI_UNAVAILABLE", error(controller().configure(TUNNEL_ID, API_KEY, foreground::add)))
        assertFalse(File(directory, "tunnel.json").exists())

        runtime.validation = "unclassified_native_result"
        assertEquals("OPENAI_UNAVAILABLE", error(controller().configure(TUNNEL_ID, API_KEY, foreground::add)))
        assertFalse(File(directory, "tunnel.json").exists())
    }

    @Test
    fun network_loss_and_stale_callbacks_cannot_revive_a_disabled_tunnel() {
        val controller = controller()
        controller.configure(TUNNEL_ID, API_KEY, foreground::add)
        controller.setEnabled(true, foreground::add)
        network.emit(true)
        assertEquals(1, runtime.starts)

        network.emit(false)
        assertEquals(TUNNEL_STOPPED, runtime.state())
        assertEquals("connecting", string(controller.settings(), "state"))

        val stale = network.callback
        controller.setEnabled(false, foreground::add)
        stale?.invoke(true)
        assertEquals(1, runtime.starts)
        assertEquals(TUNNEL_STOPPED, runtime.state())
        assertEquals(listOf(true, false), foreground)
    }

    @Test
    fun enabled_configuration_restores_after_process_recreation_and_clear_removes_it() {
        controller().run {
            configure(TUNNEL_ID, API_KEY, foreground::add)
            setEnabled(true, foreground::add)
            suspendRuntime(foreground::add)
        }

        val restoredRuntime = FakeRuntime()
        val restoredNetwork = FakeNetwork()
        val restored = controller(restoredRuntime, restoredNetwork)
        restored.restore(foreground::add)
        restoredNetwork.emit(true)
        assertEquals(1, restoredRuntime.starts)
        assertEquals(TUNNEL_RUNNING, restoredRuntime.state())

        val cleared = restored.clear(foreground::add)
        assertFalse(boolean(cleared, "configured"))
        assertFalse(File(directory, "tunnel.json").exists())
        assertTrue(cipher.deleted)
    }

    private fun boolean(reply: String, name: String): Boolean =
        Json.parseToJsonElement(reply).jsonObject.getValue(name).jsonPrimitive.content.toBooleanStrict()

    private fun string(reply: String, name: String): String =
        Json.parseToJsonElement(reply).jsonObject.getValue(name).jsonPrimitive.content

    private fun error(reply: String): String = string(reply, "error")

    private class FakeRuntime : TunnelRuntimePort {
        var starts = 0
        var validation = "valid"
        var tunnelId: String? = null
        var apiKey: String? = null
        private var current = TUNNEL_STOPPED

        override fun validate(tunnelId: String, apiKey: String): String = validation

        override fun start(tunnelId: String, apiKey: String): Boolean {
            starts += 1
            this.tunnelId = tunnelId
            this.apiKey = apiKey
            current = TUNNEL_RUNNING
            return true
        }

        override fun stop(): Boolean {
            current = TUNNEL_STOPPED
            return true
        }

        override fun state(): String = current

        override fun lastCallEpochMs(): Long = 0
    }

    private class FakeNetwork : TunnelNetworkMonitor {
        var callback: ((Boolean) -> Unit)? = null

        override fun start(changed: (Boolean) -> Unit): Boolean {
            callback = changed
            return true
        }

        override fun stop() = Unit

        fun emit(available: Boolean) {
            callback?.invoke(available)
        }
    }

    private class FakeCipher : TunnelCredentialCipher {
        private val values = mutableMapOf<String, String>()
        var deleted = false

        override fun encrypt(tunnelId: String, apiKey: String): EncryptedTunnelCredential {
            val id = UUID.randomUUID().toString()
            values[id] = apiKey
            return EncryptedTunnelCredential(id, "test-iv")
        }

        override fun decrypt(tunnelId: String, credential: EncryptedTunnelCredential): String =
            checkNotNull(values[credential.ciphertext])

        override fun deleteKey() {
            deleted = true
            values.clear()
        }
    }

    private class FakeFileSystem : McpSettingsFileSystem {
        override fun restrictToOwner(file: File) = Unit
        override fun isOwnerOnly(file: File): Boolean = true
        override fun syncDirectory(directory: File) = Unit
    }

    private companion object {
        const val TUNNEL_ID = "tunnel_0123456789abcdefghijklmnopqrstuv"
        const val API_KEY = "test-runtime-api-key"
    }
}
