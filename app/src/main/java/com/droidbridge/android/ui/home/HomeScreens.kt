package com.droidbridge.android.ui.home

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.client.CapabilityRow
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.product.home.HomeMcpRow
import com.droidbridge.android.product.home.HomeProjection
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsView
import com.droidbridge.android.product.tasks.TaskFilter
import com.droidbridge.android.product.tasks.TaskRepository
import com.droidbridge.android.product.tasks.TaskResult
import com.droidbridge.android.ui.CapabilityListItem
import com.droidbridge.android.ui.common.ReasonText
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteContent
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.common.RouteLoading
import com.droidbridge.android.ui.common.routeContent
import com.droidbridge.android.ui.common.showsRefresh
import com.droidbridge.android.ui.mcp.agentConnectionLabel
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** The Home facts read from their owners; the Runtime status comes from the client snapshot. */
data class HomeProjectionState(
    val activeTasks: String,
    val activeTaskCount: Int,
    val mcp: HomeMcpRow,
)

data class HomeUiState(
    val projection: HomeProjectionState? = null,
    val loadFailed: Boolean = false,
    val refreshing: Boolean = false,
    val mcpFailed: Boolean = false,
    /** The tunnel is the other connection way; the one agent-connection row states whether either is up. */
    val tunnel: TunnelSettingsView? = null,
    val tunnelFailed: Boolean = false,
    /** Executions a lost instance left running, which keep the Magisk daemon from starting. */
    val strandedExecutions: Int = 0,
    val clearingStranded: Boolean = false,
    /** The error token of the last clear that did not succeed. */
    val strandedClearError: String? = null,
)

class HomeViewModel(
    private val client: DroidBridgeClient,
    private val tasks: TaskRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(HomeUiState())
    val state: StateFlow<HomeUiState> = mutableState.asStateFlow()

    /** A [quiet] refresh keeps the page current without showing the refresh indicator. */
    fun refresh(quiet: Boolean = false) {
        if (!quiet) mutableState.update { it.copy(refreshing = true) }
        viewModelScope.launch {
            val active = async { tasks.list(TaskFilter.Active, HomeProjection.TASK_PAGE_LIMIT) }
            val mcp = async { runCatching { client.mcpSettings() }.getOrNull()?.let(McpSettingsReplies::settings) }
            val tunnel = async { runCatching { client.tunnelSettings() }.getOrNull() }
            val stranded = async { runCatching { client.strandedExecutions() }.getOrDefault(0) }
            val activeTasks = active.await() as? TaskResult.Success
            val settings = mcp.await()
            val tunnelSettings = tunnel.await()?.let(TunnelSettingsReplies::settings)
            mutableState.update { current ->
                val tunnelFacts = current.copy(
                    tunnel = tunnelSettings ?: current.tunnel,
                    tunnelFailed = tunnelSettings == null,
                    strandedExecutions = stranded.await().coerceAtLeast(0),
                )
                if (activeTasks == null || settings == null) {
                    tunnelFacts.copy(refreshing = false, loadFailed = current.projection == null)
                } else {
                    tunnelFacts.copy(
                        projection = HomeProjectionState(
                            activeTasks = HomeProjection.countLabel(activeTasks.value.size, HomeProjection.TASK_PAGE_LIMIT),
                            activeTaskCount = activeTasks.value.size,
                            mcp = HomeProjection.mcpRow(settings),
                        ),
                        loadFailed = false,
                        refreshing = false,
                    )
                }
            }
        }
    }

    /** Settles the stranded executions so the daemon can start again, then rereads Home. */
    fun clearStrandedExecutions() {
        mutableState.update { it.copy(clearingStranded = true, strandedClearError = null) }
        viewModelScope.launch {
            val reply = runCatching { client.clearStrandedExecutions() }
            val error = reply.fold(
                { runCatching { org.json.JSONObject(it) }.getOrNull()?.takeIf { json -> !json.has("cleared") }?.optString("code", "INTERNAL_ERROR") },
                { "CAPABILITY_UNAVAILABLE" },
            )
            client.recheck()
            mutableState.update { it.copy(clearingStranded = false, strandedClearError = error) }
            refresh(quiet = true)
        }
    }
}

enum class HomeDestination { Diagnostics, Capabilities, AgentConnections, Updates }

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun HomeRoute(
    viewModel: HomeViewModel,
    clientState: ClientState,
    attention: List<CapabilityRow>,
    onCapabilityAction: (CapabilityRow) -> Unit,
    newerVersionAvailable: Boolean,
    open: (HomeDestination) -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // Connections and the store change without a Home event (a tunnel finishing its first poll, a
    // daemon failing to start), so the page rereads them while it is visible.
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(lifecycleOwner) {
        lifecycleOwner.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                delay(HOME_POLL_MILLIS)
                viewModel.refresh(quiet = true)
            }
        }
    }
    Scaffold(
        modifier = Modifier.testTag("route:Home"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
                actions = { if (showsRefresh(state.projection != null, state.refreshing)) RefreshIndicator("home") },
            )
        },
    ) { padding ->
        val projection = state.projection
        Box(Modifier.fillMaxSize().padding(padding)) {
            // A daemon that refuses to start leaves Home unreadable, which is exactly when the
            // recovery has to be reachable, so the card also stands above the error and loading states.
            val runtimeReady = (clientState as? ClientState.Available)?.snapshot?.readiness == RuntimeReadiness.Ready
            val showStranded = state.strandedExecutions > 0 && !runtimeReady
            when (routeContent(projection != null, state.loadFailed)) {
                RouteContent.Error, RouteContent.Loading -> Column(
                    Modifier.fillMaxSize().padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    if (showStranded) {
                        StrandedExecutionsCard(state.clearingStranded, state.strandedClearError, viewModel::clearStrandedExecutions)
                    }
                    Box(Modifier.weight(1f)) {
                        if (projection == null && state.loadFailed) RouteError("home") { viewModel.refresh() } else RouteLoading("home")
                    }
                }
                RouteContent.Empty, RouteContent.Content ->
                    HomeContent(
                        requireNotNull(projection), state, clientState, attention, onCapabilityAction,
                        newerVersionAvailable, open, viewModel::clearStrandedExecutions,
                    )
            }
        }
    }
}

@Composable
private fun HomeContent(
    projection: HomeProjectionState,
    state: HomeUiState,
    clientState: ClientState,
    attention: List<CapabilityRow>,
    onCapabilityAction: (CapabilityRow) -> Unit,
    newerVersionAvailable: Boolean,
    open: (HomeDestination) -> Unit,
    clearStranded: () -> Unit,
) {
    val snapshot = (clientState as? ClientState.Available)?.snapshot
    val slot = HomeProjection.updateSlot(snapshot?.compatibility.orEmpty(), newerVersionAvailable)
    val transparent = ListItemDefaults.colors(containerColor = Color.Transparent)
    val agentState = HomeProjection.agentConnections(
        mcp = projection.mcp,
        tunnelRunning = state.tunnel?.state == TunnelRuntimeState.Running,
        readFailed = state.mcpFailed || state.tunnelFailed,
    )
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { StatusCard(clientState) { open(HomeDestination.Diagnostics) } }
        // Only while the Runtime is not ready: a live Runtime settles its own executions.
        if (state.strandedExecutions > 0 && snapshot?.readiness != RuntimeReadiness.Ready) {
            item { StrandedExecutionsCard(state.clearingStranded, state.strandedClearError, clearStranded) }
        }
        item {
            Column {
                if (attention.isNotEmpty()) SectionTitle(R.string.home_attention)
                GroupCard {
                    attention.forEach { row -> CapabilityListItem(row, refreshing = false, colors = transparent) { onCapabilityAction(row) } }
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.capabilities_title)) },
                        supportingContent = if (attention.isEmpty()) ({ Text(stringResource(R.string.home_all_ready)) }) else null,
                        leadingContent = if (attention.isEmpty()) ({
                            Icon(painterResource(R.drawable.ic_status_success), contentDescription = null)
                        }) else null,
                        colors = transparent,
                        modifier = Modifier.clickable { open(HomeDestination.Capabilities) }.testTag("home:capabilities"),
                    )
                }
            }
        }
        item {
            GroupCard {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.agent_connection_title)) },
                    supportingContent = { Text(agentConnectionLabel(agentState)) },
                    colors = transparent,
                    modifier = Modifier.clickable { open(HomeDestination.AgentConnections) }.testTag("home:agent"),
                )
            }
        }
        if (slot.visible) {
            item {
                GroupCard {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.home_updates)) },
                        leadingContent = { Icon(painterResource(R.drawable.ic_system_update), contentDescription = null) },
                        colors = transparent,
                        modifier = Modifier.clickable { open(HomeDestination.Updates) }.testTag("home:updates"),
                    )
                }
            }
        }
    }
}

@Composable
private fun StatusCard(clientState: ClientState, open: () -> Unit) {
    val status = runtimeStatus(clientState)
    val colors = MaterialTheme.colorScheme
    Card(
        onClick = open,
        colors = CardDefaults.cardColors(
            containerColor = when (status.tone) {
                StatusTone.Ready -> colors.primaryContainer
                StatusTone.Pending -> colors.secondaryContainer
                StatusTone.Failed -> colors.errorContainer
            },
        ),
        modifier = Modifier.fillMaxWidth().testTag("home:status"),
    ) {
        Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(painterResource(status.icon), contentDescription = null, modifier = Modifier.size(32.dp))
            Column(Modifier.padding(start = 16.dp)) {
                Text(stringResource(R.string.home_status_title), style = MaterialTheme.typography.labelLarge)
                Text(stringResource(status.label), style = MaterialTheme.typography.titleLarge)
                (clientState as? ClientState.Unavailable)?.reason?.let { reason ->
                    Text(stringResource(ReasonText.resource(reason)), style = MaterialTheme.typography.bodyMedium)
                }
            }
        }
    }
}

@Composable
private fun StrandedExecutionsCard(clearing: Boolean, error: String?, clear: () -> Unit) {
    var confirming by remember { mutableStateOf(false) }
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
        modifier = Modifier.fillMaxWidth().testTag("home:stranded"),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.stranded_title), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.stranded_body), style = MaterialTheme.typography.bodyMedium)
            error?.let {
                Text(
                    stringResource(R.string.stranded_failed, it),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag("home:stranded:error"),
                )
            }
            Button(
                onClick = { confirming = true },
                enabled = !clearing,
                modifier = Modifier.align(Alignment.End).testTag("home:stranded:clear"),
            ) { Text(stringResource(R.string.stranded_action)) }
        }
    }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text(stringResource(R.string.stranded_confirm_title)) },
            text = { Text(stringResource(R.string.stranded_confirm_body)) },
            confirmButton = {
                TextButton(onClick = { confirming = false; clear() }, modifier = Modifier.testTag("home:stranded:confirm")) {
                    Text(stringResource(R.string.action_confirm))
                }
            },
            dismissButton = { TextButton(onClick = { confirming = false }) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
}

private const val HOME_POLL_MILLIS = 5_000L

@Composable
private fun SectionTitle(@StringRes title: Int) {
    Text(
        stringResource(title),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
    )
}

@Composable
private fun GroupCard(content: @Composable () -> Unit) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.fillMaxWidth(),
    ) { content() }
}

private enum class StatusTone { Ready, Pending, Failed }

private data class RuntimeStatus(@StringRes val label: Int, @DrawableRes val icon: Int, val tone: StatusTone)

private fun runtimeStatus(clientState: ClientState): RuntimeStatus = when (clientState) {
    is ClientState.Available -> when (clientState.snapshot.readiness) {
        RuntimeReadiness.Ready -> RuntimeStatus(R.string.state_ready, R.drawable.ic_status_success, StatusTone.Ready)
        RuntimeReadiness.Initializing -> RuntimeStatus(R.string.state_starting, R.drawable.ic_status_schedule, StatusTone.Pending)
        RuntimeReadiness.Unavailable -> RuntimeStatus(R.string.state_unavailable, R.drawable.ic_status_error, StatusTone.Failed)
    }
    ClientState.Connecting, ClientState.Disconnected ->
        RuntimeStatus(R.string.state_starting, R.drawable.ic_status_schedule, StatusTone.Pending)
    is ClientState.Unavailable -> RuntimeStatus(R.string.state_unavailable, R.drawable.ic_status_error, StatusTone.Failed)
}
