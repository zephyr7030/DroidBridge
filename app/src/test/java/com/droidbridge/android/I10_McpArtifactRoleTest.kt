package com.droidbridge.android

import com.droidbridge.android.product.mcp.McpListenerState
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.runtimehost.DaemonMessageKind
import com.droidbridge.android.runtimehost.DaemonOperationToken
import com.droidbridge.android.runtimehost.DaemonWireCodec
import com.droidbridge.android.runtimehost.DaemonWireEnvelope
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class I10_McpArtifactRoleTest {
    @Test
    fun I10_G07_forwardedArtifactReadCarriesExactlyTheMcpArtifactRole() {
        val response = DaemonWireEnvelope(
            kind = DaemonMessageKind.Response,
            messageId = "99400000-0000-4000-8000-000000000001",
            replyTo = "99400000-0000-4000-8000-000000000002",
            runtimeEpoch = "99400000-0000-4000-8000-000000000003",
            hostGeneration = 2,
            runtimeInstanceId = "99400000-0000-4000-8000-000000000004",
            operation = DaemonOperationToken.RuntimeForward,
            payload = buildJsonObject {
                put("uri", "dbref:stdout:99300000-0000-4000-8000-000000000001")
                put("kind", "stdout")
                put("size", 6)
            },
            fdRoles = listOf("mcp_artifact"),
        )

        val decoded = DaemonWireCodec.decode(DaemonWireCodec.encode(response), 1)
        assertEquals(listOf("mcp_artifact"), decoded.fdRoles)
        assertEquals(response.payload, decoded.payload)
    }

    @Test
    fun I10_G06_settingsRepliesAreAcceptedOnlyInTheirExactShape() {
        val running = McpSettingsReplies.settings(
            "{\"schema_version\":1,\"enabled\":true,\"listener\":\"running\"," +
                "\"endpoint\":\"http://127.0.0.1:8765/mcp\",\"protocol_version\":\"2026-07-28\"}",
        )
        assertEquals(McpListenerState.Running, running?.listener)
        assertNull(running?.reason)

        val failed = McpSettingsReplies.settings(
            "{\"schema_version\":1,\"enabled\":true,\"listener\":\"failed\",\"reason\":\"FGS_START_REJECTED\"," +
                "\"endpoint\":\"http://127.0.0.1:8765/mcp\",\"protocol_version\":\"2026-07-28\"}",
        )
        assertEquals("FGS_START_REJECTED", failed?.reason)

        for (reply in listOf(
            "{\"schema_version\":1,\"error\":\"IO_ERROR\"}",
            "{\"schema_version\":1,\"enabled\":true,\"listener\":\"running\",\"reason\":\"X\"," +
                "\"endpoint\":\"e\",\"protocol_version\":\"2026-07-28\"}",
            "{\"schema_version\":1,\"enabled\":true,\"listener\":\"running\",\"token\":\"t\"," +
                "\"endpoint\":\"e\",\"protocol_version\":\"2026-07-28\"}",
        )) {
            assertNull(reply, McpSettingsReplies.settings(reply))
        }
        assertEquals("abc", McpSettingsReplies.token("{\"schema_version\":1,\"token\":\"abc\"}"))
        assertNull(McpSettingsReplies.token("{\"schema_version\":1,\"error\":\"IO_ERROR\"}"))
    }
}
