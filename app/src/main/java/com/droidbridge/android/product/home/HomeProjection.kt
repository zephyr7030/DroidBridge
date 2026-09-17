package com.droidbridge.android.product.home

import com.droidbridge.android.product.mcp.McpListenerState
import com.droidbridge.android.product.mcp.McpSettingsView

enum class HomeMcpRow { Off, Running, EnabledNotRunning }

/** An agent a live connection serves; the declaration order is the display order. */
enum class ConnectedAgent { LocalMcp, ChatGpt }

/**
 * What the one agent-connection entry states: every agent currently connected, or, with none
 * connected, whether that is known or a read failed.
 */
data class AgentConnectionSummary(val connected: List<ConnectedAgent>, val unreadable: Boolean)

/** The `ui.product.v1` Home update slot; UpdateManager owns `newer_version_available`. */
data class HomeUpdateSlot(
    val newerVersionAvailable: Boolean,
    val componentMismatch: Boolean,
) {
    val visible: Boolean get() = newerVersionAvailable || componentMismatch
}

object HomeProjection {
    const val TASK_PAGE_LIMIT = 500

    /** S-UI-017: a count equal to its one bounded page limit renders as `<limit>+`. */
    fun countLabel(count: Int, pageLimit: Int): String =
        if (count >= pageLimit) "$pageLimit+" else count.toString()

    fun mcpRow(settings: McpSettingsView): HomeMcpRow = when {
        !settings.enabled -> HomeMcpRow.Off
        settings.listener == McpListenerState.Running -> HomeMcpRow.Running
        else -> HomeMcpRow.EnabledNotRunning
    }

    /**
     * The one agent-connection entry: Local MCP, the ChatGPT tunnel and later agents are peers
     * under it, so it names each one that is connected. A failed read hides nothing that was read.
     */
    fun agentConnections(mcp: HomeMcpRow, tunnelRunning: Boolean, readFailed: Boolean): AgentConnectionSummary {
        val connected = buildList {
            if (mcp == HomeMcpRow.Running) add(ConnectedAgent.LocalMcp)
            if (tunnelRunning) add(ConnectedAgent.ChatGpt)
        }
        return AgentConnectionSummary(connected, unreadable = readFailed && connected.isEmpty())
    }

    /** Component mismatch is only an observed `incompatible` protocol or store-schema fact. */
    fun updateSlot(compatibility: Map<String, String>, newerVersionAvailable: Boolean = false): HomeUpdateSlot = HomeUpdateSlot(
        newerVersionAvailable = newerVersionAvailable,
        componentMismatch = compatibility["protocol"] == INCOMPATIBLE ||
            compatibility["store_schema"] == INCOMPATIBLE,
    )

    private const val INCOMPATIBLE = "incompatible"
}
