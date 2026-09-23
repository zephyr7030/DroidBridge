package com.droidbridge.android.ui.mcp

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.droidbridge.android.R
import com.droidbridge.android.ui.common.RowIcon
import com.droidbridge.android.product.home.AgentConnectionSummary
import com.droidbridge.android.product.home.ConnectedAgent
import com.droidbridge.android.product.home.HomeMcpRow
import com.droidbridge.android.product.home.HomeProjection
import com.droidbridge.android.ui.settings.BackButton

/**
 * The parent level of the agent connection: Local MCP and the ChatGPT tunnel are peers under this one
 * entry, and a later way is one more row here. Home and Settings both open this same page.
 */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AgentConnectionRoute(
    viewModel: McpViewModel,
    openMcp: () -> Unit,
    openTunnel: () -> Unit,
    back: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val settings = state.settings
    val mcpLabel = settings?.let { mcpStateLabel(HomeProjection.mcpRow(it)) }
    val tunnelLabel = when {
        settings == null -> null
        state.tunnelFailed -> R.string.state_error
        else -> tunnelStatusLabel(state.tunnelSettings)
    }
    Scaffold(
        modifier = Modifier.testTag("route:AgentConnections"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.agent_connection_title)) },
                navigationIcon = { BackButton("route:AgentConnections", R.string.agent_connection_title, back) },
            )
        },
    ) { padding ->
        LazyColumn(modifier = Modifier.fillMaxSize().padding(padding)) {
            if (state.failed) {
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.state_error)) },
                        trailingContent = {
                            Button(onClick = viewModel::refresh, modifier = Modifier.testTag("agent:retry")) {
                                Text(stringResource(R.string.action_retry))
                            }
                        },
                        modifier = Modifier.testTag("agent:state"),
                    )
                }
            }
            item { WayRow(R.string.home_mcp, R.drawable.ic_lan, mcpLabel, "agent:local_mcp", openMcp) }
            item { WayRow(R.string.mcp_chatgpt_connection, R.drawable.ic_cloud, tunnelLabel, "agent:chatgpt", openTunnel) }
        }
    }
    LaunchedEffect(Unit) { viewModel.refresh() }
}

/** One connection way: its own name, the state its owner reports, and its own page. */
@Composable
private fun WayRow(@StringRes title: Int, @DrawableRes icon: Int, @StringRes state: Int?, tag: String, open: () -> Unit) {
    val label = state
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        supportingContent = if (label != null) ({ Text(stringResource(label)) }) else null,
        leadingContent = { RowIcon(icon) },
        modifier = Modifier.clickable(onClick = open).testTag(tag),
    )
}

@StringRes
internal fun mcpStateLabel(row: HomeMcpRow): Int = when (row) {
    HomeMcpRow.Off -> R.string.mcp_state_off
    HomeMcpRow.Running -> R.string.mcp_state_running
    HomeMcpRow.EnabledNotRunning -> R.string.mcp_state_enabled_not_running
}

/** "本地 MCP和ChatGPT 已连接": every connected agent, joined the way the locale lists names. */
@Composable
internal fun agentConnectionLabel(summary: AgentConnectionSummary): String = when {
    summary.connected.isNotEmpty() -> {
        val names = summary.connected.map { agent ->
            stringResource(
                when (agent) {
                    ConnectedAgent.LocalMcp -> R.string.home_mcp
                    ConnectedAgent.ChatGpt -> R.string.agent_name_chatgpt
                },
            )
        }
        stringResource(R.string.agent_connected, android.icu.text.ListFormatter.getInstance().format(names))
    }
    summary.unreadable -> stringResource(R.string.state_error)
    else -> stringResource(R.string.agent_connection_idle)
}
