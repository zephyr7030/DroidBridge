package com.droidbridge.android

import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsError
import com.droidbridge.android.product.mcp.CHATGPT_APPS_SETTINGS_URL
import com.droidbridge.android.product.mcp.CHATGPT_CREATE_PLUGIN_URL
import com.droidbridge.android.product.mcp.OPENAI_API_KEYS_URL
import com.droidbridge.android.product.mcp.OPENAI_TUNNELS_URL
import com.droidbridge.android.product.mcp.TUNNEL_PLUGIN_NAME
import com.droidbridge.android.product.mcp.isTunnelConfigurationInputValid
import com.droidbridge.android.product.mcp.isTunnelPluginCreationReady
import com.droidbridge.android.product.mcp.isTunnelStepActionEnabled
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I13_TunnelProjectionTest {
    @Test
    fun projection_accepts_exact_configured_and_unconfigured_states() {
        val empty = TunnelSettingsReplies.settings(
            """{"schema_version":1,"configured":false,"enabled":false,"state":"stopped","protocol_version":"2026-07-28"}""",
        )
        requireNotNull(empty)
        assertFalse(empty.configured)
        assertNull(empty.tunnelId)

        val connected = TunnelSettingsReplies.settings(
            """{"schema_version":1,"configured":true,"enabled":true,"state":"running","tunnel_id":"tunnel_0123456789abcdefghijklmnopqrstuv","last_call_epoch_ms":1789495200000,"protocol_version":"2026-07-28"}""",
        )
        requireNotNull(connected)
        assertTrue(connected.enabled)
        assertEquals(TunnelRuntimeState.Running, connected.state)
        assertEquals(1789495200000, connected.lastCallEpochMs)
    }

    @Test
    fun projection_rejects_impossible_or_secret_bearing_states() {
        val impossible =
            """{"schema_version":1,"configured":false,"enabled":true,"state":"running","protocol_version":"2026-07-28"}"""
        val secretBearing =
            """{"schema_version":1,"configured":true,"enabled":false,"state":"stopped","tunnel_id":"tunnel_0123456789abcdefghijklmnopqrstuv","api_key":"secret","protocol_version":"2026-07-28"}"""

        assertNull(TunnelSettingsReplies.settings(impossible))
        assertNull(TunnelSettingsReplies.settings(secretBearing))
    }

    @Test
    fun configuration_input_requires_exact_tunnel_id_and_bounded_visible_ascii_key() {
        val tunnelId = "tunnel_0123456789abcdefghijklmnopqrstuv"

        assertTrue(isTunnelConfigurationInputValid(tunnelId, "sk-test"))
        assertFalse(isTunnelConfigurationInputValid(" tunnel_0123456789abcdefghijklmnopqrstuv", "sk-test"))
        assertFalse(isTunnelConfigurationInputValid(tunnelId, ""))
        assertFalse(isTunnelConfigurationInputValid(tunnelId, "line\nbreak"))
        assertFalse(isTunnelConfigurationInputValid(tunnelId, "x".repeat(513)))
    }

    @Test
    fun setup_links_use_the_official_https_surfaces() {
        assertEquals("https://platform.openai.com/settings/organization/tunnels", OPENAI_TUNNELS_URL)
        assertEquals("https://platform.openai.com/api-keys", OPENAI_API_KEYS_URL)
        assertEquals("https://chatgpt.com/plugins#settings/Plugins", CHATGPT_APPS_SETTINGS_URL)
        assertEquals(
            "https://chatgpt.com/plugins#settings/Connectors?create-connector=true&redirectAfter=%2Fplugins",
            CHATGPT_CREATE_PLUGIN_URL,
        )
        assertEquals("DroidBridge", TUNNEL_PLUGIN_NAME)
    }

    @Test
    fun plugin_creation_requires_the_runtime_and_a_running_tunnel() {
        assertTrue(isTunnelPluginCreationReady(runtimeReady = true, TunnelRuntimeState.Running))
        assertFalse(isTunnelPluginCreationReady(runtimeReady = false, TunnelRuntimeState.Running))
        assertFalse(isTunnelPluginCreationReady(runtimeReady = true, TunnelRuntimeState.Connecting))
        assertFalse(isTunnelPluginCreationReady(runtimeReady = true, TunnelRuntimeState.Stopped))
    }

    @Test
    fun ordered_actions_are_gated_only_during_first_configuration() {
        assertFalse(isTunnelStepActionEnabled(firstConfiguration = true, prerequisiteMet = false))
        assertTrue(isTunnelStepActionEnabled(firstConfiguration = true, prerequisiteMet = true))
        assertTrue(isTunnelStepActionEnabled(firstConfiguration = false, prerequisiteMet = false))
    }

    @Test
    fun configuration_errors_are_projected_without_showing_raw_payloads() {
        assertEquals(
            TunnelSettingsError.TunnelNotFound,
            TunnelSettingsReplies.error("""{"schema_version":1,"error":"TUNNEL_NOT_FOUND"}"""),
        )
        assertEquals(
            TunnelSettingsError.ApiKeyInvalid,
            TunnelSettingsReplies.error("""{"schema_version":1,"error":"API_KEY_INVALID"}"""),
        )
        assertNull(TunnelSettingsReplies.error("""{"schema_version":1,"error":"secret"}"""))
    }
}
