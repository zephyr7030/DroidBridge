package com.droidbridge.standalone.ui.updates

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.ui.R
import com.droidbridge.standalone.R as AppR
import com.droidbridge.ui.common.RowIcon
import com.droidbridge.standalone.client.DroidBridgeClient
import com.droidbridge.standalone.product.update.MaintenanceRecordView
import com.droidbridge.standalone.product.update.MaintenanceReply
import com.droidbridge.standalone.product.release.ReleaseClassification
import com.droidbridge.standalone.product.update.UpdateCheck
import com.droidbridge.standalone.product.update.UpdateMaintenanceReplies
import com.droidbridge.standalone.product.update.UpdateMaintenanceView
import com.droidbridge.standalone.product.update.UpdateManager
import com.droidbridge.standalone.product.update.UpdateState
import com.droidbridge.ui.common.RefreshIndicator
import com.droidbridge.ui.common.RouteError
import com.droidbridge.ui.common.RouteLoading
import com.droidbridge.ui.settings.BackButton
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class UpdatesUiState(
    val maintenance: UpdateMaintenanceView? = null,
    val readFailure: MaintenanceReply.Refused? = null,
    val busy: Boolean = false,
    val actionFailure: MaintenanceReply.Refused? = null,
)

class UpdatesViewModel(
    private val client: DroidBridgeClient,
    private val updates: UpdateManager,
) : ViewModel() {
    private val mutableState = MutableStateFlow(UpdatesUiState())
    val state: StateFlow<UpdatesUiState> = mutableState.asStateFlow()
    val updateState: StateFlow<UpdateState> = updates.state
    private var refreshJob: Job? = null
    private var maintenanceRevision = 0L

    fun refresh() {
        if (mutableState.value.busy) return
        maintenanceRevision++
        refreshJob?.cancel()
        refreshJob = viewModelScope.launch { refreshState() }
    }

    private suspend fun refreshState() {
        val revision = maintenanceRevision
        val reply = try {
            client.updateMaintenance()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            if (revision != maintenanceRevision || mutableState.value.busy) return
            mutableState.update { it.copy(readFailure = MaintenanceReply.Refused("COMMUNICATION_FAILED", "read_maintenance")) }
            return
        }
        if (revision != maintenanceRevision || mutableState.value.busy) return
        val view = UpdateMaintenanceReplies.state(reply)
        mutableState.update { previous ->
            val completed = previous.maintenance?.record != null && view != null && view.record == null
            previous.copy(
                maintenance = view ?: previous.maintenance,
                readFailure = if (view == null) UpdateMaintenanceReplies.mutation(reply) as? MaintenanceReply.Refused else null,
                actionFailure = if (completed) null else previous.actionFailure,
            )
        }
        if (view != null) withContext(Dispatchers.IO) { updates.refreshDownloads() }
    }

    /** Poll only a visible installation; callbacks and package changes remain authoritative. */
    fun monitorInstallation(): Job = viewModelScope.launch {
        while (isActive) {
            delay(1_000)
            if (!mutableState.value.busy && mutableState.value.maintenance?.record?.phase == "apk_installing") {
                refreshJob?.join()
                refreshState()
            }
        }
    }

    fun check() { viewModelScope.launch { updates.check() } }
    fun download() { viewModelScope.launch { updates.download() } }

    fun installApk() = mutate("install_request") {
        val record = mutableState.value.maintenance?.record
        if (record != null) return@mutate UpdateMaintenanceReplies.mutation(client.installUpdateApk(record.updateId))
        val checked = updates.state.value.check as? UpdateCheck.Checked
            ?: return@mutate MaintenanceReply.Refused("INVALID_ARGUMENT", "verify_manifest")
        when (val begun = UpdateMaintenanceReplies.mutation(client.beginProductUpdate(checked.manifest, checked.signature))) {
            is MaintenanceReply.Refused -> begun
            is MaintenanceReply.Recorded -> begun.record?.let {
                UpdateMaintenanceReplies.mutation(client.installUpdateApk(it.updateId))
            } ?: MaintenanceReply.Refused("RESPONSE_INVALID", "response_decode")
        }
    }

    fun cancel(record: MaintenanceRecordView) = mutate("cancel_request") {
        UpdateMaintenanceReplies.mutation(client.cancelUpdate(record.updateId))
    }

    fun localFailure(code: String, stage: String) {
        mutableState.update { it.copy(actionFailure = MaintenanceReply.Refused(code, stage)) }
    }

    private fun mutate(stage: String, action: suspend () -> MaintenanceReply) {
        if (mutableState.value.busy) return
        maintenanceRevision++
        refreshJob?.cancel()
        mutableState.update { it.copy(busy = true, actionFailure = null) }
        viewModelScope.launch {
            val reply = try {
                action()
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                MaintenanceReply.Refused("COMMUNICATION_FAILED", stage)
            }
            mutableState.update { it.copy(busy = false, actionFailure = reply as? MaintenanceReply.Refused) }
            refresh()
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun UpdatesRoute(viewModel: UpdatesViewModel, apkVersion: String, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val updates by viewModel.updateState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    LifecycleResumeEffect(viewModel) {
        viewModel.refresh()
        val monitoring = viewModel.monitorInstallation()
        onPauseOrDispose { monitoring.cancel() }
    }
    val maintenance = state.maintenance
    val record = maintenance?.record
    val installApk = {
        try {
            if (context.packageManager.canRequestPackageInstalls()) {
                viewModel.installApk()
            } else {
                context.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + context.packageName)))
            }
        } catch (_: android.content.ActivityNotFoundException) {
            viewModel.localFailure("ACTIVITY_NOT_FOUND", "install_permission")
        } catch (_: SecurityException) {
            viewModel.localFailure("PERMISSION_DENIED", "install_permission")
        }
    }
    Scaffold(
        modifier = Modifier.testTag("route:Updates"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(AppR.string.updates_title)) },
                navigationIcon = { BackButton("route:Updates", AppR.string.updates_title, back) },
                actions = { if (state.busy || updates.downloading) RefreshIndicator("updates") },
            )
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item {
                ListItem(
                    headlineContent = { Text(stringResource(AppR.string.updates_current_version)) },
                    leadingContent = { RowIcon(R.drawable.ic_info) },
                    supportingContent = { Text(apkVersion) },
                    modifier = Modifier.testTag("updates:current_version"),
                )
            }
            item {
                Button(
                    onClick = viewModel::check,
                    enabled = maintenance != null && updates.check != UpdateCheck.Checking && updates.check != UpdateCheck.Unconfigured,
                    modifier = Modifier.fillMaxWidth().padding(16.dp).heightIn(min = 56.dp).testTag("updates:check"),
                ) { Text(stringResource(AppR.string.action_check_for_updates)) }
            }
            val failure = state.actionFailure ?: state.readFailure ?: record?.installFailure
            if (failure != null) {
                item { UpdateFailure(failure) }
            } else if (updates.downloadFailed) {
                item { RouteError("updates:action", retry = null) }
            }
            if (record != null) {
                maintenanceActions(record, state.busy, installApk, viewModel::cancel)
            } else {
                checkRegion(updates, state.busy, installApk, viewModel::download, viewModel::check)
            }
        }
    }
}

private fun LazyListScope.checkRegion(
    updates: UpdateState,
    busy: Boolean,
    installApk: () -> Unit,
    download: () -> Unit,
    retry: () -> Unit,
) {
    when (val check = updates.check) {
        UpdateCheck.Unconfigured -> item { Status(AppR.string.updates_configuration_unavailable, null, "configuration_unavailable", R.drawable.ic_status_unknown) }
        UpdateCheck.Idle -> Unit
        UpdateCheck.Checking -> item { RouteLoading("updates") }
        UpdateCheck.Failed -> item { RouteError("updates", retry) }
        is UpdateCheck.Checked -> when (val classification = check.classification) {
            ReleaseClassification.UpToDate -> item { Status(AppR.string.updates_up_to_date, null, "up_to_date", R.drawable.ic_status_success) }
            is ReleaseClassification.ProductUpdate -> {
                item { Status(AppR.string.updates_available, classification.manifest.version, "available", R.drawable.ic_system_update) }
                val apk = updates.downloads?.apk
                item {
                    if (apk == null) {
                        Action(AppR.string.action_download_update, "download_update", !updates.downloading, download)
                    } else {
                        Action(AppR.string.updates_install_apk, "install_apk", !busy, installApk)
                    }
                }
            }
        }
    }
}

private fun LazyListScope.maintenanceActions(
    record: MaintenanceRecordView,
    busy: Boolean,
    installApk: () -> Unit,
    cancel: (MaintenanceRecordView) -> Unit,
) {
    item { Status(AppR.string.updates_available, record.targetVersion, "maintenance", R.drawable.ic_system_update) }
    // An install under way is followed by Runtime recovery until the package is replaced.
    if (record.phase == "prepared") {
        item { Action(AppR.string.updates_install_apk, "install_apk", !busy, installApk) }
    } else {
        item { RouteLoading("updates:installing") }
        if (record.awaitingConfirmation && record.installFailure == null) {
            item {
                Text(stringResource(AppR.string.updates_awaiting_confirmation), Modifier.padding(16.dp).testTag("updates:awaiting_confirmation"))
            }
        }
    }
    if (record.phase == "prepared" || record.phase == "apk_installing") {
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

@Composable
private fun UpdateFailure(failure: MaintenanceReply.Refused) {
    val reason = when (failure.code) {
        "HOST_TRANSITION_PENDING" -> AppR.string.updates_error_busy
        "CAPABILITY_UNAVAILABLE", "RUNTIME_UNAVAILABLE" -> AppR.string.updates_error_runtime
        "PERMISSION_DENIED", "INSTALLER_BLOCKED" -> AppR.string.updates_error_permission
        "INSTALLER_ABORTED" -> AppR.string.updates_error_aborted
        "INSTALLER_STORAGE" -> AppR.string.updates_error_storage
        "INSTALLER_INVALID_APK", "INSTALLER_INCOMPATIBLE", "INSTALLER_CONFLICT" -> AppR.string.updates_error_apk
        "INVALID_ARGUMENT" -> if (failure.stage in setOf("verify_manifest", "verify_apk", "validate_install", "apk_write")) AppR.string.updates_error_apk else AppR.string.updates_error_generic
        "STALE_AUTHORITY" -> AppR.string.updates_error_stale
        "COMMUNICATION_FAILED" -> AppR.string.updates_error_connection
        "CONFIRMATION_MISSING", "ACTIVITY_NOT_FOUND", "CALLBACK_TIMEOUT" -> AppR.string.updates_error_confirmation
        "INSTALLER_RESULT_MISSING", "INSTALLER_STATUS_MISSING", "INSTALLER_STATUS_UNKNOWN" -> AppR.string.updates_error_result_missing
        else -> AppR.string.updates_error_generic
    }
    val stage = when (failure.stage) {
        "release_configuration", "verify_manifest", "verify_apk", "validate_install" -> AppR.string.updates_stage_verify
        "activate_apk_host", "close_admission", "enter_maintenance", "record_maintenance" -> AppR.string.updates_stage_prepare
        "create_session", "record_session", "apk_write", "apk_write_commit", "installer_commit" -> AppR.string.updates_stage_install
        "installer_confirmation", "install_permission" -> AppR.string.updates_stage_confirmation
        "installer_result" -> AppR.string.updates_stage_result
        "validate_cancel", "abandon_session", "delete_maintenance", "reopen_admission" -> AppR.string.updates_stage_cancel
        "read_maintenance", "recover_maintenance", "cleanup_downloads" -> AppR.string.updates_stage_refresh
        else -> AppR.string.updates_stage_request
    }
    ListItem(
        headlineContent = { Text(stringResource(reason)) },
        supportingContent = {
            Text(stringResource(AppR.string.updates_error_details, stringResource(stage) + (failure.stage?.let { " (" + it + ")" } ?: ""), failure.code))
        },
        leadingContent = { RowIcon(R.drawable.ic_info) },
        modifier = Modifier.testTag("updates:action_error"),
    )
}
