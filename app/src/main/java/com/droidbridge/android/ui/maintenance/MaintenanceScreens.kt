package com.droidbridge.android.ui.maintenance

import android.content.ContentResolver
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.client.DroidBridgeClient
import com.droidbridge.android.product.maintenance.MaintenanceBlocker
import com.droidbridge.android.product.maintenance.MaintenanceReplies
import com.droidbridge.android.product.maintenance.MaintenanceState
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.common.RouteLoading
import com.droidbridge.android.ui.diagnostics.DiagnosticsExporter
import com.droidbridge.android.ui.diagnostics.JSON_MIME
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class MaintenanceUiState(
    val maintenance: MaintenanceState? = null,
    val loadFailed: Boolean = false,
    val confirming: Boolean = false,
    val working: Boolean = false,
    val failed: Boolean = false,
    val recovered: Boolean = false,
)

class MaintenanceViewModel(
    private val client: DroidBridgeClient,
    private val exporter: DiagnosticsExporter,
) : ViewModel() {
    private val mutableState = MutableStateFlow(MaintenanceUiState())
    val state: StateFlow<MaintenanceUiState> = mutableState.asStateFlow()

    fun load() {
        viewModelScope.launch {
            val reply = runCatching { client.maintenanceState() }.getOrNull()?.let(MaintenanceReplies::state)
            mutableState.update { it.copy(maintenance = reply ?: it.maintenance, loadFailed = reply == null && it.maintenance == null) }
        }
    }

    fun fileName(): String = exporter.fileName()

    fun export(uri: Uri?, resolver: ContentResolver) {
        if (uri == null) return
        mutableState.update { it.copy(working = true, failed = false) }
        viewModelScope.launch {
            val written = exporter.write(uri, resolver)
            mutableState.update { it.copy(working = false, failed = !written) }
        }
    }

    fun requestReset() = mutableState.update { it.copy(confirming = true, failed = false) }

    fun dismissReset() = mutableState.update { it.copy(confirming = false) }

    /** Invokes only the owning recovery contract for the observed blocker. */
    fun confirmReset() {
        val blocker = mutableState.value.maintenance?.takeIf(MaintenanceState::resetAvailable)?.blocker ?: return
        mutableState.update { it.copy(confirming = false, working = true, failed = false) }
        viewModelScope.launch {
            val reply = runCatching {
                if (blocker == MaintenanceBlocker.OwnerCorrupt) client.resetRuntimeHostToApk() else client.resetRuntimeData()
            }.getOrNull()
            val reset = reply?.let(MaintenanceReplies::resetSucceeded) == true
            mutableState.update { it.copy(working = false, failed = !reset, recovered = reset) }
            if (!reset) load()
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun MaintenanceRecoveryRoute(viewModel: MaintenanceViewModel, onRecovered: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val resolver = LocalContext.current.contentResolver
    val createDocument = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(JSON_MIME)) { uri ->
        viewModel.export(uri, resolver)
    }
    LaunchedEffect(Unit) { viewModel.load() }
    LaunchedEffect(state.recovered) { if (state.recovered) onRecovered() }
    Scaffold(
        modifier = Modifier.testTag("route:MaintenanceRecovery"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.maintenance_recovery_title)) },
                actions = { if (state.working) RefreshIndicator("maintenance") },
            )
        },
    ) { padding ->
        val maintenance = state.maintenance
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                maintenance == null && state.loadFailed -> RouteError("maintenance", viewModel::load)
                maintenance == null -> RouteLoading("maintenance")
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    item {
                        ListItem(
                            headlineContent = { Text(stringResource(reason(maintenance))) },
                            leadingContent = { Icon(painterResource(R.drawable.ic_status_error), contentDescription = null) },
                            modifier = Modifier.testTag("maintenance:reason"),
                        )
                    }
                    item {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.action_export_diagnostics)) },
                            leadingContent = { Icon(painterResource(R.drawable.ic_download), contentDescription = null) },
                            modifier = Modifier
                                .clickable(enabled = !state.working) { createDocument.launch(viewModel.fileName()) }
                                .testTag("maintenance:export"),
                        )
                    }
                    if (maintenance.resetAvailable) {
                        item {
                            ListItem(
                                headlineContent = { Text(stringResource(resetAction(maintenance.blocker))) },
                                modifier = Modifier
                                    .clickable(enabled = !state.working, onClick = viewModel::requestReset)
                                    .testTag("maintenance:reset"),
                            )
                        }
                    }
                    if (state.failed) item { RouteError("maintenance:action", retry = null) }
                }
            }
        }
    }
    val maintenance = state.maintenance
    if (state.confirming && maintenance != null) {
        val owner = maintenance.blocker == MaintenanceBlocker.OwnerCorrupt
        AlertDialog(
            onDismissRequest = viewModel::dismissReset,
            title = { Text(stringResource(if (owner) R.string.dialog_reset_host_title else R.string.dialog_reset_runtime_title)) },
            text = { Text(stringResource(if (owner) R.string.dialog_reset_host_body else R.string.dialog_reset_runtime_body)) },
            confirmButton = {
                TextButton(onClick = viewModel::confirmReset, modifier = Modifier.testTag("maintenance:reset:confirm")) {
                    Text(stringResource(R.string.action_confirm))
                }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismissReset, modifier = Modifier.testTag("maintenance:reset:cancel")) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

/** S-UI-017 MaintenanceRecovery reason: cleanup uncertainty first, then the blocker's own reason. */
@StringRes
internal fun reason(maintenance: MaintenanceState): Int = when {
    !maintenance.cleanupVerified -> R.string.reason_cleanup_unverified
    maintenance.blocker == MaintenanceBlocker.StoreCorrupt -> R.string.reason_store_unavailable
    else -> R.string.reason_runtime_unavailable
}

@StringRes
internal fun resetAction(blocker: MaintenanceBlocker): Int =
    if (blocker == MaintenanceBlocker.OwnerCorrupt) R.string.action_reset_runtime_host_to_apk else R.string.data_reset_runtime_data
