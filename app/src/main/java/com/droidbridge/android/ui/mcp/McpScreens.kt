package com.droidbridge.android.ui.mcp

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.provider.Settings as AndroidSettings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.ui.common.RowIcon
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.home.HomeProjection
import com.droidbridge.android.product.mcp.McpListenerState
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.product.mcp.McpSettingsView
import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsView
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class McpUiState(
    val settings: McpSettingsView? = null,
    val tunnelSettings: TunnelSettingsView? = null,
    /** A tunnel read that failed is not the same fact as a tunnel that is not configured. */
    val tunnelFailed: Boolean = false,
    val loading: Boolean = true,
    val failed: Boolean = false,
    /** Present only after an explicit Reveal; dropped with this route's ViewModel. */
    val revealedToken: String? = null,
)

class McpViewModel(private val client: DroidBridgeClient) : ViewModel() {
    private val mutableState = MutableStateFlow(McpUiState())
    val state: StateFlow<McpUiState> = mutableState.asStateFlow()

    init {
        viewModelScope.launch {
            client.state.collect { connection ->
                if (connection is ClientState.Available && mutableState.value.settings == null) refresh()
            }
        }
    }

    fun refresh() {
        mutableState.update { it.copy(loading = true) }
        viewModelScope.launch {
            val settings = runCatching { client.mcpSettings() }.getOrNull()?.let(McpSettingsReplies::settings)
            val tunnelSettings = runCatching { client.tunnelSettings() }.getOrNull()?.let(TunnelSettingsReplies::settings)
            mutableState.update { current ->
                current.copy(
                    settings = settings ?: current.settings,
                    tunnelSettings = tunnelSettings ?: current.tunnelSettings,
                    tunnelFailed = tunnelSettings == null,
                    loading = false,
                    failed = settings == null,
                )
            }
        }
    }

    fun setEnabled(enabled: Boolean) = mutate { client.setMcpEnabled(enabled) }

    /** A notification decision changes a grant the Runtime reports, so it is read again. */
    fun notificationsDecided() = client.recheck()

    fun rotate() = mutate(remask = true) { client.rotateMcpToken() }

    fun reveal() {
        viewModelScope.launch {
            val token = runCatching { client.revealMcpToken() }.getOrNull()?.let(McpSettingsReplies::token)
            mutableState.update { if (token == null) it.copy(failed = true) else it.copy(revealedToken = token) }
        }
    }

    /** Reads the token for one Copy action without revealing it on screen. */
    suspend fun tokenForCopy(): String? =
        runCatching { client.revealMcpToken() }.getOrNull()?.let(McpSettingsReplies::token)

    fun copyFailed() = mutableState.update { it.copy(failed = true) }

    private fun mutate(remask: Boolean = false, call: suspend () -> String) {
        mutableState.update { it.copy(loading = true) }
        viewModelScope.launch {
            val settings = runCatching { call() }.getOrNull()?.let(McpSettingsReplies::settings)
            mutableState.update { current ->
                McpUiState(
                    settings = settings ?: current.settings,
                    tunnelSettings = current.tunnelSettings,
                    tunnelFailed = current.tunnelFailed,
                    loading = false,
                    failed = settings == null,
                    revealedToken = if (remask || settings == null) null else current.revealedToken,
                )
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun McpRoute(
    viewModel: McpViewModel,
    notificationsUnavailable: Boolean,
    shouldRequestNotifications: () -> Boolean,
    finishSetup: (() -> Unit)?,
    back: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val snackbars = remember { SnackbarHostState() }
    var confirmRotate by remember { mutableStateOf(false) }
    val copiedText = stringResource(R.string.snackbar_token_copied)
    // Denial still commits enable; the warning row then explains the limited visibility.
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        viewModel.notificationsDecided()
        viewModel.setEnabled(true)
    }
    Scaffold(
        modifier = Modifier.testTag("route:MCP"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.home_mcp)) },
                navigationIcon = {
                    IconButton(onClick = back, modifier = Modifier.testTag("route:MCP:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.home_mcp))
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbars) },
        bottomBar = {
            finishSetup?.let { finish ->
                Button(
                    onClick = finish,
                    modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                        .testTag("mcp:finish_setup"),
                ) { Text(stringResource(R.string.action_finish_setup)) }
            }
        },
    ) { padding ->
        val settings = state.settings
        LazyColumn(modifier = Modifier.fillMaxSize().padding(padding)) {
            if (state.failed || settings == null) {
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(if (state.failed) R.string.state_error else R.string.state_loading)) },
                        trailingContent = if (state.failed) ({
                            Button(onClick = viewModel::refresh, modifier = Modifier.testTag("mcp:retry")) {
                                Text(stringResource(R.string.action_retry))
                            }
                        }) else null,
                        modifier = Modifier.testTag("mcp:state"),
                    )
                }
            }
            if (settings != null) {
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.mcp_enable)) },
                        leadingContent = { RowIcon(R.drawable.ic_power) },
                        trailingContent = {
                            Switch(
                                checked = settings.enabled,
                                enabled = !state.loading,
                                onCheckedChange = { enabled ->
                                    if (enabled && shouldRequestNotifications()) {
                                        notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                                    } else {
                                        viewModel.setEnabled(enabled)
                                    }
                                },
                                modifier = Modifier.testTag("mcp:enable"),
                            )
                        },
                    )
                }
                if (settings.enabled && notificationsUnavailable) {
                    item {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.warning_notification_visibility_limited)) },
                            leadingContent = { RowIcon(R.drawable.ic_notifications_off) },
                            trailingContent = {
                                TextButton(onClick = {
                                    context.startActivity(
                                        Intent(AndroidSettings.ACTION_APP_NOTIFICATION_SETTINGS)
                                            .putExtra(AndroidSettings.EXTRA_APP_PACKAGE, context.packageName),
                                    )
                                }) { Text(stringResource(R.string.action_open_settings)) }
                            },
                            modifier = Modifier.testTag("mcp:notification_warning"),
                        )
                    }
                }
                item {
                    val failedReason = listenerReason(settings)
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.mcp_runtime_state)) },
                        leadingContent = { RowIcon(R.drawable.ic_monitor_heart) },
                        supportingContent = {
                            Text(stringResource(mcpStateLabel(HomeProjection.mcpRow(settings))))
                            if (settings.enabled && settings.listener != McpListenerState.Running) {
                                Text(stringResource(failedReason ?: R.string.state_error))
                            }
                        },
                        trailingContent = if (settings.enabled && settings.listener == McpListenerState.Failed) ({
                            Button(onClick = { viewModel.setEnabled(true) }, modifier = Modifier.testTag("mcp:listener_retry")) {
                                Text(stringResource(R.string.action_retry))
                            }
                        }) else null,
                        modifier = Modifier.testTag("mcp:runtime_state"),
                    )
                }
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.mcp_endpoint)) },
                        leadingContent = { RowIcon(R.drawable.ic_link) },
                        supportingContent = { Text(settings.endpoint) },
                        modifier = Modifier.testTag("mcp:endpoint"),
                    )
                }
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.mcp_access_token)) },
                        leadingContent = { RowIcon(R.drawable.ic_key) },
                        supportingContent = { Text(state.revealedToken ?: MASKED_TOKEN, modifier = Modifier.testTag("mcp:token")) },
                    )
                    Row(modifier = Modifier.padding(horizontal = 16.dp)) {
                        TextButton(onClick = viewModel::reveal, modifier = Modifier.testTag("mcp:reveal")) {
                            Text(stringResource(R.string.action_reveal))
                        }
                        TextButton(
                            onClick = {
                                scope.launch {
                                    val token = viewModel.tokenForCopy()
                                    val copied = token != null && runCatching {
                                        context.getSystemService(ClipboardManager::class.java)
                                            .setPrimaryClip(ClipData.newPlainText(copiedText, token))
                                    }.isSuccess
                                    if (copied) snackbars.showSnackbar(copiedText) else viewModel.copyFailed()
                                }
                            },
                            modifier = Modifier.testTag("mcp:copy"),
                        ) { Text(stringResource(R.string.action_copy)) }
                        TextButton(onClick = { confirmRotate = true }, modifier = Modifier.testTag("mcp:rotate")) {
                            Text(stringResource(R.string.action_rotate))
                        }
                    }
                }
            }
        }
    }
    if (confirmRotate) {
        AlertDialog(
            onDismissRequest = { confirmRotate = false },
            title = { Text(stringResource(R.string.dialog_rotate_token_title)) },
            text = { Text(stringResource(R.string.dialog_rotate_token_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmRotate = false
                        viewModel.rotate()
                    },
                    modifier = Modifier.testTag("mcp:rotate:confirm"),
                ) { Text(stringResource(R.string.action_regenerate)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmRotate = false }) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }
    LaunchedEffect(Unit) { viewModel.refresh() }
}

private fun listenerReason(settings: McpSettingsView): Int? = when (settings.reason) {
    "MCP_LISTENER_FAILED" -> R.string.reason_mcp_listener_failed
    "FGS_START_REJECTED" -> R.string.reason_fgs_start_rejected
    else -> null
}

/** The single tunnel-state wording, shared by the agent-connection list and the tunnel page. */
internal fun tunnelStatusLabel(settings: TunnelSettingsView?): Int = when {
    settings == null || !settings.configured -> R.string.tunnel_state_not_configured
    settings.state == TunnelRuntimeState.Running -> R.string.mcp_state_running
    settings.state == TunnelRuntimeState.Connecting -> R.string.tunnel_state_connecting
    settings.state == TunnelRuntimeState.Failed -> R.string.task_state_failed
    else -> R.string.mcp_state_off
}

private const val MASKED_TOKEN = "••••••••••••"
