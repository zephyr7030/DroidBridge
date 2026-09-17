package com.droidbridge.android.ui.diagnostics

import android.content.ContentResolver
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
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
import com.droidbridge.android.product.diagnostics.DiagnosticsExport
import com.droidbridge.android.product.diagnostics.FaultFileRead
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.settings.BackButton
import java.io.File
import java.time.Instant
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject

/**
 * The default-process S-SEC-005 export path, shared by Diagnostics and MaintenanceRecovery. A
 * missing binding, a Service refusal or the deadline all make the live snapshot unavailable.
 */
class DiagnosticsExporter(
    private val client: DroidBridgeClient,
    private val canonicalBase: File,
    private val productVersions: JsonObject,
    private val releaseIdentifiers: JsonObject,
) {
    suspend fun liveSnapshot(): String? =
        withTimeoutOrNull(LIVE_DEADLINE_MILLIS) { runCatching { client.diagnosticsSnapshot() }.getOrNull() }

    suspend fun faultFiles(): Map<String, FaultFileRead> =
        withContext(Dispatchers.IO) { DiagnosticsExport.readFaultFiles(canonicalBase) }

    fun fileName(): String = DiagnosticsExport.fileName(Instant.now())

    /** Builds a fresh export and writes it to the user-selected document; false leaves no claimed export. */
    suspend fun write(uri: Uri, resolver: ContentResolver): Boolean {
        val encoded = runCatching {
            DiagnosticsExport.build(Instant.now(), productVersions, liveSnapshot(), faultFiles(), releaseIdentifiers)
        }.getOrNull() ?: return false
        return withContext(Dispatchers.IO) {
            runCatching {
                checkNotNull(resolver.openOutputStream(uri, "wt")).use { it.write(encoded.encodeToByteArray()) }
            }.isSuccess
        }
    }

    private companion object {
        /** The Service bounds its own read to 2000 ms; this covers the Binder round trip. */
        const val LIVE_DEADLINE_MILLIS = 2_500L
    }
}

data class DiagnosticsUiState(
    val live: JsonObject? = null,
    val liveLoaded: Boolean = false,
    val faults: Map<String, FaultFileRead>? = null,
    val exporting: Boolean = false,
    val exportFailed: Boolean = false,
)

class DiagnosticsViewModel(private val exporter: DiagnosticsExporter) : ViewModel() {
    private val mutableState = MutableStateFlow(DiagnosticsUiState())
    val state: StateFlow<DiagnosticsUiState> = mutableState.asStateFlow()

    fun load() {
        viewModelScope.launch {
            val faults = exporter.faultFiles()
            mutableState.update { it.copy(faults = faults) }
            val live = exporter.liveSnapshot()?.let { reply -> runCatching { Json.parseToJsonElement(reply).jsonObject }.getOrNull() }
            mutableState.update { it.copy(live = live, liveLoaded = true) }
        }
    }

    fun fileName(): String = exporter.fileName()

    fun export(uri: Uri?, resolver: ContentResolver) {
        if (uri == null) return
        mutableState.update { it.copy(exporting = true, exportFailed = false) }
        viewModelScope.launch {
            val written = exporter.write(uri, resolver)
            mutableState.update { it.copy(exporting = false, exportFailed = !written) }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun DiagnosticsRoute(viewModel: DiagnosticsViewModel, apkVersion: String, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val resolver = LocalContext.current.contentResolver
    val createDocument = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(JSON_MIME)) { uri ->
        viewModel.export(uri, resolver)
    }
    LaunchedEffect(Unit) { viewModel.load() }
    Scaffold(
        modifier = Modifier.testTag("route:Diagnostics"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.diagnostics_title)) },
                navigationIcon = { BackButton("route:Diagnostics", R.string.diagnostics_title, back) },
                actions = { if (!state.liveLoaded || state.exporting) RefreshIndicator("diagnostics") },
            )
        },
    ) { padding ->
        val status = state.live?.get("status") as? JsonObject
        val session = state.live?.get("session") as? JsonObject
        val runtimeHost = status.child("components")?.child("runtime_host")
        val magisk = status.child("components")?.child("magisk")
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            item { Fact(R.string.diag_apk_version, apkVersion, "apk_version") }
            item { Fact(R.string.diag_runtime_host, runtimeHost.text("host") ?: session.text("host"), "runtime_host") }
            item { Fact(R.string.diag_component_version, runtimeHost.text("component_version"), "component_version") }
            magisk.text("daemon_version")?.let { item { Fact(R.string.diag_daemon_version, it, "daemon_version") } }
            magisk.text("module_version")?.let { item { Fact(R.string.diag_module_version, it, "module_version") } }
            item { Fact(R.string.mcp_protocol_version, runtimeHost.text("protocol_version"), "protocol") }
            item { Fact(R.string.diag_store_schema, runtimeHost.text("store_schema_version"), "store_schema") }
            item {
                val runtime = status.child("runtime")
                val readiness = runtime.text("readiness")?.let { value -> runtime.text("reason")?.let { "$value $it" } ?: value }
                Fact(R.string.diag_runtime_readiness, readiness ?: session.text("start_failure"), "runtime_readiness")
            }
            item {
                val compatibility = status.child("compatibility")
                val value = compatibility?.entries?.joinToString { (key, fact) -> "$key=${(fact as? JsonPrimitive)?.contentOrNull}" }
                Fact(R.string.diag_ipc_compatibility, value, "ipc_compatibility")
            }
            item {
                val facts = listOfNotNull(status.child("grants"), status.child("capabilities"))
                    .flatMap { it.entries }
                    .joinToString("\n") { (key, fact) ->
                        val entry = fact as? JsonObject
                        listOfNotNull(key, entry.text("state"), entry.text("reason")).joinToString(" ")
                    }
                Fact(R.string.diag_capability_facts, facts.ifEmpty { null }, "capability_facts")
            }
            item {
                ListItem(headlineContent = { Text(stringResource(R.string.diag_recent_failures)) }, modifier = Modifier.testTag("diagnostics:recent_failures"))
            }
            val failures = state.faults.orEmpty().values
                .flatMap { read -> read.records.orEmpty().mapNotNull { it as? JsonObject } }
                .sortedByDescending { it.text("at") }
            if (state.faults != null && failures.isEmpty()) {
                item {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.diagnostics_no_failures)) },
                        leadingContent = { Icon(painterResource(R.drawable.ic_status_unknown), contentDescription = null) },
                        modifier = Modifier.testTag("diagnostics:empty"),
                    )
                }
            }
            items(failures) { record ->
                ListItem(
                    headlineContent = { Text(listOfNotNull(record.text("component"), record.text("code")).joinToString(" ")) },
                    supportingContent = {
                        Text(listOfNotNull(record.text("phase"), record.text("at"), record.text("repeat_count")?.let { "x$it" }).joinToString(" "))
                    },
                    modifier = Modifier.testTag("diagnostics:failure:${record.text("record_id")}"),
                )
            }
            if (state.exportFailed) item { RouteError("diagnostics:export", retry = null) }
            item {
                ListItem(
                    headlineContent = { Text(stringResource(R.string.action_export_diagnostics)) },
                    leadingContent = { Icon(painterResource(R.drawable.ic_download), contentDescription = null) },
                    modifier = Modifier
                        .clickable(enabled = !state.exporting) { createDocument.launch(viewModel.fileName()) }
                        .testTag("diagnostics:export"),
                )
            }
        }
    }
}

/** A technical fact row; an absent live value shows `state_unavailable` rather than a guess. */
@Composable
private fun Fact(@StringRes title: Int, value: String?, tag: String) {
    ListItem(
        headlineContent = { Text(stringResource(title)) },
        supportingContent = { Text(value ?: stringResource(R.string.state_unavailable)) },
        modifier = Modifier.testTag("diagnostics:$tag"),
    )
}

private fun JsonObject?.child(key: String): JsonObject? = this?.get(key) as? JsonObject

private fun JsonObject?.text(key: String): String? = (this?.get(key) as? JsonPrimitive)?.contentOrNull

internal const val JSON_MIME = "application/json"
