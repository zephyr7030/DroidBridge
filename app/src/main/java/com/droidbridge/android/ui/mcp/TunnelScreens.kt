package com.droidbridge.android.ui.mcp

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.ui.common.RowIcon
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.mcp.CHATGPT_APPS_SETTINGS_URL
import com.droidbridge.android.product.mcp.CHATGPT_CREATE_PLUGIN_URL
import com.droidbridge.android.product.mcp.OPENAI_API_KEYS_URL
import com.droidbridge.android.product.mcp.OPENAI_TUNNELS_URL
import com.droidbridge.android.product.mcp.TUNNEL_PLUGIN_NAME
import com.droidbridge.android.product.mcp.TunnelRuntimeState
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsError
import com.droidbridge.android.product.mcp.TunnelSettingsView
import com.droidbridge.android.product.mcp.isTunnelConfigurationInputValid
import com.droidbridge.android.product.mcp.isTunnelPluginCreationReady
import com.droidbridge.android.product.mcp.isTunnelStepActionEnabled
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

data class TunnelUiState(
    val settings: TunnelSettingsView? = null,
    val loading: Boolean = true,
    val failed: Boolean = false,
    val error: TunnelSettingsError? = null,
)

class TunnelViewModel(private val client: DroidBridgeClient) : ViewModel() {
    private val mutableState = MutableStateFlow(TunnelUiState())
    val state: StateFlow<TunnelUiState> = mutableState.asStateFlow()

    init {
        viewModelScope.launch {
            client.state.collect { connection ->
                if (connection is ClientState.Available && mutableState.value.settings == null) refresh()
            }
        }
    }

    fun refresh(background: Boolean = false) {
        if (!background) mutableState.update { it.copy(loading = true, failed = false, error = null) }
        viewModelScope.launch {
            val reply = runCatching { client.tunnelSettings() }.getOrNull()
            val settings = reply?.let(TunnelSettingsReplies::settings)
            mutableState.update { current ->
                current.copy(
                    settings = settings ?: current.settings,
                    loading = if (background) current.loading else false,
                    failed = if (background) current.failed else settings == null,
                    error = if (settings == null) reply?.let(TunnelSettingsReplies::error) else null,
                )
            }
        }
    }

    fun configureAndEnable(tunnelId: String, apiKey: String, complete: (Boolean) -> Unit) {
        mutableState.update { it.copy(loading = true, failed = false, error = null) }
        viewModelScope.launch {
            val reply = runCatching { client.configureTunnel(tunnelId, apiKey) }.getOrNull()
            val configured = reply?.let(TunnelSettingsReplies::settings)
            val connected = when {
                configured == null -> null
                configured.enabled -> configured
                else -> runCatching { client.setTunnelEnabled(true) }
                    .getOrNull()?.let(TunnelSettingsReplies::settings)
            }
            mutableState.update { current ->
                TunnelUiState(
                    settings = connected ?: configured ?: current.settings,
                    loading = false,
                    failed = connected == null,
                    error = if (configured == null) reply?.let(TunnelSettingsReplies::error) else null,
                )
            }
            complete(configured != null)
        }
    }

    fun setEnabled(enabled: Boolean) = mutate { client.setTunnelEnabled(enabled) }

    /** A notification decision changes a grant the Runtime reports, so it is read again. */
    fun notificationsDecided() = client.recheck()

    fun clear(complete: (Boolean) -> Unit = {}) = mutate(complete = complete) { client.clearTunnel() }

    private fun mutate(
        background: Boolean = false,
        complete: (Boolean) -> Unit = {},
        call: suspend () -> String,
    ) {
        if (!background) mutableState.update { it.copy(loading = true, failed = false, error = null) }
        viewModelScope.launch {
            val reply = runCatching { call() }.getOrNull()
            val settings = reply?.let(TunnelSettingsReplies::settings)
            mutableState.update { current ->
                TunnelUiState(
                    settings = settings ?: current.settings,
                    loading = if (background) current.loading else false,
                    failed = settings == null,
                    error = if (settings == null) reply?.let(TunnelSettingsReplies::error) else null,
                )
            }
            complete(settings != null)
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun TunnelRoute(
    viewModel: TunnelViewModel,
    runtimeReady: Boolean,
    notificationsUnavailable: Boolean,
    shouldRequestNotifications: () -> Boolean,
    done: () -> Unit,
    finishSetup: (() -> Unit)?,
    back: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var tunnelId by remember { mutableStateOf("") }
    var apiKey by remember { mutableStateOf("") }
    var editing by remember { mutableStateOf(false) }
    var confirmClear by remember { mutableStateOf(false) }
    var pendingConnect by remember { mutableStateOf<Pair<String, String>?>(null) }
    var enforceFirstSetupOrder by rememberSaveable { mutableStateOf<Boolean?>(null) }
    var openedPluginSettings by rememberSaveable { mutableStateOf(false) }
    val connect: (String, String) -> Unit = { id, key ->
        viewModel.configureAndEnable(id, key) { success ->
            if (success) {
                apiKey = ""
                editing = false
            }
        }
    }
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        viewModel.notificationsDecided()
        pendingConnect?.let { (id, key) -> connect(id, key) } ?: viewModel.setEnabled(true)
        pendingConnect = null
    }
    val settings = state.settings
    val showForm = settings?.configured != true || editing
    val inputValid = isTunnelConfigurationInputValid(tunnelId, apiKey)
    val creationReady = settings?.let {
        isTunnelPluginCreationReady(runtimeReady, it.state)
    } == true
    val orderedSetup = enforceFirstSetupOrder != false
    val pluginSettingsEnabled = isTunnelStepActionEnabled(orderedSetup, creationReady)
    val createPluginEnabled = isTunnelStepActionEnabled(orderedSetup, creationReady && openedPluginSettings)
    val doneEnabled = isTunnelStepActionEnabled(orderedSetup, settings?.lastCallEpochMs != null)

    Scaffold(
        modifier = Modifier.testTag("route:TunnelSetup"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.tunnel_title)) },
                navigationIcon = {
                    IconButton(onClick = back, modifier = Modifier.testTag("route:TunnelSetup:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.tunnel_title))
                    }
                },
            )
        },
        bottomBar = {
            // First setup ends here whether or not ChatGPT is configured yet; it can be done later.
            Button(
                onClick = finishSetup ?: done,
                enabled = finishSetup != null || doneEnabled,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("tunnel:done"),
            ) {
                Text(stringResource(if (finishSetup != null) R.string.action_finish_setup else R.string.action_done))
            }
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(padding),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.tunnel_intro_title)) },
                    leadingContent = { RowIcon(R.drawable.ic_info) },
                    supportingContent = { Text(stringResource(R.string.tunnel_intro_body)) },
                    modifier = Modifier.testTag("tunnel:intro"),
                )
            }
            if (state.failed || settings == null) {
                item {
                    ListItem(
                        headlineContent = {
                            Text(stringResource(if (state.failed) R.string.state_error else R.string.state_loading))
                        },
                        supportingContent = state.error?.let { error ->
                            ({ Text(stringResource(tunnelSettingsError(error))) })
                        },
                        trailingContent = if (state.failed) ({
                            Button(onClick = { viewModel.refresh() }, modifier = Modifier.testTag("tunnel:retry")) {
                                Text(stringResource(R.string.action_retry))
                            }
                        }) else null,
                        modifier = Modifier.testTag("tunnel:load_state"),
                    )
                }
            }
            if (showForm) {
                item {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(stringResource(R.string.tunnel_credentials_title), style = MaterialTheme.typography.titleMedium)
                        ExternalLinkRow(
                            R.string.tunnel_open_tunnels,
                            OPENAI_TUNNELS_URL,
                            "tunnel:open_tunnels",
                            context::openWebPage,
                            context::copyText,
                        )
                        ExternalLinkRow(
                            R.string.tunnel_open_api_keys,
                            OPENAI_API_KEYS_URL,
                            "tunnel:open_api_keys",
                            context::openWebPage,
                            context::copyText,
                        )
                        OutlinedTextField(
                            value = tunnelId,
                            onValueChange = { tunnelId = it },
                            singleLine = true,
                            label = { Text(stringResource(R.string.tunnel_id)) },
                            modifier = Modifier.fillMaxWidth().testTag("tunnel:id"),
                        )
                        TextButton(
                            onClick = { context.clipboardText()?.let { tunnelId = it } },
                            modifier = Modifier.testTag("tunnel:paste_id"),
                        ) { Text(stringResource(R.string.action_paste_clipboard)) }
                        OutlinedTextField(
                            value = apiKey,
                            onValueChange = { apiKey = it },
                            singleLine = true,
                            label = { Text(stringResource(R.string.tunnel_api_key)) },
                            supportingText = { Text(stringResource(R.string.tunnel_api_key_storage)) },
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                            modifier = Modifier.fillMaxWidth().testTag("tunnel:api_key"),
                        )
                        TextButton(
                            onClick = { context.clipboardText()?.let { apiKey = it } },
                            modifier = Modifier.testTag("tunnel:paste_api_key"),
                        ) { Text(stringResource(R.string.action_paste_clipboard)) }
                        Button(
                            onClick = {
                                if (shouldRequestNotifications()) {
                                    pendingConnect = tunnelId to apiKey
                                    notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                                } else {
                                    connect(tunnelId, apiKey)
                                }
                            },
                            enabled = inputValid && !state.loading,
                            modifier = Modifier.fillMaxWidth().testTag("tunnel:connect"),
                        ) { Text(stringResource(R.string.tunnel_save_connect)) }
                        if (editing) {
                            TextButton(
                                onClick = {
                                    apiKey = ""
                                    editing = false
                                },
                                modifier = Modifier.testTag("tunnel:replace_cancel"),
                            ) { Text(stringResource(R.string.action_cancel)) }
                        }
                    }
                }
            }
            if (settings?.configured == true) {
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.tunnel_enable)) },
                        leadingContent = { RowIcon(R.drawable.ic_power) },
                        supportingContent = { Text(settings.tunnelId.orEmpty()) },
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
                                modifier = Modifier.testTag("tunnel:enable"),
                            )
                        },
                    )
                }
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.tunnel_runtime_state)) },
                        leadingContent = { RowIcon(R.drawable.ic_monitor_heart) },
                        supportingContent = {
                            Column {
                                Text(stringResource(tunnelStateLabel(settings)))
                                tunnelReason(settings.reason)?.let { Text(stringResource(it)) }
                                settings.lastError?.let {
                                    Text(
                                        stringResource(R.string.tunnel_last_error, it),
                                        modifier = Modifier.testTag("tunnel:last_error"),
                                    )
                                }
                            }
                        },
                        trailingContent = if (settings.state == TunnelRuntimeState.Failed) ({
                            Button(
                                onClick = { viewModel.setEnabled(true) },
                                modifier = Modifier.testTag("tunnel:runtime_retry"),
                            ) { Text(stringResource(R.string.action_retry)) }
                        }) else null,
                        modifier = Modifier.testTag("tunnel:runtime_state"),
                    )
                }
                if (settings.enabled && notificationsUnavailable) {
                    item {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.warning_notification_visibility_limited)) },
                            leadingContent = { RowIcon(R.drawable.ic_notifications_off) },
                            modifier = Modifier.testTag("tunnel:notification_warning"),
                        )
                    }
                }
                item {
                    Row(Modifier.padding(horizontal = 16.dp)) {
                        TextButton(
                            onClick = {
                                tunnelId = settings.tunnelId.orEmpty()
                                editing = true
                            },
                            modifier = Modifier.testTag("tunnel:replace"),
                        ) { Text(stringResource(R.string.tunnel_replace)) }
                        TextButton(onClick = { confirmClear = true }, modifier = Modifier.testTag("tunnel:clear")) {
                            Text(stringResource(R.string.tunnel_disconnect_forget))
                        }
                    }
                }
            }
            if (settings != null) {
                item {
                    Text(
                        stringResource(R.string.tunnel_connection_status),
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.padding(start = 16.dp, top = 16.dp, end = 16.dp),
                    )
                }
                item {
                    TunnelStatusRow(
                        R.string.tunnel_status_runtime,
                        R.drawable.ic_monitor_heart,
                        if (runtimeReady) R.string.state_ready else R.string.state_starting,
                        "tunnel:status:runtime",
                    )
                }
                item {
                    TunnelStatusRow(
                        R.string.tunnel_status_openai,
                        R.drawable.ic_cloud,
                        tunnelReadinessLabel(settings),
                        "tunnel:status:openai",
                        tunnelReason(settings.reason),
                    )
                }
                item {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(
                            stringResource(R.string.tunnel_create_plugin_title),
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(stringResource(R.string.tunnel_create_plugin_instructions))
                        OutlinedButton(
                            onClick = {
                                openedPluginSettings = true
                                context.openWebPage(CHATGPT_APPS_SETTINGS_URL)
                            },
                            enabled = pluginSettingsEnabled,
                            modifier = Modifier.fillMaxWidth().testTag("tunnel:open_plugin_settings"),
                        ) { Text(stringResource(R.string.tunnel_open_plugin_settings)) }
                        Button(
                            onClick = { context.openWebPage(CHATGPT_CREATE_PLUGIN_URL) },
                            enabled = createPluginEnabled,
                            modifier = Modifier.fillMaxWidth().testTag("tunnel:open_create_plugin"),
                        ) { Text(stringResource(R.string.tunnel_open_create_plugin)) }
                    }
                }
                if (settings.configured) {
                    item {
                        Text(
                            stringResource(R.string.tunnel_plugin_information),
                            style = MaterialTheme.typography.titleMedium,
                            modifier = Modifier.padding(start = 16.dp, top = 16.dp, end = 16.dp),
                        )
                    }
                    item {
                        CopyValueRow(
                            title = R.string.tunnel_plugin_name,
                            icon = R.drawable.ic_extension,
                            value = TUNNEL_PLUGIN_NAME,
                            tag = "tunnel:copy_plugin_name",
                        ) { context.copyText(TUNNEL_PLUGIN_NAME) }
                    }
                    item {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.tunnel_id)) },
                            leadingContent = { RowIcon(R.drawable.ic_badge) },
                            supportingContent = { Text(settings.tunnelId.orEmpty()) },
                            modifier = Modifier.testTag("tunnel:plugin_tunnel_id"),
                        )
                    }
                }
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.tunnel_first_call_title)) },
                        leadingContent = { RowIcon(R.drawable.ic_chat) },
                        supportingContent = {
                            Text(
                                settings.lastCallEpochMs?.let(::formatLastCall)
                                    ?: stringResource(R.string.tunnel_first_call_waiting),
                            )
                        },
                        modifier = Modifier.testTag("tunnel:first_call"),
                    )
                }
            }
            settings?.let { current ->
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.mcp_protocol_version)) },
                        leadingContent = { RowIcon(R.drawable.ic_tag) },
                        supportingContent = { Text(current.protocolVersion) },
                        modifier = Modifier.testTag("tunnel:protocol_version"),
                    )
                }
            }
        }
    }

    if (confirmClear) {
        AlertDialog(
            onDismissRequest = { confirmClear = false },
            title = { Text(stringResource(R.string.tunnel_clear_title)) },
            text = { Text(stringResource(R.string.tunnel_clear_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmClear = false
                        viewModel.clear { success -> if (success) tunnelId = "" }
                    },
                    modifier = Modifier.testTag("tunnel:clear:confirm"),
                ) { Text(stringResource(R.string.tunnel_disconnect_forget)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmClear = false }) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }

    LaunchedEffect(Unit) { viewModel.refresh() }
    LaunchedEffect(settings) {
        if (enforceFirstSetupOrder == null && settings != null) {
            enforceFirstSetupOrder = !settings.configured
        }
    }
    LaunchedEffect(settings?.enabled) {
        while (settings?.enabled == true) {
            delay(2_000)
            viewModel.refresh(background = true)
        }
    }
}

private fun tunnelStateLabel(settings: TunnelSettingsView): Int = when (settings.state) {
    TunnelRuntimeState.Stopped -> R.string.mcp_state_off
    TunnelRuntimeState.Connecting -> R.string.tunnel_state_connecting
    TunnelRuntimeState.Running -> R.string.mcp_state_running
    TunnelRuntimeState.Failed -> R.string.task_state_failed
}

private fun tunnelReadinessLabel(settings: TunnelSettingsView): Int = when {
    !settings.configured -> R.string.tunnel_state_not_configured
    settings.state == TunnelRuntimeState.Stopped -> R.string.mcp_state_off
    settings.state == TunnelRuntimeState.Connecting -> R.string.tunnel_state_connecting
    settings.state == TunnelRuntimeState.Running -> R.string.state_ready
    else -> R.string.task_state_failed
}

private fun tunnelReason(reason: String?): Int? = when (reason) {
    "FGS_START_REJECTED" -> R.string.reason_fgs_start_rejected
    "NETWORK_MONITOR_FAILED" -> R.string.reason_tunnel_network_monitor_failed
    "CREDENTIALS_UNAVAILABLE" -> R.string.reason_tunnel_credentials_unavailable
    "TUNNEL_RUNTIME_FAILED" -> R.string.reason_tunnel_runtime_failed
    else -> null
}

private fun tunnelSettingsError(error: TunnelSettingsError): Int = when (error) {
    TunnelSettingsError.TunnelNotFound -> R.string.reason_tunnel_not_found
    TunnelSettingsError.ApiKeyInvalid -> R.string.reason_tunnel_api_key_invalid
    TunnelSettingsError.OpenAiUnavailable -> R.string.reason_tunnel_openai_unavailable
    TunnelSettingsError.InvalidConfig -> R.string.reason_tunnel_invalid_config
    TunnelSettingsError.IoError -> R.string.reason_store_unavailable
    TunnelSettingsError.NotConfigured -> R.string.reason_tunnel_not_configured
}

private fun formatLastCall(epochMs: Long): String =
    DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.MEDIUM).format(Date(epochMs))

@Composable
private fun TunnelStatusRow(
    title: Int,
    icon: Int,
    status: Int,
    tag: String,
    reason: Int? = null,
) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        leadingContent = { RowIcon(icon) },
        supportingContent = {
            Column {
                Text(stringResource(status))
                reason?.let { Text(stringResource(it)) }
            }
        },
        modifier = Modifier.testTag(tag),
    )
}

@Composable
private fun CopyValueRow(title: Int, icon: Int, value: String, tag: String, copy: () -> Unit) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        leadingContent = { RowIcon(icon) },
        supportingContent = { Text(value) },
        trailingContent = {
            TextButton(onClick = copy, modifier = Modifier.testTag(tag)) {
                Text(stringResource(R.string.action_copy))
            }
        },
    )
}

@Composable
private fun ExternalLinkRow(
    title: Int,
    url: String,
    tag: String,
    open: (String) -> Unit,
    copy: (String) -> Unit,
) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = { open(url) }, modifier = Modifier.weight(1f).testTag(tag)) {
            Text(stringResource(title))
        }
        TextButton(onClick = { copy(url) }, modifier = Modifier.testTag("$tag:copy")) {
            Text(stringResource(R.string.action_copy_link))
        }
    }
}

private fun android.content.Context.openWebPage(url: String) {
    startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
}

private fun android.content.Context.copyText(value: String) {
    getSystemService(ClipboardManager::class.java)
        .setPrimaryClip(ClipData.newPlainText(TUNNEL_PLUGIN_NAME, value))
}

private fun android.content.Context.clipboardText(): String? =
    getSystemService(ClipboardManager::class.java)
        .primaryClip
        ?.getItemAt(0)
        ?.coerceToText(this)
        ?.toString()
