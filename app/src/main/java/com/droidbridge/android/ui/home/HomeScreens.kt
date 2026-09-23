package com.droidbridge.android.ui.home

import com.droidbridge.android.product.runtime.PublicResult
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
import androidx.compose.material3.Badge
import androidx.compose.foundation.Image
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.graphics.asImageBitmap
import androidx.core.graphics.drawable.toBitmap
import com.droidbridge.android.R
import com.droidbridge.android.product.tasks.TaskSummary
import com.droidbridge.android.ui.tasks.TaskRow
import com.droidbridge.android.ui.common.RowIcon
import com.droidbridge.android.client.CapabilityRow
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.product.home.HomeMcpRow
import com.droidbridge.android.product.home.HomeProjection
import com.droidbridge.android.product.home.HomeUsability
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsView
import com.droidbridge.android.product.tasks.TaskFilter
import com.droidbridge.android.product.tasks.TaskRepository
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
    val activeTaskCount: Int,
    val mcp: HomeMcpRow,
    /** Running work first, then the most recent Tasks that ended. */
    val tasks: List<TaskSummary> = emptyList(),
)

data class HomeUiState(
    val projection: HomeProjectionState? = null,
    val loadFailed: Boolean = false,
    val refreshing: Boolean = false,
    val mcpFailed: Boolean = false,
    /** The tunnel is the other connection way; the one agent-connection row states whether either is up. */
    val tunnel: TunnelSettingsView? = null,
    val tunnelFailed: Boolean = false,
    /** Executions a lost instance left running, when that authority has answered. */
    val strandedExecutions: Int? = null,
    val strandedReadFailed: Boolean = false,
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
            val ended = async { tasks.list(TaskFilter.Completed, HomeProjection.ENDED_TASK_LIMIT) }
            val mcp = async { runCatching { client.mcpSettings() }.getOrNull()?.let(McpSettingsReplies::settings) }
            val tunnel = async { runCatching { client.tunnelSettings() }.getOrNull() }
            val stranded = async { runCatching { client.strandedExecutions() }.getOrNull() }
            val activeTasks = active.await() as? PublicResult.Success
            val endedTasks = ended.await() as? PublicResult.Success
            val settings = mcp.await()
            val tunnelSettings = tunnel.await()?.let(TunnelSettingsReplies::settings)
            val strandedExecutions = stranded.await()?.coerceAtLeast(0)
            mutableState.update { current ->
                val tunnelFacts = current.copy(
                    tunnel = tunnelSettings ?: current.tunnel,
                    tunnelFailed = tunnelSettings == null,
                    strandedExecutions = strandedExecutions ?: current.strandedExecutions,
                    strandedReadFailed = strandedExecutions == null,
                )
                if (activeTasks == null || endedTasks == null || settings == null) {
                    tunnelFacts.copy(refreshing = false, loadFailed = current.projection == null)
                } else {
                    tunnelFacts.copy(
                        projection = HomeProjectionState(
                            activeTaskCount = activeTasks.value.size,
                            mcp = HomeProjection.mcpRow(settings),
                            tasks = activeTasks.value + endedTasks.value,
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
    checking: Boolean,
    onCapabilityAction: (CapabilityRow) -> Unit,
    newerVersionAvailable: Boolean,
    openTask: (String) -> Unit,
    open: (HomeDestination) -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // Connections and the store change without a Home event (a tunnel finishing its first poll, a
    // daemon failing to start), so the page rereads them while it is visible.
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(lifecycleOwner) {
        lifecycleOwner.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                // A Runtime that is still starting answers nothing yet, so the first read is
                // retried quickly and the page settles as soon as the Runtime is up.
                delay(if (state.projection == null) HOME_STARTUP_POLL_MILLIS else HOME_POLL_MILLIS)
                viewModel.refresh(quiet = true)
            }
        }
    }
    // A Runtime that has not started yet reports itself unavailable, which is indistinguishable
    // from one that cannot start until this grace passes; until then Home reads as loading.
    var startupGracePassed by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        delay(HOME_STARTUP_GRACE_MILLIS)
        startupGracePassed = true
    }
    Scaffold(
        modifier = Modifier.testTag("route:Home"),
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        AppIcon(Modifier.size(32.dp))
                        Text(stringResource(R.string.app_name), modifier = Modifier.padding(start = 12.dp))
                    }
                },
                actions = { if (showsRefresh(state.projection != null, state.refreshing)) RefreshIndicator("home") },
            )
        },
    ) { padding ->
        val projection = state.projection
        Box(Modifier.fillMaxSize().padding(padding)) {
            // A daemon that refuses to start leaves Home unreadable, which is exactly when the
            // recovery has to be reachable, so the card also stands above the error and loading states.
            val runtimeReady = (clientState as? ClientState.Available)?.snapshot?.readiness == RuntimeReadiness.Ready
            // Reads fail while the App is still binding and the Runtime still starting; that is
            // loading, and only a Runtime that reports itself unavailable makes them errors.
            val starting = clientState is ClientState.Connecting || clientState is ClientState.Disconnected ||
                (clientState as? ClientState.Available)?.snapshot?.readiness == RuntimeReadiness.Initializing ||
                !startupGracePassed
            val showStranded = (state.strandedExecutions ?: 0) > 0 && !runtimeReady
            val showStrandedReadError = state.strandedReadFailed && !runtimeReady && !starting
            when (routeContent(projection != null, state.loadFailed && !starting)) {
                RouteContent.Error, RouteContent.Loading -> Column(
                    Modifier.fillMaxSize().padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    if (showStranded) {
                        StrandedExecutionsCard(state.clearingStranded, state.strandedClearError, viewModel::clearStrandedExecutions)
                    }
                    if (showStrandedReadError) {
                        StrandedReadErrorCard { viewModel.refresh() }
                    }
                    Box(Modifier.weight(1f)) {
                        if (projection == null && state.loadFailed && !starting) {
                            RouteError("home") { viewModel.refresh() }
                        } else {
                            RouteLoading("home")
                        }
                    }
                }
                RouteContent.Empty, RouteContent.Content ->
                    HomeContent(
                        requireNotNull(projection), state, clientState, attention, checking, onCapabilityAction,
                        newerVersionAvailable, open, viewModel::clearStrandedExecutions,
                        { viewModel.refresh() }, openTask,
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
    checking: Boolean,
    onCapabilityAction: (CapabilityRow) -> Unit,
    newerVersionAvailable: Boolean,
    open: (HomeDestination) -> Unit,
    clearStranded: () -> Unit,
    refreshStranded: () -> Unit,
    openTask: (String) -> Unit,
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
        item {
            StatusCard(clientState, HomeProjection.usability(agentState, attention.size, checking)) {
                open(HomeDestination.Diagnostics)
            }
        }
        // Only while the Runtime is not ready: a live Runtime settles its own executions.
        if ((state.strandedExecutions ?: 0) > 0 && snapshot?.readiness != RuntimeReadiness.Ready) {
            item { StrandedExecutionsCard(state.clearingStranded, state.strandedClearError, clearStranded) }
        }
        if (state.strandedReadFailed && snapshot?.readiness != RuntimeReadiness.Ready) {
            item { StrandedReadErrorCard(refreshStranded) }
        }
        item {
            Column {
                if (attention.isNotEmpty()) SectionTitle(R.string.home_attention)
                GroupCard {
                    attention.forEach { row -> CapabilityListItem(row, refreshing = false, colors = transparent) { onCapabilityAction(row) } }
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.capabilities_title)) },
                        // A step still being determined asks for nothing yet, which is not the same as done.
                        supportingContent = if (attention.isEmpty()) ({
                            Text(stringResource(if (checking) R.string.capabilities_checking else R.string.home_all_ready))
                        }) else null,
                        leadingContent = { RowIcon(R.drawable.ic_verified_user) },
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
                    leadingContent = { RowIcon(R.drawable.ic_smart_toy) },
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
        item(key = "tasks:title") {
            Row(verticalAlignment = Alignment.CenterVertically) {
                SectionTitle(R.string.nav_tasks)
                if (projection.activeTaskCount > 0) {
                    Badge(modifier = Modifier.padding(start = 8.dp).testTag("home:tasks:badge")) {
                        Text(HomeProjection.countLabel(projection.activeTaskCount, HomeProjection.TASK_PAGE_LIMIT))
                    }
                }
            }
        }
        if (projection.tasks.isEmpty()) {
            item(key = "tasks:empty") {
                GroupCard {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.tasks_empty)) },
                        colors = transparent,
                        modifier = Modifier.testTag("home:tasks:empty"),
                    )
                }
            }
        } else {
            item(key = "tasks") {
                GroupCard {
                    projection.tasks.forEach { task -> TaskRow(task, transparent) { openTask(task.taskId) } }
                }
            }
        }
    }
}

@Composable
private fun StatusCard(clientState: ClientState, usability: HomeUsability, open: () -> Unit) {
    val status = runtimeStatus(clientState, usability)
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

@Composable
private fun StrandedReadErrorCard(retry: () -> Unit) {
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
        modifier = Modifier.fillMaxWidth().testTag("home:stranded:read-error"),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(20.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(stringResource(R.string.state_error), style = MaterialTheme.typography.bodyMedium)
            TextButton(onClick = retry) { Text(stringResource(R.string.action_retry)) }
        }
    }
}

private const val HOME_POLL_MILLIS = 5_000L
/** While Home has no projection yet, the Runtime is most likely still starting. */
private const val HOME_STARTUP_POLL_MILLIS = 1_000L
private const val HOME_STARTUP_GRACE_MILLIS = 30_000L

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

private fun runtimeStatus(clientState: ClientState, usability: HomeUsability): RuntimeStatus = when (clientState) {
    is ClientState.Available -> when (clientState.snapshot.readiness) {
        RuntimeReadiness.Ready -> when (usability) {
            HomeUsability.Usable -> RuntimeStatus(R.string.home_state_usable, R.drawable.ic_status_success, StatusTone.Ready)
            HomeUsability.NoAgentConnected ->
                RuntimeStatus(R.string.home_state_no_agent, R.drawable.ic_status_unknown, StatusTone.Pending)
            HomeUsability.NeedsAttention ->
                RuntimeStatus(R.string.home_attention, R.drawable.ic_status_error, StatusTone.Pending)
            HomeUsability.Checking ->
                RuntimeStatus(R.string.capabilities_checking, R.drawable.ic_status_schedule, StatusTone.Pending)
        }
        RuntimeReadiness.Initializing -> RuntimeStatus(R.string.state_starting, R.drawable.ic_status_schedule, StatusTone.Pending)
        RuntimeReadiness.Unavailable -> RuntimeStatus(R.string.state_unavailable, R.drawable.ic_status_error, StatusTone.Failed)
    }
    ClientState.Connecting, ClientState.Disconnected ->
        RuntimeStatus(R.string.state_starting, R.drawable.ic_status_schedule, StatusTone.Pending)
    is ClientState.Unavailable -> RuntimeStatus(R.string.state_unavailable, R.drawable.ic_status_error, StatusTone.Failed)
}

/** The launcher icon, so Home carries the App's own mark beside its name. */
@Composable
private fun AppIcon(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val icon = remember { context.packageManager.getApplicationIcon(context.packageName).toBitmap().asImageBitmap() }
    Image(icon, contentDescription = null, modifier = modifier)
}
