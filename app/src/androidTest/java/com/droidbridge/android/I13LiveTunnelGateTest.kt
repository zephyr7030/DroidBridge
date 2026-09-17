package com.droidbridge.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsError
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class I13LiveTunnelGateTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    /** The credential validation reaches the OpenAI control plane and classifies its refusal. */
    @Test
    fun invalidApiKeyReachesTheControlPlaneAndIsClassified() = runBlocking {
        val client = DroidBridgeClient(context)
        try {
            client.bind()
            withTimeout(RUNTIME_AVAILABLE_TIMEOUT_MS) {
                client.state.filterIsInstance<ClientState.Available>().first()
            }
            assertEquals(
                TunnelSettingsError.ApiKeyInvalid,
                TunnelSettingsReplies.error(client.configureTunnel(TUNNEL_ID, PROBE_KEY)),
            )
        } finally {
            client.unbind()
        }
    }

    @Test
    fun liveCredentialIsEncryptedAndTheNativeTunnelReachesTheControlPlane() = runBlocking {
        val input = File(context.filesDir, "tunnel-gate.json")
        assumeTrue(input.isFile)
        val fixture = JSONObject(input.readText())
        assertTrue(input.delete())
        val tunnelId = fixture.getString("tunnel_id")
        val apiKey = fixture.getString("api_key")
        val client = DroidBridgeClient(context)
        try {
            client.bind()
            withTimeout(RUNTIME_AVAILABLE_TIMEOUT_MS) {
                client.state.filterIsInstance<ClientState.Available>().first()
            }
            val configured = TunnelSettingsReplies.settings(client.configureTunnel(tunnelId, apiKey))
            assertTrue(configured?.configured == true)
            assertTrue(TunnelSettingsReplies.settings(client.setTunnelEnabled(true))?.enabled == true)
            val running = withTimeout(45_000) {
                var current = TunnelSettingsReplies.settings(client.tunnelSettings())
                while (current?.state != TunnelRuntimeState.Running) {
                    delay(500)
                    current = TunnelSettingsReplies.settings(client.tunnelSettings())
                }
                current
            }
            assertTrue(running.enabled)
            val stored = File(
                context.createDeviceProtectedStorageContext().filesDir,
                "droidbridge/tunnel.json",
            ).readText()
            assertFalse(stored.contains(apiKey))
        } finally {
            client.unbind()
        }
    }

    private companion object {
        /**
         * A cold runtime process builds its network attachment and execution graph before the
         * client can reach the Runtime, and the first snapshot follows the graph, so the wait
         * covers more than a binder connection.
         */
        const val RUNTIME_AVAILABLE_TIMEOUT_MS = 60_000L
        /** Any well-formed ID: the control plane refuses the key before it looks the tunnel up. */
        const val TUNNEL_ID = "tunnel_00000000000000000000000000000000"
        const val PROBE_KEY = "sk-invalid-for-connectivity-probe"
    }
}
