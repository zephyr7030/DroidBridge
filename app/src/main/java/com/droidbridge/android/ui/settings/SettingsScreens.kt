package com.droidbridge.android.ui.settings

import com.droidbridge.android.ui.mcp.agentConnectionLabel
import com.droidbridge.android.ui.common.GroupCard
import com.droidbridge.android.product.home.AgentConnectionSummary
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.graphics.Color
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Arrangement
import com.droidbridge.android.product.mcp.MCP_PROTOCOL_VERSION
import android.app.LocaleConfig
import android.app.LocaleManager
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.LocaleList
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.core.graphics.drawable.toBitmap
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.about.LicenseEntry
import com.droidbridge.android.product.maintenance.MaintenanceReplies
import com.droidbridge.android.product.mcp.McpSettingsReplies
import com.droidbridge.android.product.mcp.TunnelSettingsReplies
import com.droidbridge.android.product.settings.ThemePreference
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteEmpty
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.common.RowIcon
import java.io.File
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class SettingsDestination { Capabilities, AgentConnections, Diagnostics, Updates, Data, About, Welcome }

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun SettingsRoute(
    theme: ThemePreference,
    setTheme: (ThemePreference) -> Unit,
    status: SettingsStatus,
    open: (SettingsDestination) -> Unit,
) {
    var choosingTheme by rememberSaveable { mutableStateOf(false) }
    Scaffold(
        modifier = Modifier.testTag("route:Settings"),
        topBar = { TopAppBar(title = { Text(stringResource(R.string.nav_settings)) }) },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            group(R.string.settings_group_connection) {
                Link(
                    R.string.capabilities_title,
                    R.drawable.ic_verified_user,
                    "settings:capabilities",
                    when {
                        status.pendingSetup > 0 ->
                            pluralStringResource(R.plurals.capabilities_remaining, status.pendingSetup, status.pendingSetup)
                        status.checking -> stringResource(R.string.capabilities_checking)
                        else -> stringResource(R.string.home_all_ready)
                    },
                ) { open(SettingsDestination.Capabilities) }
                RowDivider()
                Link(
                    R.string.agent_connection_title,
                    R.drawable.ic_smart_toy,
                    "settings:agent",
                    status.agent?.let { agentConnectionLabel(it) } ?: stringResource(R.string.capabilities_checking),
                ) { open(SettingsDestination.AgentConnections) }
            }
            group(R.string.settings_group_maintenance) {
                Link(
                    R.string.diagnostics_title,
                    R.drawable.ic_bug_report,
                    "settings:diagnostics",
                    stringResource(R.string.settings_diagnostics_summary),
                ) { open(SettingsDestination.Diagnostics) }
                RowDivider()
                Link(
                    R.string.settings_data,
                    R.drawable.ic_storage,
                    "settings:data",
                    stringResource(R.string.settings_data_summary),
                ) { open(SettingsDestination.Data) }
            }
            group(R.string.settings_group_appearance) {
                LanguageItem()
                RowDivider()
                Link(
                    R.string.settings_theme,
                    R.drawable.ic_palette,
                    "settings:theme",
                    stringResource(themeLabel(theme)),
                ) { choosingTheme = true }
            }
            group(R.string.settings_group_app) {
                // Always reachable fallback into first-launch setup, whatever state the app is in.
                Link(
                    R.string.settings_open_welcome,
                    R.drawable.ic_replay,
                    "settings:welcome",
                    stringResource(R.string.settings_welcome_summary),
                ) { open(SettingsDestination.Welcome) }
                RowDivider()
                Link(
                    R.string.settings_about,
                    R.drawable.ic_info,
                    "settings:about",
                    stringResource(R.string.settings_about_summary),
                ) { open(SettingsDestination.About) }
                RowDivider()
                Link(
                    R.string.settings_updates,
                    R.drawable.ic_system_update,
                    "settings:updates",
                    if (status.newerVersionAvailable) {
                        stringResource(R.string.settings_updates_available)
                    } else {
                        stringResource(R.string.settings_updates_current, status.versionName)
                    },
                ) { open(SettingsDestination.Updates) }
            }
        }
    }
    if (choosingTheme) {
        ChoiceDialog(
            title = R.string.settings_theme,
            tag = "settings:theme",
            options = ThemePreference.entries,
            selected = theme,
            optionTag = ThemePreference::wireValue,
            label = { stringResource(themeLabel(it)) },
            onSelect = setTheme,
            onDismiss = { choosingTheme = false },
        )
    }
}

/**
 * The language list is the build-generated locale config, so adding a `values-<locale>` resource
 * folder adds a choice. The system stores the per-app locale; an empty list follows the system.
 */
@Composable
private fun LanguageItem() {
    val context = LocalContext.current
    val localeManager = remember { context.getSystemService(LocaleManager::class.java) }
    val supported = remember {
        LocaleConfig(context).takeIf { it.status == LocaleConfig.STATUS_SUCCESS }?.supportedLocales
    }
    var current by remember { mutableStateOf(localeManager.applicationLocales.takeUnless { it.isEmpty }?.get(0)) }
    var choosing by rememberSaveable { mutableStateOf(false) }
    val systemLabel = stringResource(R.string.language_system)
    Link(
        R.string.settings_language,
        R.drawable.ic_language,
        "settings:language",
        when {
            supported == null -> stringResource(R.string.state_error)
            else -> current?.let(::languageName) ?: systemLabel
        },
        enabled = supported != null,
    ) { choosing = true }
    if (choosing && supported != null) {
        ChoiceDialog(
            title = R.string.settings_language,
            tag = "settings:language",
            options = listOf<Locale?>(null) + List(supported.size()) { supported[it] },
            selected = current,
            optionTag = { it?.toLanguageTag() ?: "system" },
            label = { locale -> locale?.let(::languageName) ?: systemLabel },
            onSelect = { locale ->
                localeManager.applicationLocales = locale?.let { LocaleList(it) } ?: LocaleList.getEmptyLocaleList()
                current = localeManager.applicationLocales.takeUnless { it.isEmpty }?.get(0)
            },
            onDismiss = { choosing = false },
        )
    }
}

/** A language is always named in itself, so it stays recognizable whichever language is active. */
private fun languageName(locale: Locale): String =
    locale.getDisplayName(locale).replaceFirstChar { it.titlecase(locale) }

@Composable
private fun <T> ChoiceDialog(
    @StringRes title: Int,
    tag: String,
    options: List<T>,
    selected: T,
    optionTag: (T) -> String,
    label: @Composable (T) -> String,
    onSelect: (T) -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(title)) },
        text = {
            Column {
                options.forEach { value ->
                    Row(
                        Modifier.fillMaxWidth().heightIn(min = 56.dp)
                            .selectable(selected = selected == value, role = Role.RadioButton) {
                                onDismiss()
                                onSelect(value)
                            }
                            .testTag("$tag:${optionTag(value)}"),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = selected == value, onClick = null)
                        Text(label(value), Modifier.padding(start = 16.dp))
                    }
                }
            }
        },
        confirmButton = {},
        dismissButton = {
            TextButton(onClick = onDismiss, modifier = Modifier.testTag("$tag:cancel")) {
                Text(stringResource(R.string.action_cancel))
            }
        },
    )
}

/** What the Settings rows report live: the same facts Home shows, read once by the Main entry. */
data class SettingsStatus(
    val pendingSetup: Int,
    val checking: Boolean,
    val agent: AgentConnectionSummary?,
    val versionName: String,
    val newerVersionAvailable: Boolean,
)

private fun LazyListScope.group(@StringRes title: Int, rows: @Composable () -> Unit) {
    item(key = title) {
        Column {
            Text(
                stringResource(title),
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
            )
            GroupCard { Column { rows() } }
        }
    }
}

/** Every row has a title, one line of state and a chevron, so all rows are the same height. */
@Composable
private fun Link(
    @StringRes title: Int,
    @DrawableRes icon: Int,
    tag: String,
    summary: String,
    enabled: Boolean = true,
    action: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        supportingContent = { Text(summary, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        leadingContent = { RowIcon(icon) },
        trailingContent = { RowIcon(R.drawable.ic_chevron_right) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier.clickable(enabled = enabled, onClick = action).testTag(tag),
    )
}

@Composable
private fun RowDivider() {
    HorizontalDivider(
        modifier = Modifier.padding(start = 56.dp),
        color = MaterialTheme.colorScheme.outlineVariant,
    )
}

@StringRes
private fun themeLabel(theme: ThemePreference): Int = when (theme) {
    ThemePreference.System -> R.string.theme_system
    ThemePreference.Light -> R.string.theme_light
    ThemePreference.Dark -> R.string.theme_dark
}

enum class DataAction(
    @StringRes val title: Int,
    @DrawableRes val icon: Int,
    @StringRes val dialogTitle: Int,
    @StringRes val dialogBody: Int,
    val tag: String,
) {
    ResetMcp(R.string.data_reset_mcp_credentials, R.drawable.ic_key_off, R.string.dialog_reset_mcp_title, R.string.dialog_reset_mcp_body, "reset_mcp_credentials"),
    ClearChatGpt(R.string.data_clear_chatgpt_credentials, R.drawable.ic_logout, R.string.dialog_clear_chatgpt_title, R.string.dialog_clear_chatgpt_body, "clear_chatgpt_credentials"),
    ResetRuntime(R.string.data_reset_runtime_data, R.drawable.ic_restart, R.string.dialog_reset_runtime_title, R.string.dialog_reset_runtime_body, "reset_runtime_data"),
    DeleteUpdates(R.string.data_delete_downloaded_updates, R.drawable.ic_delete_sweep, R.string.dialog_delete_updates_title, R.string.dialog_delete_updates_body, "delete_downloaded_updates"),
}

data class DataUiState(val confirming: DataAction? = null, val running: DataAction? = null, val failed: Boolean = false)

/** Data actions: each succeeds silently and fails only through the common error state. */
class DataViewModel(
    private val client: DroidBridgeClient,
    private val updateCache: File,
) : ViewModel() {
    private val mutableState = MutableStateFlow(DataUiState())
    val state: StateFlow<DataUiState> = mutableState.asStateFlow()

    fun request(action: DataAction) = mutableState.update { it.copy(confirming = action, failed = false) }

    fun dismiss() = mutableState.update { it.copy(confirming = null) }

    fun confirm() {
        val action = mutableState.value.confirming ?: return
        mutableState.value = DataUiState(running = action)
        viewModelScope.launch {
            val succeeded = when (action) {
                DataAction.ResetMcp -> runCatching { client.rotateMcpToken() }.getOrNull()
                    ?.let(McpSettingsReplies::settings) != null
                DataAction.ClearChatGpt -> runCatching { client.clearTunnel() }.getOrNull()
                    ?.let(TunnelSettingsReplies::settings)?.configured == false
                DataAction.ResetRuntime -> runCatching { client.resetRuntimeData() }.getOrNull()
                    ?.let(MaintenanceReplies::resetSucceeded) == true
                DataAction.DeleteUpdates -> withContext(Dispatchers.IO) {
                    runCatching { updateCache.deleteRecursively() && !updateCache.exists() }.getOrDefault(false)
                }
            }
            mutableState.value = DataUiState(failed = !succeeded)
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun DataRoute(viewModel: DataViewModel, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    Scaffold(
        modifier = Modifier.testTag("route:Data"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings_data)) },
                navigationIcon = { BackButton("route:Data", R.string.settings_data, back) },
                actions = { if (state.running != null) RefreshIndicator("data") },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            if (state.failed) item { RouteError("data", retry = null) }
            items(DataAction.entries) { action ->
                ListItem(
                    headlineContent = { Text(stringResource(action.title)) },
                    leadingContent = { RowIcon(action.icon) },
                    modifier = Modifier
                        .clickable(enabled = state.running == null) { viewModel.request(action) }
                        .testTag("data:${action.tag}"),
                )
            }
        }
    }
    state.confirming?.let { action ->
        AlertDialog(
            onDismissRequest = viewModel::dismiss,
            title = { Text(stringResource(action.dialogTitle)) },
            text = { Text(stringResource(action.dialogBody)) },
            confirmButton = {
                TextButton(onClick = viewModel::confirm, modifier = Modifier.testTag("data:${action.tag}:confirm")) {
                    Text(stringResource(R.string.action_confirm))
                }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismiss, modifier = Modifier.testTag("data:${action.tag}:cancel")) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AboutRoute(
    repositoryUrl: String?,
    openLicenses: () -> Unit,
    back: () -> Unit,
) {
    val context = LocalContext.current
    val packageInfo = remember { context.packageManager.getPackageInfo(context.packageName, 0) }
    val icon = remember { context.packageManager.getApplicationIcon(context.packageName).toBitmap().asImageBitmap() }
    Scaffold(
        modifier = Modifier.testTag("route:About"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings_about)) },
                navigationIcon = { BackButton("route:About", R.string.settings_about, back) },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.app_name), style = MaterialTheme.typography.titleLarge) },
                    supportingContent = { Text("${packageInfo.versionName} (${packageInfo.longVersionCode})") },
                    leadingContent = { Image(icon, contentDescription = null, modifier = Modifier.size(56.dp)) },
                    modifier = Modifier.testTag("about:app"),
                )
            }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.about_android_support)) },
                    supportingContent = {
                        Text("${stringResource(R.string.about_abi)}  ${Build.SUPPORTED_ABIS.firstOrNull().orEmpty()}")
                    },
                    leadingContent = { RowIcon(R.drawable.ic_android) },
                    modifier = Modifier.testTag("about:platform"),
                )
            }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.about_mcp_protocol_version)) },
                    supportingContent = { Text(MCP_PROTOCOL_VERSION) },
                    leadingContent = { RowIcon(R.drawable.ic_tag) },
                    modifier = Modifier.testTag("about:mcp_protocol"),
                )
            }
            item {
                ListItem(
                    headlineContent = { Text("${stringResource(R.string.mcp_protocol_version)}  $PROTOCOL_VERSION") },
                    supportingContent = { Text("${stringResource(R.string.diag_store_schema)}  $STORE_SCHEMA_VERSION") },
                    leadingContent = { RowIcon(R.drawable.ic_tag) },
                    modifier = Modifier.testTag("about:versions"),
                )
            }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.about_licenses)) },
                    leadingContent = { RowIcon(R.drawable.ic_description) },
                    modifier = Modifier.clickable(onClick = openLicenses).testTag("about:licenses"),
                )
            }
            repositoryUrl?.let { url ->
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.about_repository)) },
                        supportingContent = { Text(url) },
                        leadingContent = { RowIcon(R.drawable.ic_code) },
                        modifier = Modifier
                            .clickable { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
                            .testTag("about:repository"),
                    )
                }
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun LicensesRoute(entries: List<LicenseEntry>, notices: () -> String, back: () -> Unit) {
    var showingNotices by rememberSaveable { mutableStateOf(false) }
    Scaffold(
        modifier = Modifier.testTag("route:Licenses"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.licenses_title)) },
                navigationIcon = { BackButton("route:Licenses", R.string.licenses_title, back) },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.about_third_party_notices)) },
                    leadingContent = { Icon(painterResource(R.drawable.ic_description), contentDescription = null) },
                    modifier = Modifier.clickable { showingNotices = true }.testTag("licenses:third_party_notices"),
                )
            }
            if (entries.isEmpty()) {
                item { RouteEmpty("licenses", R.string.licenses_empty) }
            } else {
                items(entries, key = { "${it.name}@${it.version}" }) { entry ->
                    ListItem(
                        headlineContent = { Text(entry.name) },
                        supportingContent = { Text(entry.supportingText) },
                        modifier = Modifier.testTag("licenses:${entry.name}"),
                    )
                }
            }
        }
    }
    if (showingNotices) {
        AlertDialog(
            onDismissRequest = { showingNotices = false },
            title = { Text(stringResource(R.string.about_third_party_notices)) },
            text = {
                Box(Modifier.verticalScroll(rememberScrollState()).testTag("licenses:third_party_notices:text")) {
                    Text(notices())
                }
            },
            confirmButton = {
                TextButton(onClick = { showingNotices = false }, modifier = Modifier.testTag("licenses:third_party_notices:confirm")) {
                    Text(stringResource(R.string.action_confirm))
                }
            },
        )
    }
}

@Composable
internal fun BackButton(tag: String, @StringRes description: Int, back: () -> Unit) {
    IconButton(onClick = back, modifier = Modifier.testTag("$tag:back")) {
        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(description))
    }
}

private const val PROTOCOL_VERSION = "1"
private const val STORE_SCHEMA_VERSION = "1"
