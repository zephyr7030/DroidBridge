package com.droidbridge.android.ui.updates

import com.droidbridge.android.product.release.ModulePresence
import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.ui.common.RowIcon
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.update.MaintenanceRecordView
import com.droidbridge.android.product.update.MaintenanceReply
import com.droidbridge.android.product.release.ReleaseClassification
import com.droidbridge.android.product.update.UpdateCheck
import com.droidbridge.android.product.update.UpdateMaintenanceReplies
import com.droidbridge.android.product.update.UpdateMaintenanceView
import com.droidbridge.android.product.update.UpdateManager
import com.droidbridge.android.product.update.UpdateState
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.common.RouteLoading
import com.droidbridge.android.ui.settings.BackButton
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class UpdatesUiState(
    val maintenance: UpdateMaintenanceView? = null,
    val maintenanceFailed: Boolean = false,
    val busy: Boolean = false,
    val actionFailed: Boolean = false,
)

/** Presents UpdateManager (default process) and the Runtime-owned maintenance record together. */
class UpdatesViewModel(
    private val client: DroidBridgeClient,
    private val updates: UpdateManager,
    private val cacheRoot: File,
) : ViewModel() {
    private val mutableState = MutableStateFlow(UpdatesUiState())
    val state: StateFlow<UpdatesUiState> = mutableState.asStateFlow()
    val updateState: StateFlow<UpdateState> = updates.state

    fun refresh() {
        viewModelScope.launch {
            val view = runCatching { client.updateMaintenance() }.getOrNull()?.let(UpdateMaintenanceReplies::state)
            mutableState.update { it.copy(maintenance = view ?: it.maintenance, maintenanceFailed = view == null) }
            withContext(Dispatchers.IO) { updates.cleanup(referenced(view?.record)) }
        }
    }

    fun check() {
        val module = mutableState.value.maintenance?.module ?: return
        viewModelScope.launch { updates.check(module) }
    }

    fun download() {
        val module = mutableState.value.maintenance?.module ?: return
        viewModelScope.launch { updates.download(moduleRequired = module != ModulePresence.Absent) }
    }

    /** Enters product-update maintenance when needed, then makes one explicit PackageInstaller attempt. */
    fun installApk() = mutate {
        val record = mutableState.value.maintenance?.record ?: begin(product = true) ?: return@mutate FAILED
        client.installUpdateApk(record.updateId)
    }

    fun installModule() = mutate {
        val record = mutableState.value.maintenance?.record ?: begin(product = false) ?: return@mutate FAILED
        client.installUpdateModule(record.updateId)
    }

    /** Exports the verified module ZIP after the module step is recorded; export never advances the phase. */
    fun exportModule(target: Uri, resolver: ContentResolver) = mutate {
        val record = mutableState.value.maintenance?.record ?: begin(product = false) ?: return@mutate FAILED
        val source = File(File(cacheRoot, record.targetVersion), record.moduleFile)
        val copied = withContext(Dispatchers.IO) {
            runCatching {
                checkNotNull(resolver.openOutputStream(target, "wt")).use { output -> source.inputStream().use { it.copyTo(output) } }
            }.isSuccess
        }
        if (copied) EXPORTED else FAILED
    }

    fun continueWithoutModule(record: MaintenanceRecordView) = mutate { client.continueWithoutModule(record.updateId) }

    fun cancel(record: MaintenanceRecordView) = mutate { client.cancelUpdate(record.updateId) }

    private suspend fun begin(product: Boolean): MaintenanceRecordView? {
        val checked = updates.state.value.check as? UpdateCheck.Checked ?: return null
        val reply = if (product) {
            client.beginProductUpdate(checked.manifest, checked.signature)
        } else {
            client.beginModuleRepair(checked.manifest, checked.signature)
        }
        return (UpdateMaintenanceReplies.mutation(reply) as? MaintenanceReply.Recorded)?.record
    }

    /** Runs one maintenance action; any refusal or local failure shows the common error state. */
    private fun mutate(action: suspend () -> String) {
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, actionFailed = false) }
        viewModelScope.launch {
            val reply = runCatching { action() }.getOrElse { FAILED }
            val refused = UpdateMaintenanceReplies.mutation(reply) is MaintenanceReply.Refused
            mutableState.update { it.copy(busy = false, actionFailed = refused) }
            refresh()
        }
    }

    private fun referenced(record: MaintenanceRecordView?): Set<File> {
        record ?: return emptySet()
        val directory = File(cacheRoot, record.targetVersion)
        return setOf(File(directory, "droidbridge-${record.targetVersion}-arm64-v8a.apk"), File(directory, record.moduleFile))
    }

    private companion object {
        const val FAILED = """{"schema_version":1,"error":"IO_ERROR"}"""
        const val EXPORTED = """{"schema_version":1,"record":null}"""
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun UpdatesRoute(viewModel: UpdatesViewModel, apkVersion: String, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val updates by viewModel.updateState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var confirmingApkOnly by rememberSaveable { mutableStateOf(false) }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(ZIP_MIME)) { uri ->
        if (uri != null) viewModel.exportModule(uri, context.contentResolver)
    }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { viewModel.refresh() }
    val maintenance = state.maintenance
    val record = maintenance?.record
    val installApk = {
        if (context.packageManager.canRequestPackageInstalls()) {
            viewModel.installApk()
        } else {
            context.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}")))
        }
    }
    val exportModule = { name: String -> export.launch(name) }
    val installModule = { viewModel.installModule() }
    Scaffold(
        modifier = Modifier.testTag("route:Updates"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.updates_title)) },
                navigationIcon = { BackButton("route:Updates", R.string.updates_title, back) },
                actions = { if (state.busy || updates.downloading) RefreshIndicator("updates") },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.updates_current_version)) },
                    leadingContent = { RowIcon(R.drawable.ic_info) },
                    supportingContent = { Text(apkVersion) },
                    modifier = Modifier.testTag("updates:current_version"),
                )
            }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.updates_component_status)) },
                    leadingContent = { RowIcon(R.drawable.ic_extension) },
                    supportingContent = { Text(stringResource(maintenance?.module?.let(::moduleLabel) ?: R.string.state_unknown)) },
                    modifier = Modifier.testTag("updates:component_status"),
                )
            }
            item {
                Button(
                    onClick = viewModel::check,
                    enabled = maintenance != null && updates.check != UpdateCheck.Checking && updates.check != UpdateCheck.Unconfigured,
                    modifier = Modifier.fillMaxWidth().padding(16.dp).heightIn(min = 56.dp).testTag("updates:check"),
                ) { Text(stringResource(R.string.action_check_for_updates)) }
            }
            if (state.actionFailed || updates.downloadFailed || state.maintenanceFailed) {
                item { RouteError("updates:action", retry = null) }
            }
            if (record != null) {
                maintenanceActions(record, maintenance.privilegedInstall, state.busy, installApk, installModule, exportModule, viewModel::cancel) {
                    confirmingApkOnly = true
                }
            } else {
                checkRegion(updates, state.busy, installApk, exportModule, viewModel::download, viewModel::check)
            }
        }
    }
    if (confirmingApkOnly && record != null) {
        AlertDialog(
            onDismissRequest = { confirmingApkOnly = false },
            title = { Text(stringResource(R.string.dialog_apk_only_title)) },
            text = { Text(stringResource(R.string.dialog_apk_only_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmingApkOnly = false
                        viewModel.continueWithoutModule(record)
                    },
                    modifier = Modifier.testTag("updates:continue_apk_only:confirm"),
                ) { Text(stringResource(R.string.action_continue)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmingApkOnly = false }) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }
}

private fun LazyListScope.checkRegion(
    updates: UpdateState,
    busy: Boolean,
    installApk: () -> Unit,
    exportModule: (String) -> Unit,
    download: () -> Unit,
    retry: () -> Unit,
) {
    when (val check = updates.check) {
        UpdateCheck.Unconfigured -> item { Status(R.string.updates_configuration_unavailable, null, "configuration_unavailable", R.drawable.ic_status_unknown) }
        UpdateCheck.Idle -> Unit
        UpdateCheck.Checking -> item { RouteLoading("updates") }
        UpdateCheck.Failed -> item { RouteError("updates", retry) }
        is UpdateCheck.Checked -> when (val classification = check.classification) {
            ReleaseClassification.UpToDate -> item { Status(R.string.updates_up_to_date, null, "up_to_date", R.drawable.ic_status_success) }
            is ReleaseClassification.ProductUpdate -> {
                item { Status(R.string.updates_available, classification.manifest.version, "available", R.drawable.ic_system_update) }
                val apk = updates.downloads?.apk
                item {
                    if (apk == null) {
                        Action(R.string.action_download_update, "download_update", !updates.downloading, download)
                    } else {
                        Action(R.string.updates_install_apk, "install_apk", !busy, installApk)
                    }
                }
            }
            is ReleaseClassification.ModuleRepair -> {
                item { Status(R.string.updates_module_repair_required, classification.manifest.version, "module_repair_required", R.drawable.ic_status_error) }
                val module = updates.downloads?.module
                item {
                    if (module == null) {
                        Action(R.string.action_download_module, "download_module", !updates.downloading, download)
                    } else {
                        Action(R.string.updates_export_module, "export_module", !busy) { exportModule(module.name) }
                    }
                }
            }
        }
    }
}

private fun LazyListScope.maintenanceActions(
    record: MaintenanceRecordView,
    privilegedInstall: Boolean,
    busy: Boolean,
    installApk: () -> Unit,
    installModule: () -> Unit,
    exportModule: (String) -> Unit,
    cancel: (MaintenanceRecordView) -> Unit,
    continueApkOnly: () -> Unit,
) {
    item { Status(if (record.productUpdate) R.string.updates_available else R.string.updates_module_repair_required, record.targetVersion, "maintenance", if (record.productUpdate) R.drawable.ic_system_update else R.drawable.ic_status_error) }
    when (record.phase) {
        "prepared" -> item { Action(R.string.updates_install_apk, "install_apk", !busy, installApk) }
        "apk_installing", "apk_installed" -> item { RouteLoading("updates:installing") }
        "module_installing" -> item {
            if (record.nativeAttemptActive) RouteLoading("updates:installing") else Status(R.string.updates_reboot_required, null, "reboot_required", R.drawable.ic_restart)
        }
        "module_pending" -> {
            item {
                // A compatible daemon installs the verified module directly; otherwise the ZIP is exported.
                if (privilegedInstall) {
                    Action(R.string.action_install_module, "install_module", !busy, installModule)
                } else {
                    Action(R.string.updates_export_module, "export_module", !busy) { exportModule(record.moduleFile) }
                }
            }
            item {
                OutlinedButton(
                    onClick = continueApkOnly,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).heightIn(min = 56.dp).testTag("updates:continue_apk_only"),
                ) { Text(stringResource(R.string.updates_continue_apk_only)) }
            }
        }
    }
    if (record.phase == "module_installing" && !record.nativeAttemptActive) {
        item {
            OutlinedButton(
                onClick = continueApkOnly,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).heightIn(min = 56.dp).testTag("updates:continue_apk_only"),
            ) { Text(stringResource(R.string.updates_continue_apk_only)) }
        }
    }
    val settledModuleStep = record.phase == "module_pending" || (record.phase == "module_installing" && !record.nativeAttemptActive)
    val cancellable = !record.nativeAttemptActive &&
        (record.phase == "prepared" || record.phase == "apk_installing" || (!record.productUpdate && settledModuleStep))
    if (cancellable) {
        item {
            TextButton(
                onClick = { cancel(record) },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).heightIn(min = 48.dp).testTag("updates:cancel"),
            ) { Text(stringResource(R.string.action_cancel)) }
        }
    }
}

@Composable
private fun Status(@StringRes text: Int, version: String?, tag: String, @DrawableRes icon: Int) {
    ListItem(
        headlineContent = { Text(stringResource(text)) },
        leadingContent = { RowIcon(icon) },
        supportingContent = version?.let { { Text(it) } },
        modifier = Modifier.testTag("updates:$tag"),
    )
}

@Composable
private fun Action(@StringRes text: Int, tag: String, enabled: Boolean, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp).heightIn(min = 56.dp).testTag("updates:$tag"),
    ) { Text(stringResource(text)) }
}

@StringRes
private fun moduleLabel(module: ModulePresence): Int = when (module) {
    ModulePresence.Compatible -> R.string.state_ready
    ModulePresence.Absent -> R.string.state_not_installed
    ModulePresence.Mismatched -> R.string.state_update_required
    ModulePresence.Excluded -> R.string.updates_module_excluded
}

private const val ZIP_MIME = "application/zip"
