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

/**
 * What Home's status card says once the Runtime is ready: whether an AI agent can use this phone
 * now. A running Runtime alone is not that, so the card never reads as usable while no agent is
 * connected, while something still needs the user, or while a fact it depends on is unread.
 */
enum class HomeUsability { Usable, NoAgentConnected, NeedsAttention, Checking }

/** The `ui.product.v1` Home update slot; UpdateManager owns `newer_version_available`. */
data class HomeUpdateSlot(
    val newerVersionAvailable: Boolean,
    val componentMismatch: Boolean,
) {
    val visible: Boolean get() = newerVersionAvailable || componentMismatch
}

object HomeProjection {
    const val TASK_PAGE_LIMIT = 500
    /** Ended Tasks Home lists below the running ones, newest first. */
    const val ENDED_TASK_LIMIT = 50

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

    /**
     * [attention] counts the steps waiting on the user, [checking] whether a step is still being
     * determined. No connected agent comes first: nothing else matters to a caller that cannot reach
     * the phone.
     */
    fun usability(agents: AgentConnectionSummary, attention: Int, checking: Boolean): HomeUsability = when {
        agents.connected.isEmpty() && !agents.unreadable -> HomeUsability.NoAgentConnected
        attention > 0 -> HomeUsability.NeedsAttention
        agents.unreadable || checking -> HomeUsability.Checking
        else -> HomeUsability.Usable
    }

    /** Component mismatch is only an observed `incompatible` protocol or store-schema fact. */
    fun updateSlot(compatibility: Map<String, String>, newerVersionAvailable: Boolean = false): HomeUpdateSlot = HomeUpdateSlot(
        newerVersionAvailable = newerVersionAvailable,
        componentMismatch = compatibility["protocol"] == INCOMPATIBLE ||
            compatibility["store_schema"] == INCOMPATIBLE,
    )

    private const val INCOMPATIBLE = "incompatible"
}
